//! Subprocess transport for provider plugins: ndjson JSON-RPC over the
//! child's stdin/stdout.
//!
//! Wire format — one JSON object per line:
//!
//! ```text
//! host → plugin:  {"id":1,"method":"ping","params":{...}}
//! plugin → host:  {"id":1,"result":{...}} | {"id":1,"error":{"message":"..."}}
//! ```
//!
//! Transport-only by design: spawning, request/response correlation
//! (out-of-order responses are matched by `id`), and graceful shutdown live
//! here; [`HostedPlugin`] implements the
//! [`ProviderPlugin`](super::ProviderPlugin) surface on top of
//! [`PluginTransport`]. Response lines without an `id`
//! (notifications/events) are read and ignored for now.
//!
//! What runs behind the pipe is `plugin-host.js` (embedded via
//! `include_str!` and materialized by [`default_launch`]): a Node ESM host
//! that `import()`s real OpenCode plugin modules, injects the SDK-shaped
//! `client`, and answers these RPCs. Auth credentials are written by that
//! host into the very auth file [`PluginAuthStore`] reads, so host and plugin
//! share one ledger instead of keeping parallel records.
//!
//! Runtime constraint: the workspace tokio is built without the `process`
//! and `io` features, so the child is driven with `std::process` plus a
//! dedicated reader thread; the only tokio surface is `tokio::sync`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::methods;
use super::{
    ApiKeyParams, ApiKeyResult, AuthResult, Authorization, AuthorizeParams, CallbackParams,
    CallbackResult, LoadParams, LoaderOptions, ModelsParams, PluginAuthStore, PluginError,
    PluginFuture, PluginModel, PluginProviderInfo, ProviderPlugin, SignOutParams, UsageParams,
};

/// Error message used whenever the plugin process is gone.
const GONE_MSG: &str = "plugin process is gone";

/// JS host source, embedded so packaged builds need no data files.
const PLUGIN_HOST_JS: &str = include_str!("plugin-host.js");

/// argv flag carrying the serialized [`PluginSpec`] list to `plugin-host.js`.
const PLUGIN_ARG: &str = "--plugins";

/// Environment override for the node executable used by [`default_launch`].
const NODE_ENV: &str = "ANY_CONVERTER_NODE";

/// Waiters for in-flight requests, keyed by request id.
type PendingReplies = HashMap<u64, oneshot::Sender<Result<Value, String>>>;

/// How to launch a plugin subprocess.
#[derive(Debug, Clone)]
pub struct PluginLaunch {
    /// Executable to run (resolved via `PATH`).
    pub program: String,
    /// Command-line arguments passed to the program.
    pub args: Vec<String>,
    /// Extra environment variables set for the child.
    pub env: Vec<(String, String)>,
}

/// One plugin the JS host should load: magpie's `{spec, target}` pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSpec {
    /// Package name, absolute path, or path relative to `target`.
    pub spec: String,
    /// Directory the spec resolves from; empty means the host's cwd.
    #[serde(default)]
    pub target: String,
}

impl PluginSpec {
    /// A plugin resolved from `target` (empty string = the host's cwd).
    pub fn new(spec: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            spec: spec.into(),
            target: target.into(),
        }
    }
}

/// Launch [`PLUGIN_HOST_JS`] on node, loading `plugins` at `init` time.
///
/// The host script is materialized once per process as a `0600` `.mjs` file
/// under the temp dir (`.mjs` because the temp dir has no `package.json`
/// marking it as ESM). The plugin list rides along in argv as
/// `--plugins <json>` so [`HostedPlugin::start`] can forward it verbatim in
/// the `init` handshake. `ANY_CONVERTER_NODE` overrides the `node` binary.
pub fn default_launch(plugins: Vec<PluginSpec>) -> PluginLaunch {
    let mut args = vec![plugin_host_path().to_string_lossy().into_owned()];
    if !plugins.is_empty() {
        match serde_json::to_string(&plugins) {
            Ok(json) => {
                args.push(PLUGIN_ARG.to_string());
                args.push(json);
            }
            Err(e) => log::warn!("failed to encode plugin specs for the host argv: {e}"),
        }
    }
    PluginLaunch {
        program: std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_string()),
        args,
        env: vec![],
    }
}

/// Plugins baked into `args` by [`default_launch`] (empty for other launches).
fn plugins_from_args(args: &[String]) -> Vec<PluginSpec> {
    args.iter()
        .position(|arg| arg == PLUGIN_ARG)
        .and_then(|at| args.get(at + 1))
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

/// On-disk path of the embedded JS host (written once per process).
fn plugin_host_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| match write_plugin_host() {
        Ok(path) => path,
        Err(e) => {
            // Dev fallback: the source tree copy. Node may parse it as CJS
            // if an enclosing package.json says so, hence the warning.
            log::warn!("failed to materialize plugin-host.js ({e}); using the source tree");
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/plugin-host.js")
        }
    })
    .clone()
}

