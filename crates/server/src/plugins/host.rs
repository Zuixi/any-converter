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
//! Runtime constraint: the workspace tokio is built without the `process`
//! and `io` features, so the child is driven with `std::process` plus a
//! dedicated reader thread; the only tokio surface is `tokio::sync`.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::methods;
use super::{
    ApiKeyParams, ApiKeyResult, AuthResult, Authorization, AuthorizeParams, CallbackParams,
    CallbackResult, PluginAuth, PluginAuthStore, PluginError, PluginFuture, PluginProviderInfo,
    ProviderPlugin, SignOutParams, UsageParams,
};

/// Error message used whenever the plugin process is gone.
const GONE_MSG: &str = "plugin process is gone";

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
/// involved, and returns the raw status/headers/body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFetchRequest {
    /// Full request URL.
    pub url: String,
    /// HTTP method (upper case).
    pub method: String,
    /// Request headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Raw request body, already a string; empty when there is none.
    pub body: String,
}

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
    /// Spawn the plugin subprocess and perform the `init` handshake.
    ///
    /// `init` carries the host auth store path (the plugin persists real
    /// credentials there itself) and must answer within 60 seconds with a
    /// `plugins` array. The array must be non-empty; its entries are not
    /// parsed here.
    pub async fn start(
        launch: PluginLaunch,
        info: PluginProviderInfo,
        auth: Arc<PluginAuthStore>,
    ) -> Result<Self, PluginError> {
        let transport = Arc::new(PluginTransport::spawn(launch)?);
        let auth_path = auth.path().to_string_lossy().into_owned();
        let handshake = timeout(
            Duration::from_secs(60),
            transport.call("init", json!({ "authPath": auth_path })),
        )
        .await;
        match handshake {
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
                    .is_some_and(|plugins| !plugins.is_empty()) => {}
            Ok(Ok(_)) => {
                let _ = transport.try_shutdown();
                return Err(PluginError::Protocol(
                    "init response must include a non-empty `plugins` array".into(),
                ));
            }
        }
        Ok(Self {
            info,
            transport,
            auth,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_session: AtomicU64::new(0),
        })
    }

    /// Forward an HTTP request to the plugin (`fetch` RPC) and return its
    /// raw response.
    pub async fn fetch(
        &self,
        account: &str,
        req: PluginFetchRequest,
    ) -> Result<PluginFetchResponse, PluginError> {
        let PluginFetchRequest {
            url,
            method,
            headers,
            body,
        } = req;
        let headers: Map<String, Value> = headers
            .into_iter()
            .map(|(name, value)| (name, Value::String(value)))
            .collect();
        let params = json!({
            "url": url,
            "method": method,
            "headers": headers,
            "body": body,
            "provider": self.info.id,
            "account": account,
        });
        let value = self.transport.call("fetch", params).await?;
        serde_json::from_value(value)
            .map_err(|e| PluginError::Protocol(format!("invalid `fetch` response: {e}")))
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
        let auth = Arc::clone(&self.auth);
        let sessions = Arc::clone(&self.sessions);
        Box::pin(async move {
            let ctx = {
                let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
                guard.remove(&params.session)
            }
            .ok_or_else(|| {
                PluginError::Protocol(format!("unknown session `{}`", params.session))
            })?;
            let result = Self::rpc::<CallbackResult>(
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
            // Placeholder entry: the plugin process writes the real
            // credentials to the auth file itself (it knows the path from
            // `init`); the host's store only records that the account
            // exists. T8 fills in the refresh token.
            let provider = if result.provider.is_empty() {
                ctx.provider.clone()
            } else {
                result.provider.clone()
            };
            let account = if result.account.is_empty() {
                ctx.account.clone()
            } else {
                result.account.clone()
            };
            auth.set(
                &format!("{provider}:{account}"),
                PluginAuth::OAuth {
                    refresh: None,
                    access: None,
                    expires: None,
                    extra: BTreeMap::new(),
                },
            )
            .map_err(|e| PluginError::Storage(e.to_string()))?;
            Ok(result)
        })
    }

    fn api_key(&self, params: ApiKeyParams) -> PluginFuture<AuthResult> {
        let transport = Arc::clone(&self.transport);
        let auth = Arc::clone(&self.auth);
        Box::pin(async move {
            let request = serde_json::to_value(&params)
                .map_err(|e| PluginError::Rpc(format!("failed to encode apiKey params: {e}")))?;
            let result = Self::rpc::<ApiKeyResult>(&transport, methods::API_KEY, request).await?;
            if !result.ok {
                return Err(PluginError::Rpc(
                    result.error.unwrap_or_else(|| "apiKey failed".to_string()),
                ));
            }
            auth.set(
                &format!("{}:{}", result.provider, result.account),
                PluginAuth::ApiKey {
                    key: params.key,
                    extra: BTreeMap::new(),
                },
            )
            .map_err(|e| PluginError::Storage(e.to_string()))?;
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
            Ok(Some(value))
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
    async fn hosted_authorize_replaces_session_and_callback_stores_placeholder() {
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

        // Placeholder entry: real tokens are written by the plugin process
        // itself; the host store only records that the account exists.
        assert_eq!(
            store.load().unwrap().get("github:me"),
            Some(&PluginAuth::OAuth {
                refresh: None,
                access: None,
                expires: None,
                extra: BTreeMap::new(),
            })
        );
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
    async fn hosted_api_key_stores_entry() {
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
        assert_eq!(
            store.load().unwrap().get("openai:bot"),
            Some(&PluginAuth::ApiKey {
                key: "sk-x".into(),
                extra: BTreeMap::new(),
            })
        );
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

        let response = plugin
            .fetch(
                "me",
                PluginFetchRequest {
                    url: "https://api.example.com/v1/chat".into(),
                    method: "POST".into(),
                    headers: vec![
                        ("authorization".into(), "Bearer t".into()),
                        ("content-type".into(), "application/json".into()),
                    ],
                    body: "{\"q\":1}".into(),
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
}