/// Write [`PLUGIN_HOST_JS`] to `<temp>/any-converter-plugin-host-<pid>/`.
fn write_plugin_host() -> std::io::Result<PathBuf> {
    let dir =
        std::env::temp_dir().join(format!("any-converter-plugin-host-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join("plugin-host.mjs");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(PLUGIN_HOST_JS.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// Live ndjson JSON-RPC connection to a plugin subprocess.
///
/// Responses are matched to requests by `id`, so a plugin may reply in any
/// order. Callers that need a deadline should wrap [`PluginTransport::call`]
/// in `tokio::time::timeout`; the transport itself waits indefinitely.
#[derive(Debug)]
pub struct PluginTransport {
    next_id: AtomicU64,
    /// Write end of the child's stdin; taken (dropped) on shutdown.
    stdin: Mutex<Option<ChildStdin>>,
    /// Waiters for unanswered requests, shared with the reader thread.
    pending: Arc<Mutex<PendingReplies>>,
    /// The child itself; `None` once reaped.
    child: Mutex<Option<Child>>,
    /// Reader thread draining the child's stdout; `None` once joined.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Set once the reader hit EOF (or shutdown ran): the process is gone.
    gone: Arc<AtomicBool>,
    /// OS process id of the child.
    pid: u32,
}

/// A dropped transport takes its child with it.
///
/// Without this an abandoned host — a racing duplicate from a concurrent
/// start, or a manager shutting down — left its Node process running until
/// the machine was rebooted. `Drop` cannot await, so this is the blunt
/// version: kill the child and let it be reaped by the OS; `shutdown()`
/// remains the graceful path.
impl Drop for PluginTransport {
    fn drop(&mut self) {
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = child {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl PluginTransport {
    /// Spawn the plugin described by `launch` and start reading its stdout.
    ///
    /// The child gets piped stdin/stdout and inherited stderr. Fails with
    /// [`PluginError::Rpc`] if the process cannot be spawned or stdio cannot
    /// be wired up.
    pub fn spawn(launch: PluginLaunch) -> Result<Self, PluginError> {
        let mut child = Command::new(&launch.program)
            .args(&launch.args)
            .envs(launch.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                PluginError::Rpc(format!("failed to spawn plugin `{}`: {e}", launch.program))
            })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (stdin, stdout) = match (stdin, stdout) {
            (Some(stdin), Some(stdout)) => (stdin, stdout),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PluginError::Rpc("plugin stdio was not piped".into()));
            }
        };

        let pending: Arc<Mutex<PendingReplies>> = Arc::new(Mutex::new(HashMap::new()));
        let gone = Arc::new(AtomicBool::new(false));
        let pid = child.id();
        let reader = std::thread::Builder::new()
            .name(format!("plugin-reader-{pid}"))
            .spawn({
                let pending = Arc::clone(&pending);
                let gone = Arc::clone(&gone);
                move || read_responses(stdout, &pending, &gone)
            })
            .map_err(|e| {
                let _ = child.kill();
                let _ = child.wait();
                PluginError::Rpc(format!("failed to spawn reader thread: {e}"))
            })?;

        Ok(Self {
            next_id: AtomicU64::new(0),
            stdin: Mutex::new(Some(stdin)),
            pending,
            child: Mutex::new(Some(child)),
            reader: Mutex::new(Some(reader)),
            gone,
            pid,
        })
    }

    /// The OS process id of the plugin subprocess.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Send a JSON-RPC request and await its matching response.
    ///
    /// The id is auto-incremented; responses may arrive in any order. The
    /// reply future resolves early with [`PluginError::Protocol`] if the
    /// plugin process dies while we are waiting. This method has no built-in
    /// timeout — wrap it with `tokio::time::timeout` if you need one.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        if self.gone.load(Ordering::Acquire) {
            return Err(PluginError::Protocol(GONE_MSG.into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = serde_json::json!({"id": id, "method": method, "params": params});
        let line = serde_json::to_string(&payload)
            .map_err(|e| PluginError::Rpc(format!("failed to encode request: {e}")))?;

        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);

        // Write one ndjson line; the guard is dropped at the block end so it
        // is never held across the await below.
        let written = {
            let mut guard = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_mut() {
                Some(stdin) => writeln!(stdin, "{line}")
                    .and_then(|()| stdin.flush())
                    .map_err(|e| {
                        PluginError::Protocol(format!("{GONE_MSG} (stdin write failed: {e})"))
                    }),
                None => Err(PluginError::Protocol(GONE_MSG.into())),
            }
        };
        if let Err(e) = written {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }

        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(PluginError::Rpc(message)),
            // Sender dropped without answering: EOF or shutdown drained it.
            Err(_recv) => Err(PluginError::Protocol(GONE_MSG.into())),
        }
    }

    /// Shut the plugin down, gracefully if possible.
    ///
    /// Closes the child's stdin (well-behaved plugins exit on EOF), polls
    /// `try_wait` for up to ~5s (50 × 100ms), then falls back to `kill`.
    /// Afterwards the child is reaped and the reader thread joined. Safe to
    /// call more than once.
    pub fn try_shutdown(&self) -> Result<(), PluginError> {
        // Close the write end so the plugin sees stdin EOF.
        drop(self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());

        // Give the child up to ~5s to exit on its own.
        let mut exited = false;
        for _ in 0..50 {
            if self.child_exited()? {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = guard.as_mut() {
                if !exited {
                    child.kill().map_err(|e| {
                        PluginError::Rpc(format!("failed to kill plugin (pid {}): {e}", self.pid))
                    })?;
                }
                let _ = child.wait(); // reap; the pipes close and the reader hits EOF
            }
            *guard = None;
        }

        // The reader thread exits once stdout reaches EOF (child gone).
        if let Some(handle) = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }

        self.gone.store(true, Ordering::Release);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }

    /// Non-blocking check whether the child has exited. `Ok(true)` when it is
    /// gone (including already reaped).
    fn child_exited(&self) -> Result<bool, PluginError> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            None => Ok(true),
            Some(child) => child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|e| PluginError::Rpc(format!("failed to wait for plugin: {e}"))),
        }
    }
}

/// `fetch` request the host asks the plugin to perform on its behalf.
///
/// The plugin runs the HTTP request against the upstream provider (its own
/// web API or auth endpoints) where the host's HTTP stack must not be
/// involved, and returns the raw status/headers/body. `provider`/`account`/
/// `model`/`npm` mirror magpie's `plugin.FetchRequest`: plugins that sign
/// requests need the identity, and per-package routing needs the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginFetchRequest {
    /// Full request URL.
    pub url: String,
    /// HTTP method (upper case).
    pub method: String,
    /// Request headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Raw request body, already a string; empty when there is none.
    pub body: String,
    /// Provider id the request belongs to (defaults to the plugin's own).
    #[serde(default)]
    pub provider: String,
    /// Account name the request belongs to.
    #[serde(default)]
    pub account: String,
    /// Model id the request is for, when the caller knows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// AI SDK package of the model, when the caller knows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub npm: String,
}

/// The [`ProviderPlugin::fetch`] parameter (alias; same shape as
/// [`PluginFetchRequest`]).
pub type PluginFetchParams = PluginFetchRequest;

/// `fetch` response returned by the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFetchResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// Response body text.
    pub body: String,
}

/// Context the host keeps for an in-flight authorization flow.
///
/// `authorize` swaps the plugin's own session id for a host-generated one
/// (`s-<n>`) before returning the [`Authorization`]; the plugin-side session
/// id — plus the provider/account the flow was started for — stays here so
/// `callback` can map the host session back to the plugin.
#[derive(Debug, Clone)]
pub struct SessionCtx {
    /// The plugin's own flow handle, passed back in the `callback` RPC.
    pub plugin_session: String,
    /// Provider id from the original `authorize` request.
    pub provider: String,
    /// Account name from the original `authorize` request.
    pub account: String,
}

/// A provider plugin running as a subprocess: [`PluginTransport`] plus the
/// [`ProviderPlugin`] RPC surface (`init` handshake, auth flows, `fetch`
/// forwarding).
pub struct HostedPlugin {
    info: PluginProviderInfo,
    transport: Arc<PluginTransport>,
    auth: Arc<PluginAuthStore>,
    /// Open authorization flows keyed by host session id. `Arc`-wrapped so
    /// the `'static` futures returned by [`ProviderPlugin`] can own a shared
    /// handle.
    sessions: Arc<Mutex<HashMap<String, SessionCtx>>>,
    /// Monotonic counter for host-generated session ids (`s-<n>`).
    next_session: AtomicU64,
}

impl HostedPlugin {
    /// The child's process id (test and diagnostics use).
    pub fn pid(&self) -> u32 {
        self.transport.pid()
    }

    /// Spawn the plugin subprocess and perform the `init` handshake.
    ///
    /// `init` carries the host auth store path (the plugin persists real
    /// credentials there itself, so both sides share one auth file) plus the
    /// plugin list taken from a [`default_launch`] argv. It must answer within
    /// 60 seconds with a `plugins` array; the array must be non-empty (the JS
    /// host reports failed loads as `{spec, error}` entries). When the reply
    /// also carries a `providers` array (the JS host always does), the
    /// plugin's own metadata replaces the caller-supplied `info`.
    pub async fn start(
        launch: PluginLaunch,
        info: PluginProviderInfo,
        auth: Arc<PluginAuthStore>,
    ) -> Result<Self, PluginError> {
        let plugins = plugins_from_args(&launch.args);
        let transport = Arc::new(PluginTransport::spawn(launch)?);
        let auth_path = auth.path().to_string_lossy().into_owned();
        let handshake = timeout(
            Duration::from_secs(60),
            transport.call("init", json!({ "authPath": auth_path, "plugins": plugins })),
        )
        .await;
        let value = match handshake {
            Err(_elapsed) => {
                let _ = transport.try_shutdown();
                return Err(PluginError::Protocol(
                    "init handshake timed out after 60s".into(),
                ));
            }
            Ok(Err(e)) => {
                let _ = transport.try_shutdown();
                return Err(e);
            }
            Ok(Ok(value))
                if value
                    .get("plugins")
                    .and_then(Value::as_array)
                    .is_some_and(|plugins| !plugins.is_empty()) =>
            {
                value
            }
            Ok(Ok(_)) => {
                let _ = transport.try_shutdown();
                return Err(PluginError::Protocol(
                    "init response must include a non-empty `plugins` array".into(),
                ));
            }
        };
        let info = init_provider_info(&value, &info);
        Ok(Self {
            info,
            transport,
            auth,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_session: AtomicU64::new(0),
        })
    }

    /// Call one RPC and decode the reply, mapping decode failures to
    /// [`PluginError::Protocol`].
    async fn rpc<T: serde::de::DeserializeOwned>(
        transport: &Arc<PluginTransport>,
        method: &str,
        params: Value,
    ) -> Result<T, PluginError> {
        let value = transport.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|e| PluginError::Protocol(format!("invalid `{method}` response: {e}")))
    }
}

/// Provider metadata from the init reply's `providers` array, if any.
///
/// The JS host reports the plugin's real `{id, name, methods}` metadata. The
/// entry matching `fallback.id` wins when present, otherwise the first valid
/// entry. When the reply carries no (parsable) providers — non-JS plugin
/// processes — the caller-supplied `info` is kept so those handshakes keep
/// working.
fn init_provider_info(value: &Value, fallback: &PluginProviderInfo) -> PluginProviderInfo {
    let Some(entries) = value.get("providers").and_then(Value::as_array) else {
        return fallback.clone();
    };
    let chosen = entries
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(fallback.id.as_str()))
        .or_else(|| entries.iter().find(|entry| entry.is_object()));
    let Some(entry) = chosen else {
        return fallback.clone();
    };
    match serde_json::from_value::<PluginProviderInfo>(entry.clone()) {
        Ok(info) => info,
        Err(e) => {
            log::warn!("unparsable provider metadata in init reply: {e}");
            fallback.clone()
        }
    }
}

impl ProviderPlugin for HostedPlugin {
    fn info(&self) -> &PluginProviderInfo {
        &self.info
    }

    fn authorize(&self, params: AuthorizeParams) -> PluginFuture<Authorization> {
        let transport = Arc::clone(&self.transport);
        let sessions = Arc::clone(&self.sessions);
        // The host generates the session id the caller sees; the plugin's
        // own id is parked in the session map for the callback RPC.
        let host_session = format!(
            "s-{}",
            self.next_session.fetch_add(1, Ordering::Relaxed) + 1
        );
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode authorize params: {e}")))?;
            let mut authorization =
                Self::rpc::<Authorization>(&transport, methods::AUTHORIZE, request).await?;
            let plugin_session = std::mem::take(&mut authorization.session);
            sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(
                host_session.clone(),
                SessionCtx {
                    plugin_session,
                    provider: params.provider,
                    account: params.account,
                },
            );
            authorization.session = host_session;
            Ok(authorization)
        })
    }

    fn callback(&self, params: CallbackParams) -> PluginFuture<AuthResult> {
        let transport = Arc::clone(&self.transport);
        let sessions = Arc::clone(&self.sessions);
        Box::pin(async move {
            let ctx = {
                let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
                guard.remove(&params.session)
            }
            .ok_or_else(|| {
                PluginError::Protocol(format!("unknown session `{}`", params.session))
            })?;
            let mut result = Self::rpc::<CallbackResult>(
                &transport,
                methods::CALLBACK,
                json!({ "session": ctx.plugin_session, "code": params.code }),
            )
            .await?;
            if !result.ok {
                return Err(PluginError::Rpc(
                    result
                        .error
                        .unwrap_or_else(|| "callback failed".to_string()),
                ));
            }
            // One shared auth file: the plugin already persisted the real
            // credentials through the JS host's `client.auth.set` (it knows
            // the path from `init`). Writing anything here would clobber
            // them, so the host only reports the identity the flow ran for.
            if result.provider.is_empty() {
                result.provider = ctx.provider;
            }
            if result.account.is_empty() {
                result.account = ctx.account;
            }
            Ok(result)
        })
    }

    fn api_key(&self, params: ApiKeyParams) -> PluginFuture<AuthResult> {
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode apiKey params: {e}")))?;
            let result = Self::rpc::<ApiKeyResult>(&transport, methods::API_KEY, request).await?;
            if !result.ok {
                return Err(PluginError::Rpc(
                    result.error.unwrap_or_else(|| "apiKey failed".to_string()),
                ));
            }
            // Same contract as `callback`: the plugin already persisted
            // `{type: "api", key, metadata}` through `client.auth.set`, so a
            // write here would drop the metadata.
            Ok(result)
        })
    }

    fn sign_out(&self, params: SignOutParams) -> PluginFuture<()> {
        let transport = Arc::clone(&self.transport);
        let auth = Arc::clone(&self.auth);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode signOut params: {e}")))?;
            transport.call(methods::SIGN_OUT, request).await?;
            if params.account.is_empty() {
                // The plugin signed the provider out as a whole: drop every
                // entry that belongs to it.
                let keys = auth
                    .accounts_of(&params.provider)
                    .map_err(|e| PluginError::Storage(e.to_string()))?;
                for key in keys {
                    auth.remove(&key)
                        .map_err(|e| PluginError::Storage(e.to_string()))?;
                }
            } else {
                auth.remove(&format!("{}:{}", params.provider, params.account))
                    .map_err(|e| PluginError::Storage(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn usage(&self, params: UsageParams) -> PluginFuture<Option<Value>> {
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode usage params: {e}")))?;
            let value = transport.call(methods::USAGE, request).await?;
            // `null` means the plugin has no usage hook: report "unavailable"
            // instead of leaking a JSON null to the caller.
            Ok((!value.is_null()).then_some(value))
        })
    }

    fn load(&self, params: LoadParams) -> PluginFuture<LoaderOptions> {
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode load params: {e}")))?;
            Self::rpc::<LoaderOptions>(&transport, methods::LOAD, request).await
        })
    }

    fn models(&self, params: ModelsParams) -> PluginFuture<Vec<PluginModel>> {
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode models params: {e}")))?;
            Self::rpc::<Vec<PluginModel>>(&transport, methods::MODELS, request).await
        })
    }

    fn fetch(&self, params: PluginFetchParams) -> PluginFuture<PluginFetchResponse> {
        let transport = Arc::clone(&self.transport);
        // The provider defaults to the plugin's own id when the caller does
        // not say: the host always knows which plugin it is talking to.
        let provider = if params.provider.is_empty() {
            self.info.id.clone()
        } else {
            params.provider.clone()
        };
        Box::pin(async move {
            let headers: Map<String, Value> = params
                .headers
                .into_iter()
                .map(|(name, value)| (name, Value::String(value)))
                .collect();
            let mut request = json!({
                "url": params.url,
                "method": params.method,
                "headers": headers,
                "body": params.body,
                "provider": provider,
                "account": params.account,
            });
            if !params.model.is_empty() {
                request["model"] = Value::String(params.model);
            }
            if !params.npm.is_empty() {
                request["npm"] = Value::String(params.npm);
            }
            Self::rpc::<PluginFetchResponse>(&transport, methods::FETCH, request).await
        })
    }
}

/// Reader-thread body: parse each stdout line as JSON and hand responses to
/// the waiting caller by id. Lines without an `id` (events) are ignored —
/// they become interesting in a later task. On EOF the process is marked
/// gone and any remaining waiters are released with an error.
fn read_responses(
    stdout: ChildStdout,
    pending: &Arc<Mutex<PendingReplies>>,
    gone: &Arc<AtomicBool>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue; // malformed line: skip, the protocol stays in sync per-line
        };
        dispatch_response(value, pending);
    }
    gone.store(true, Ordering::Release);
    // Dropping the senders makes every waiter's `rx.await` fail fast.
    pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Route one parsed response line to its waiter, if any.
fn dispatch_response(value: Value, pending: &Arc<Mutex<PendingReplies>>) {
    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return; // notification/event: no waiter, ignored for now
    };
    let sender = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    let Some(sender) = sender else {
        return; // unknown or abandoned id
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        let _ = sender.send(Err(message));
    } else {
        let _ = sender.send(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
    }
}

/// Tests spawn `python3 -c <script>` as a fake plugin speaking the ndjson
/// JSON-RPC protocol. When `python3` is unavailable the subprocess tests
/// skip (print a note and return) — CI environments must provide python3,
/// or these tests should later move to a self-contained fake binary.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::Duration;

    use serde_json::json;
    use tokio::time::timeout;

    use super::*;
    // Only the tests build/read concrete credential entries.
    use super::super::{AuthMethod, AuthMethodKind, PluginAuth};

    /// Echoes every request back: `result` = params, or `error` for "fail".
    const ECHO_SCRIPT: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "fail":
        resp = {"id": req["id"], "error": {"message": "boom"}}
    else:
        resp = {"id": req["id"], "result": req.get("params")}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    /// Reads exactly two requests, then replies in reverse order.
    const REVERSE_SCRIPT: &str = r#"
import sys, json
reqs = []
for line in sys.stdin:
    if line.strip():
        reqs.append(json.loads(line))
    if len(reqs) == 2:
        break
for req in reversed(reqs):
    sys.stdout.write(json.dumps({"id": req["id"], "result": req.get("params")}) + "\n")
sys.stdout.flush()
"#;

    /// Reads stdin until EOF, then exits 0.
    const QUIET_SCRIPT: &str = r#"
import sys
for line in sys.stdin:
    pass
"#;

    fn python3_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            std::process::Command::new("python3")
                .arg("-c")
                .arg("pass")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }

    fn python_launch(script: &str) -> PluginLaunch {
        PluginLaunch {
            program: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
        }
    }

    #[test]
    fn spawn_missing_program_reports_error() {
        let launch = PluginLaunch {
            program: "/nonexistent/definitely-missing-binary".into(),
            args: vec![],
            env: vec![],
        };
        let err = PluginTransport::spawn(launch).unwrap_err();
        assert!(matches!(err, PluginError::Rpc(_)));
    }

    #[tokio::test]
    async fn call_echoes_result() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(ECHO_SCRIPT)).unwrap();
        let result = timeout(
            Duration::from_secs(5),
            transport.call("ping", json!({"k": "v", "n": 7})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, json!({"k": "v", "n": 7}));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn error_response_maps_to_rpc_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(ECHO_SCRIPT)).unwrap();
        let err = timeout(Duration::from_secs(5), transport.call("fail", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m == "boom"));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn out_of_order_responses_are_matched_by_id() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        // The fake replies to the second request first; each concurrent call
        // must still receive its own params back.
        let transport = PluginTransport::spawn(python_launch(REVERSE_SCRIPT)).unwrap();
        let (first, second) = tokio::join!(
            timeout(Duration::from_secs(5), transport.call("m", json!({"n": 1})),),
            timeout(Duration::from_secs(5), transport.call("m", json!({"n": 2})),),
        );
        assert_eq!(first.unwrap().unwrap(), json!({"n": 1}));
        assert_eq!(second.unwrap().unwrap(), json!({"n": 2}));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_graceful_and_marks_gone() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(QUIET_SCRIPT)).unwrap();
        let _ = transport.pid(); // pid is exposed for logging/diagnostics
        transport.try_shutdown().unwrap();
        transport.try_shutdown().unwrap(); // idempotent

        // After shutdown the transport refuses new work.
        let err = timeout(Duration::from_secs(5), transport.call("ping", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Protocol(ref m) if m.contains("gone")));
    }

    #[tokio::test]
    async fn call_after_process_death_is_protocol_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        // "pass": the child exits immediately without reading stdin.
        let transport = PluginTransport::spawn(python_launch("pass")).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let err = timeout(Duration::from_secs(5), transport.call("ping", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Protocol(ref m) if m.contains("gone")));
        transport.try_shutdown().unwrap(); // reaps the dead child
    }

    /// Fake plugin for [`HostedPlugin`] tests. Validates the `init` params
    /// (method + authPath) and answers every host RPC with a fixed shape;
    /// `inputs.fail` makes `authorize` error, callback code `"deny"` returns
    /// `ok:false`. The `init` authPath and `fetch` params are recorded to
    /// the files named by the `RECORD` / `RECORD_FETCH` env vars when set.
    const HOSTED_SCRIPT: &str = r#"
import sys, json, os
record = os.environ.get("RECORD")
record_fetch = os.environ.get("RECORD_FETCH")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    params = req.get("params") or {}
    result = None
    error = None
    if method == "init":
        assert method == "init" and params.get("authPath"), "init must carry authPath"
        if record:
            with open(record, "w") as f:
                json.dump({"authPath": params["authPath"]}, f)
        result = {"plugins": [{"id": "fake", "name": "fake plugin"}]}
    elif method == "authorize":
        if (params.get("inputs") or {}).get("fail"):
            error = {"message": "authorize denied"}
        else:
            result = {"session": "plugin-sess-1", "url": "https://example.com/device",
                      "instructions": "Visit the URL to continue", "method": "auto", "opened": False}
    elif method == "callback":
        assert params.get("session") == "plugin-sess-1", "callback must carry the plugin session"
        if params.get("code") == "deny":
            result = {"ok": False, "error": "denied by provider", "provider": "github", "account": "me"}
        else:
            result = {"ok": True, "provider": "github", "account": "me"}
    elif method == "apiKey":
        result = {"ok": True, "provider": params.get("provider", "openai"), "account": params.get("account", "bot")}
    elif method == "signOut":
        result = None
    elif method == "usage":
        result = {"used": 42, "provider": params.get("provider"), "account": params.get("account")}
    elif method == "fetch":
        assert params.get("url") and params.get("method"), "fetch must carry url and method"
        if record_fetch:
            with open(record_fetch, "w") as f:
                json.dump(params, f)
        result = {"status": 200, "headers": {"content-type": "application/json"}, "body": "{\"ok\":true}"}
    else:
        error = {"message": "unknown method: " + str(method)}
    resp = {"id": req["id"]}
    if error is not None:
        resp["error"] = error
    else:
        resp["result"] = result
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    fn hosted_launch(script: &str, env: Vec<(&str, String)>) -> PluginLaunch {
        PluginLaunch {
            program: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: env.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    fn fake_info() -> PluginProviderInfo {
        PluginProviderInfo {
            id: "github".into(),
            name: "GitHub fake".into(),
            methods: vec![],
        }
    }

    fn authorize_params(inputs: Option<BTreeMap<String, Value>>) -> AuthorizeParams {
        AuthorizeParams {
            provider: "github".into(),
            method: 0,
            inputs,
            account: "me".into(),
            proxy: None,
        }
    }

    async fn start_hosted(
        root: &Path,
        env: Vec<(&str, String)>,
    ) -> (HostedPlugin, Arc<PluginAuthStore>) {
        let store = Arc::new(PluginAuthStore::new(root.join("auth.json")));
        let plugin = HostedPlugin::start(
            hosted_launch(HOSTED_SCRIPT, env),
            fake_info(),
            Arc::clone(&store),
        )
        .await
        .unwrap();
        (plugin, store)
    }

    #[tokio::test]
    async fn hosted_start_runs_init_handshake() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("init.json");
        let store_path = root.path().join("auth.json");
        let (plugin, _store) = start_hosted(
            root.path(),
            vec![("RECORD", record.to_string_lossy().into_owned())],
        )
        .await;

        // The fake validated method == "init" and recorded the authPath.
        let recorded: Value = serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(
            recorded["authPath"].as_str().unwrap(),
            store_path.to_str().unwrap()
        );
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_callback_reports_identity_without_writing_credentials() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_hosted(root.path(), vec![]).await;

        let authorization = plugin.authorize(authorize_params(None)).await.unwrap();
        assert_eq!(authorization.session, "s-1");
        assert_eq!(
            authorization.url.as_deref(),
            Some("https://example.com/device")
        );
        assert_eq!(
            authorization.instructions.as_deref(),
            Some("Visit the URL to continue")
        );
        assert_eq!(authorization.method, "auto");
        assert!(!authorization.opened);

        let result = plugin
            .callback(CallbackParams {
                session: authorization.session,
                code: "device-code-1".into(),
            })
            .await
            .unwrap();
        assert!(result.ok);
        assert_eq!(result.provider, "github");
        assert_eq!(result.account, "me");

        // Shared-ledger contract: the plugin process writes the real tokens
        // itself (through the JS host's `client.auth.set`), so the host must
        // not invent a placeholder entry here.
        assert!(store.load().unwrap().is_empty());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_callback_rejection_is_an_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_hosted(root.path(), vec![]).await;

        let authorization = plugin.authorize(authorize_params(None)).await.unwrap();
        assert_eq!(authorization.session, "s-1");

        // ok:false → Err(Rpc(error)) and nothing is written to the store.
        let err = plugin
            .callback(CallbackParams {
                session: authorization.session,
                code: "deny".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m == "denied by provider"));
        assert!(store.load().unwrap().is_empty());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_api_key_reports_result_without_writing_credentials() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_hosted(root.path(), vec![]).await;

        let result = plugin
            .api_key(ApiKeyParams {
                provider: "openai".into(),
                method: 1,
                inputs: None,
                key: "sk-x".into(),
                account: "bot".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert!(result.ok);
        assert_eq!(result.provider, "openai");
        assert_eq!(result.account, "bot");
        // The plugin persisted `{type: "api", key, metadata}` itself; a host
        // write would drop the metadata, so the store stays untouched.
        assert!(store.load().unwrap().is_empty());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_sign_out_removes_entries() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_hosted(root.path(), vec![]).await;

        // A specific account is removed via its `provider:account` key.
        store
            .set(
                "openai:bot",
                PluginAuth::ApiKey {
                    key: "sk-x".into(),
                    extra: BTreeMap::new(),
                },
            )
            .unwrap();
        plugin
            .sign_out(SignOutParams {
                provider: "openai".into(),
                account: "bot".into(),
            })
            .await
            .unwrap();
        assert!(!store.load().unwrap().contains_key("openai:bot"));

        // Empty account removes every entry for the provider.
        let oauth = PluginAuth::OAuth {
            refresh: None,
            access: None,
            expires: None,
            extra: BTreeMap::new(),
        };
        store.set("github:a", oauth.clone()).unwrap();
        store.set("github:b", oauth).unwrap();
        store
            .set(
                "other:c",
                PluginAuth::ApiKey {
                    key: "sk-keep".into(),
                    extra: BTreeMap::new(),
                },
            )
            .unwrap();
        plugin
            .sign_out(SignOutParams {
                provider: "github".into(),
                account: String::new(),
            })
            .await
            .unwrap();
        let map = store.load().unwrap();
        assert!(!map.contains_key("github:a"));
        assert!(!map.contains_key("github:b"));
        assert!(map.contains_key("other:c"));
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_usage_passes_through_json() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_hosted(root.path(), vec![]).await;

        let usage = plugin
            .usage(UsageParams {
                provider: "github".into(),
                account: "me".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(
            usage,
            Some(json!({"used": 42, "provider": "github", "account": "me"}))
        );
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_fetch_forwards_request_and_returns_response() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let record_fetch = root.path().join("fetch.json");
        let (plugin, _store) = start_hosted(
            root.path(),
            vec![("RECORD_FETCH", record_fetch.to_string_lossy().into_owned())],
        )
        .await;

        // The trait method (not an inherent one): provider falls back to
        // the plugin's own id, account rides in the params.
        let response = ProviderPlugin::fetch(
            &plugin,
            PluginFetchRequest {
                url: "https://api.example.com/v1/chat".into(),
                method: "POST".into(),
                headers: vec![
                    ("authorization".into(), "Bearer t".into()),
                    ("content-type".into(), "application/json".into()),
                ],
                body: "{\"q\":1}".into(),
                account: "me".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(response.body, "{\"ok\":true}");

        // The fake asserted url/method presence and recorded the params.
        let recorded: Value =
            serde_json::from_str(&fs::read_to_string(&record_fetch).unwrap()).unwrap();
        assert_eq!(recorded["url"], "https://api.example.com/v1/chat");
        assert_eq!(recorded["method"], "POST");
        assert_eq!(recorded["body"], "{\"q\":1}");
        assert_eq!(recorded["provider"], "github");
        assert_eq!(recorded["account"], "me");
        assert_eq!(recorded["headers"]["authorization"], "Bearer t");
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn hosted_recovers_after_rpc_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_hosted(root.path(), vec![]).await;

        // First call errors on the plugin side...
        let err = plugin
            .authorize(authorize_params(Some(BTreeMap::from([(
                "fail".to_string(),
                json!(true),
            )]))))
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m == "authorize denied"));

        // ...and the same transport still serves the next call; the session
        // counter shows both attempts went through the plugin.
        let authorization = plugin.authorize(authorize_params(None)).await.unwrap();
        assert_eq!(authorization.session, "s-2");
        plugin.transport.try_shutdown().unwrap();
    }

    // ------------------------------------------------------------------
    // Real plugin host: `plugin-host.js` driven by node + the fixture plugin
    // (`tests/fixtures/plugin-hello`). Skipped when node is missing.
    // ------------------------------------------------------------------

    /// Node binary used by [`default_launch`] (env override included).
    fn node_program() -> String {
        std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_string())
    }

    fn node_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            Command::new(node_program())
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
    }

    /// An OpenCode-shaped fixture plugin: `auth.provider = "hello"` with one
    /// oauth method whose callback yields `refresh: "r1" / access: "a1"`.
    fn hello_spec() -> PluginSpec {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-hello");
        PluginSpec::new(dir.to_string_lossy().into_owned(), "")
    }

    fn hello_info() -> PluginProviderInfo {
        PluginProviderInfo {
            id: "hello".into(),
            name: "Hello fixture".into(),
            methods: vec![],
        }
    }

    fn hello_authorize(inputs: Option<BTreeMap<String, Value>>) -> AuthorizeParams {
        AuthorizeParams {
            provider: "hello".into(),
            method: 0,
            inputs,
            account: "new".into(),
            proxy: None,
        }
    }

    async fn start_node_plugin(
        root: &Path,
        plugins: Vec<PluginSpec>,
    ) -> (HostedPlugin, Arc<PluginAuthStore>) {
        let store = Arc::new(PluginAuthStore::new(root.join("auth.json")));
        let plugin = HostedPlugin::start(default_launch(plugins), hello_info(), Arc::clone(&store))
            .await
            .unwrap();
        (plugin, store)
    }

    /// Drive a full oauth flow and return the plugin's result.
    async fn hello_sign_in(plugin: &HostedPlugin, account: &str) -> AuthResult {
        let authorization = plugin
            .authorize(AuthorizeParams {
                account: account.into(),
                ..hello_authorize(None)
            })
            .await
            .unwrap();
        plugin
            .callback(CallbackParams {
                session: authorization.session,
                code: "the-code".into(),
            })
            .await
            .unwrap()
    }

    #[test]
    fn node_default_launch_materializes_the_embedded_host() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let launch = default_launch(vec![hello_spec()]);
        assert_eq!(launch.program, node_program());

        // The script is written from `include_str!` as an ESM module.
        let host = Path::new(&launch.args[0]);
        assert!(host.is_file(), "missing host script: {}", host.display());
        assert_eq!(host.extension().and_then(|e| e.to_str()), Some("mjs"));
        assert_eq!(fs::read_to_string(host).unwrap(), PLUGIN_HOST_JS);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(host).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Plugins ride along in argv so `start` can forward them in `init`.
        assert_eq!(launch.args[1], PLUGIN_ARG);
        let specs: Vec<PluginSpec> = serde_json::from_str(&launch.args[2]).unwrap();
        assert_eq!(specs, vec![hello_spec()]);
        assert_eq!(plugins_from_args(&launch.args), vec![hello_spec()]);
    }

    #[tokio::test]
    async fn node_host_init_loads_the_fixture_plugin() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let auth_path = root.path().join("auth.json");

        // Raw handshake: one loaded plugin, no `error` entry.
        let transport = PluginTransport::spawn(default_launch(vec![hello_spec()])).unwrap();
        let reply = timeout(
            Duration::from_secs(60),
            transport.call(
                "init",
                json!({
                    "authPath": auth_path.to_string_lossy(),
                    "plugins": [hello_spec()],
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let plugins = reply["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["spec"], hello_spec().spec);
        assert!(
            plugins[0].get("error").is_none(),
            "fixture plugin failed to load: {reply}"
        );

        // Unknown methods answer with an error and leave the host alive.
        let err = timeout(
            Duration::from_secs(10),
            transport.call("definitely-not-a-method", json!({})),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m == "no method definitely-not-a-method"));
        transport.try_shutdown().unwrap();

        // `start` accepts the same launch: the non-empty `plugins` array is
        // what the handshake validation checks.
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_authorize_returns_flow_details() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        let authorization = plugin.authorize(hello_authorize(None)).await.unwrap();
        assert_eq!(authorization.session, "s-1");
        assert_eq!(
            authorization.url.as_deref(),
            Some("https://example.test/auth")
        );
        assert_eq!(
            authorization.instructions.as_deref(),
            Some("paste the code")
        );
        assert_eq!(authorization.method, "code");
        assert!(!authorization.opened);
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_callback_persists_real_credentials() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        let result = hello_sign_in(&plugin, "new").await;
        assert!(result.ok);
        assert_eq!(result.provider, "hello");
        assert_eq!(result.account, "new");

        // The JS host wrote the real tokens through `client.auth.set`; the
        // Rust store reads the very same file (one ledger, no placeholder).
        let map = store.load().unwrap();
        match map
            .get("hello:new")
            .expect("credentials were not persisted")
        {
            PluginAuth::OAuth {
                refresh,
                access,
                expires,
                extra,
            } => {
                assert_eq!(refresh.as_deref(), Some("r1"));
                assert_eq!(access.as_deref(), Some("a1"));
                // `Date.now() + 1h`, i.e. milliseconds since the epoch.
                assert!(expires.is_some_and(|e| e > 1_000_000_000_000));
                assert!(extra.is_empty());
            }
            other => panic!("expected an oauth entry, got {other:?}"),
        }

        // On-disk shape is OpenCode's: `{type, refresh, access, expires}`.
        let raw: Value = serde_json::from_str(&fs::read_to_string(store.path()).unwrap()).unwrap();
        assert_eq!(raw["hello:new"]["type"], "oauth");
        assert_eq!(raw["hello:new"]["refresh"], "r1");
        assert_eq!(raw["hello:new"]["access"], "a1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_sign_out_removes_credentials() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        let result = hello_sign_in(&plugin, "new").await;
        assert!(result.ok);
        assert!(store.load().unwrap().contains_key("hello:new"));

        plugin
            .sign_out(SignOutParams {
                provider: "hello".into(),
                account: "new".into(),
            })
            .await
            .unwrap();
        assert!(!store.load().unwrap().contains_key("hello:new"));
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_plugin_error_is_rpc_and_transport_recovers() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        // The fixture's `authorize` throws for `inputs.fail`.
        let err = plugin
            .authorize(hello_authorize(Some(BTreeMap::from([(
                "fail".to_string(),
                json!(true),
            )]))))
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m.contains("boom from plugin")));
        assert!(store.load().unwrap().is_empty());

        // The host process survived and still serves the next call.
        let authorization = plugin.authorize(hello_authorize(None)).await.unwrap();
        assert_eq!(authorization.session, "s-2");
        let result = plugin
            .callback(CallbackParams {
                session: authorization.session,
                code: "the-code".into(),
            })
            .await
            .unwrap();
        assert!(result.ok);
        assert!(store.load().unwrap().contains_key("hello:new"));
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_api_key_writes_metadata_and_clears_provider() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        let result = plugin
            .api_key(ApiKeyParams {
                provider: "hello".into(),
                method: 0,
                inputs: Some(BTreeMap::from([("org".to_string(), json!("acme"))])),
                key: "sk-1".into(),
                account: "bot".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert!(result.ok);
        // The JS host wrote `{type: "api", key, metadata}` itself.
        match store.load().unwrap().get("hello:bot") {
            Some(PluginAuth::ApiKey { key, extra }) => {
                assert_eq!(key, "sk-1");
                assert_eq!(extra.get("metadata"), Some(&json!({"org": "acme"})));
            }
            other => panic!("expected an api entry, got {other:?}"),
        }

        // An empty account signs the whole provider out.
        plugin
            .sign_out(SignOutParams {
                provider: "hello".into(),
                account: String::new(),
            })
            .await
            .unwrap();
        assert!(store.load().unwrap().is_empty());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_usage_without_plugin_hook_is_none() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        // The fixture exposes no `auth.usage` hook: `null` → `None`, not
        // `Some(Value::Null)`.
        let usage = plugin
            .usage(UsageParams {
                provider: "hello".into(),
                account: "new".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(usage, None);
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_init_reports_providers() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        // The metadata comes from the init reply's `providers`, not from the
        // caller-supplied fallback (`hello_info()` says "Hello fixture").
        assert_eq!(plugin.info().id, "hello");
        assert_eq!(plugin.info().name, "Hello from init");
        assert_eq!(
            plugin.info().methods,
            vec![AuthMethod {
                kind: AuthMethodKind::Oauth,
                // The fixture's own label, carried through the init reply.
                label: "Sign in".to_string(),
                prompts: vec![],
            }]
        );
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_load_runs_the_loader_once_per_account() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        // The loader only runs for signed-in accounts: sign in first.
        let result = hello_sign_in(&plugin, "new").await;
        assert!(result.ok);

        let params = LoadParams {
            provider: "hello".into(),
            account: "new".into(),
            proxy: None,
        };
        let first = plugin.load(params.clone()).await.unwrap();
        assert_eq!(first.base_url, "https://hello.example.test");
        assert_eq!(first.api_key, "k-1");
        assert_eq!(
            first.headers,
            BTreeMap::from([("X-Hello".to_string(), "1".to_string())])
        );
        assert!(!first.fetch);

        // Second load: cached — the loader did not run again (another run
        // would have bumped the fixture counter to "k-2").
        let second = plugin.load(params).await.unwrap();
        assert_eq!(second, first);
        assert_eq!(second.api_key, "k-1");
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_load_falls_back_to_the_stored_api_key() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        // Written before `init`, into the very auth file the plugin host reads.
        let store = Arc::new(PluginAuthStore::new(root.path().join("auth.json")));
        store
            .set(
                "hello:bot",
                PluginAuth::ApiKey {
                    key: "k-stored".into(),
                    extra: BTreeMap::new(),
                },
            )
            .unwrap();
        let plugin = HostedPlugin::start(
            default_launch(vec![hello_spec()]),
            hello_info(),
            Arc::clone(&store),
        )
        .await
        .unwrap();

        // The fixture loader omits apiKey when the stored credential is an
        // api key: magpie options() falls back to `stored.key`.
        let options = plugin
            .load(LoadParams {
                provider: "hello".into(),
                account: "bot".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(options.api_key, "k-stored");
        assert_eq!(options.base_url, "https://hello.example.test");
        assert!(!options.fetch);

        // No stored credentials: the loader is not called and the reply is
        // the all-defaults shape.
        let anonymous = plugin
            .load(LoadParams {
                provider: "hello".into(),
                account: "ghost".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(anonymous, LoaderOptions::default());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_models_reports_npm_and_api_id() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        let models = plugin
            .models(ModelsParams {
                provider: "hello".into(),
                account: "new".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "hello-chat");
        assert_eq!(model.name, "Hello Chat");
        assert_eq!(model.npm, "@ai-sdk/openai-compatible");
        assert_eq!(model.api_id, "hello-chat");
        assert_eq!(model.url, "");
        assert_eq!(model.context, 0);
        assert!(!model.reasoning);

        // A `null` reply from the plugin normalizes to an empty list.
        let none = plugin
            .models(ModelsParams {
                provider: "hello".into(),
                account: "none".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert!(none.is_empty());
        plugin.transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn node_host_fetch_is_reachable_through_the_trait() {
        if !node_available() {
            eprintln!("skipping: node not available");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (plugin, _store) = start_node_plugin(root.path(), vec![hello_spec()]).await;

        // Called through the trait — HostedPlugin has no inherent `fetch`.
        let response = ProviderPlugin::fetch(
            &plugin,
            PluginFetchParams {
                url: "https://api.hello.example.test/v1/chat".into(),
                method: "POST".into(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: "{\"q\":1}".into(),
                provider: "hello".into(),
                account: "new".into(),
                model: "hello-chat".into(),
                npm: "@ai-sdk/openai-compatible".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.status, 200);
        // The fixture's fetch hook echoes the request identity back.
        let body: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(body["url"], "https://api.hello.example.test/v1/chat");
        assert_eq!(body["provider"], "hello");
        assert_eq!(body["account"], "new");
        assert_eq!(body["model"], "hello-chat");
        assert_eq!(body["npm"], "@ai-sdk/openai-compatible");
        plugin.transport.try_shutdown().unwrap();
    }
}
