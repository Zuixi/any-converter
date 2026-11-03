//! Task-based sign-in flows (magpie `internal/provider/signin.go`).
//!
//! A sign-in is an inspectable, cancelable *task*: `POST /v1/accounts/signin`
//! starts one and returns a [`SignInTask`] snapshot at once; the UI polls
//! `GET /v1/accounts/signin/{id}`, submits a pasted callback or cancels.
//!
//! Mirrors magpie's `signIns` map:
//!
//! - one in-flight flow per id, tracked in a process-wide registry
//!   ([`SignInTasks::global`], seeded by the router);
//! - a 10-minute timeout ([`DEFAULT_SIGN_IN_TIMEOUT`], magpie
//!   `signInTimeout`);
//! - `cancel` finishes the flow: it aborts the background task (closing the
//!   Gemini loopback listener, which lives in `GeminiSignIn::wait`'s axum
//!   server) and removes the entry, so no listener or temp state outlives
//!   the task;
//! - `shutdown` cancels every outstanding task when the server stops.
//!
//! A completed flow stores the credential in the account store (the way the
//! CLI does) and clears any revoked marker, then reports `done` with the
//! account's user, or `failed` with the error.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::credentials::Credential;
use crate::credentials::account::AccountResolver;
use crate::credentials::copilot::{CopilotEndpoints, CopilotSignIn};
use crate::credentials::gemini::{
    DEFAULT_SIGN_IN_TIMEOUT, GeminiAccount, GeminiEndpoints, GeminiSignIn, SignInError,
};

/// The terminal state of a task's flow (`waiting|done|failed|canceled`).
pub const STATE_WAITING: &str = "waiting";
pub const STATE_DONE: &str = "done";
pub const STATE_FAILED: &str = "failed";
pub const STATE_CANCELED: &str = "canceled";

/// Gemini instructions shown to the user (loopback + paste fallback).
const GEMINI_INSTRUCTIONS: &str = "Open the URL in your browser. If the browser \
cannot reach this host, paste the callback URL from the browser's address bar below.";
/// Copilot device-flow instructions.
const COPILOT_INSTRUCTIONS: &str = "Open the verification page and enter the code.";

/// One sign-in task snapshot (magpie `SignInState`).
#[derive(Debug, Clone, Serialize)]
pub struct SignInTask {
    /// Registry id (`signin-<uuid>`); also the `id` of the sign-in route.
    pub id: String,
    /// `gemini` or `copilot`.
    pub provider: String,
    /// `waiting` | `done` | `failed` | `canceled`.
    pub state: String,
    /// Gemini: the authorize URL. Copilot: the verification page.
    pub url: String,
    /// Copilot: the user code to enter at the verification page.
    pub code: String,
    /// What the user should do next.
    pub instructions: String,
    /// Gemini: the user may paste the callback URL instead (true today).
    pub paste_callback: bool,
    /// Copilot: the user enters the device code at the vendor's page.
    pub paste_code: bool,
    /// The signed-in user (gemini email / copilot hint) once `done`.
    pub user: String,
    /// The account's plan; not resolved at sign-in time (empty today).
    pub plan: String,
    /// Failure reason once `failed`.
    pub error: String,
}

/// What the background task runs for each provider.
enum TaskDriver {
    Gemini {
        sign_in: Arc<GeminiSignIn>,
        /// The OAuth `state` `submit` must pass to `finish_pasted`.
        oauth_state: String,
    },
    /// The Copilot driver lives entirely in the background task; the
    /// cancel handle is the task's `background` join handle.
    Copilot,
}

/// One registered task: the snapshot plus its cancel handle (the background
/// task) and its driver (holding the listener / poller).
struct TaskHandle {
    task: SignInTask,
    background: Option<JoinHandle<()>>,
    driver: TaskDriver,
    /// Where a completed sign-in is persisted.
    accounts: Arc<AccountResolver>,
    /// When the flow reached done/failed/canceled, so a finished entry can
    /// be pruned: the UI polls until it sees the final state, then stops.
    finished_at: Option<std::time::Instant>,
}

/// How long a finished task stays readable after it settles. The window
/// exists only so the last poll sees `done`; nothing is kept forever, or a
/// user who signs in again after every revocation would grow the map.
const FINISHED_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// The registry: live tasks keyed by id.
#[derive(Default)]
struct Registry {
    tasks: HashMap<String, TaskHandle>,
}

/// In-flight sign-ins, keyed by id, each with a cancel handle that closes
/// its listener and removes its temporary state (magpie's `signIns`).
pub struct SignInTasks {
    inner: Arc<std::sync::Mutex<Registry>>,
    /// The server-level proxy ([`crate::config::ServerSettings::proxy`]);
    /// `start` falls back to it when the provider has none of its own.
    server_proxy: Option<String>,
}

impl Registry {
    /// Drop tasks that finished a while ago, so the map is bounded by the
    /// sign-ins in flight plus the last few minutes of finished ones.
    fn prune(&mut self) {
        let now = std::time::Instant::now();
        self.tasks.retain(|_, handle| {
            handle
                .finished_at
                .is_none_or(|at| now.duration_since(at) < FINISHED_TTL)
        });
    }
}

/// The process-wide registry (magpie's package-level `signIns` map).
static GLOBAL: OnceLock<Arc<SignInTasks>> = OnceLock::new();

impl SignInTasks {
    /// The process-wide registry, initialized lazily with no server proxy
    /// (handlers use this when no router has seeded it yet).
    pub fn global() -> Arc<SignInTasks> {
        Self::init(None)
    }

    /// Get-or-seed the process-wide registry with `server_proxy`. The
    /// router calls this once per process; the first caller's proxy wins.
    pub fn init(server_proxy: Option<String>) -> Arc<SignInTasks> {
        GLOBAL
            .get_or_init(|| Arc::new(SignInTasks::new(server_proxy)))
            .clone()
    }

    /// A standalone registry (tests).
    pub fn new(server_proxy: Option<String>) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Registry::default())),
            server_proxy,
        }
    }

    /// Start a gemini or copilot sign-in and return the task at once
    /// (magpie `startSignIn`). Uses the server-level proxy; callers that
    /// resolved a provider-level proxy use [`Self::start_with_proxy`].
    pub async fn start(
        &self,
        provider: &str,
        accounts: Arc<AccountResolver>,
    ) -> Result<SignInTask, String> {
        self.start_with_proxy(provider, self.server_proxy.clone(), accounts)
            .await
    }

    /// [`Self::start`] with an explicit egress proxy (provider.proxy >
    /// server.proxy > none), resolved by the caller.
    pub async fn start_with_proxy(
        &self,
        provider: &str,
        proxy: Option<String>,
        accounts: Arc<AccountResolver>,
    ) -> Result<SignInTask, String> {
        match provider.to_ascii_lowercase().as_str() {
            "gemini" => self.start_gemini(proxy, accounts).await,
            "copilot" => self.start_copilot(proxy, accounts).await,
            other => Err(format!(
                "unknown provider: {other} (supported: gemini, copilot)"
            )),
        }
    }

    /// The task snapshot for `id`, when it is still registered.
    pub fn status(&self, id: &str) -> Option<SignInTask> {
        // The registry lock is never held across an await point, so
        // blocking here cannot starve the runtime.
        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        registry.prune();
        registry.tasks.get(id).map(|handle| handle.task.clone())
    }

    /// Finish a flow with a pasted callback URL (gemini).
    ///
    /// Runs the existing [`GeminiSignIn::finish_pasted`] path: the
    /// background `wait` (and its loopback listener) is aborted first so
    /// the pasted exchange owns the session. A failed exchange marks the
    /// task `failed` and returns the snapshot (magpie `signInFlow.finish`).
    /// Copilot has no pasted input; submitting one is an error.
    pub async fn submit(&self, id: &str, input: &str) -> Result<SignInTask, String> {
        let (sign_in, oauth_state, background, accounts) = {
            let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            let handle = registry
                .tasks
                .get_mut(id)
                .ok_or_else(|| format!("sign-in task {id} not found"))?;
            if handle.task.state != STATE_WAITING {
                // A finished task keeps its snapshot; submit is a no-op.
                return Ok(handle.task.clone());
            }
            match &handle.driver {
                TaskDriver::Copilot => {
                    return Err("copilot has no pasted input".to_string());
                }
                TaskDriver::Gemini {
                    sign_in,
                    oauth_state,
                } => (
                    sign_in.clone(),
                    oauth_state.clone(),
                    handle.background.take(),
                    handle.accounts.clone(),
                ),
            }
        };

        // Close the loopback listener before consuming the session: the
        // background `wait` owns the listener; aborting drops its future,
        // which signals the listener's graceful shutdown.
        if let Some(background) = background {
            background.abort();
            let _ = background.await;
        }

        let result = sign_in.finish_pasted(input, &oauth_state).await;
        let (state, user, error) = settle_gemini(&accounts, result).await;
        update_task(&self.inner, id, |task| {
            task.state = state;
            task.user = user;
            task.error = error;
        })
        .await;
        self.status(id)
            .ok_or_else(|| format!("sign-in task {id} vanished during submit"))
    }

    /// Cancel a task: mark it `canceled`, abort its background task
    /// (closing the listener / poller) and remove it from the registry.
    /// Returns whether the task existed.
    pub async fn cancel(&self, id: &str) -> bool {
        let removed = {
            let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            registry.tasks.remove(id)
        };
        let Some(mut handle) = removed else {
            return false;
        };
        handle.task.state = STATE_CANCELED.to_string();
        if let Some(background) = handle.background.take() {
            background.abort();
            let _ = background.await;
        }
        // Dropping the handle drops the driver: a Gemini listener still
        // held by the session (paste path) is closed here.
        true
    }

    /// Drop every task (server shutdown): cancels each, so no listener or
    /// temp state outlives the process.
    pub async fn shutdown(&self) {
        let tasks = {
            let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut registry.tasks)
        };
        for mut handle in tasks.into_values() {
            handle.task.state = STATE_CANCELED.to_string();
            if let Some(background) = handle.background.take() {
                background.abort();
                let _ = background.await;
            }
        }
    }

    /// Start a Gemini browser sign-in: bind the loopback listener, report
    /// `waiting` + the authorize URL at once, then `wait` in the
    /// background until the callback arrives or the timeout elapses.
    async fn start_gemini(
        &self,
        proxy: Option<String>,
        accounts: Arc<AccountResolver>,
    ) -> Result<SignInTask, String> {
        let sign_in = Arc::new(
            GeminiSignIn::with_proxy(GeminiEndpoints::default(), proxy.as_deref())
                .map_err(|err| err.to_string())?,
        );
        // Local only (PKCE + loopback bind); completes before the response
        // goes out, so the caller gets the URL at once.
        let started = sign_in.start().await.map_err(|err| err.to_string())?;

        let id = format!("signin-{}", Uuid::new_v4());
        let task = SignInTask {
            id: id.clone(),
            provider: "gemini".to_string(),
            state: STATE_WAITING.to_string(),
            url: started.url,
            code: String::new(),
            instructions: GEMINI_INSTRUCTIONS.to_string(),
            paste_callback: started.paste_callback,
            paste_code: false,
            user: String::new(),
            plan: String::new(),
            error: String::new(),
        };

        let inner = self.inner.clone();
        let wait_sign_in = sign_in.clone();
        let wait_accounts = accounts.clone();
        let background = tokio::spawn(async move {
            let result = wait_sign_in.wait(DEFAULT_SIGN_IN_TIMEOUT).await;
            let (state, user, error) = settle_gemini(&wait_accounts, result).await;
            update_task(&inner, &id, |task| {
                task.state = state;
                task.user = user;
                task.error = error;
            })
            .await;
            mark_finished(&inner, &id);
        });

        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        registry.prune();
        registry.tasks.insert(
            task.id.clone(),
            TaskHandle {
                task: task.clone(),
                background: Some(background),
                driver: TaskDriver::Gemini {
                    sign_in,
                    oauth_state: started.state,
                },
                accounts,
                finished_at: None,
            },
        );
        Ok(task)
    }

    /// Start a Copilot device flow: report `waiting` at once (the device
    /// code arrives from the vendor a moment later, filling `url`/`code`),
    /// then poll in the background until the flow completes.
    async fn start_copilot(
        &self,
        proxy: Option<String>,
        accounts: Arc<AccountResolver>,
    ) -> Result<SignInTask, String> {
        let sign_in = Arc::new(
            CopilotSignIn::with_proxy(CopilotEndpoints::default(), proxy.as_deref())
                .map_err(|err| err.to_string())?,
        );

        let id = format!("signin-{}", Uuid::new_v4());
        let task = SignInTask {
            id: id.clone(),
            provider: "copilot".to_string(),
            state: STATE_WAITING.to_string(),
            url: CopilotEndpoints::default().verification_uri,
            code: String::new(),
            instructions: COPILOT_INSTRUCTIONS.to_string(),
            paste_callback: false,
            paste_code: true,
            user: String::new(),
            plan: String::new(),
            error: String::new(),
        };

        let inner = self.inner.clone();
        let start_sign_in = sign_in.clone();
        let bg_accounts = accounts.clone();
        let background = tokio::spawn(async move {
            match start_sign_in.start().await {
                Ok(started) => {
                    update_task(&inner, &id, |task| {
                        task.url = started.verification_uri.clone();
                        task.code = started.user_code.clone();
                    })
                    .await;
                    let result = start_sign_in.wait(&started, DEFAULT_SIGN_IN_TIMEOUT).await;
                    let (state, user, error) = settle_copilot(&bg_accounts, result).await;
                    update_task(&inner, &id, |task| {
                        task.state = state;
                        task.user = user;
                        task.error = error;
                    })
                    .await;
                }
                Err(err) => {
                    update_task(&inner, &id, |task| {
                        task.state = STATE_FAILED.to_string();
                        task.error = err.to_string();
                    })
                    .await;
                }
            }
        });

        let mut registry = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        registry.prune();
        registry.tasks.insert(
            task.id.clone(),
            TaskHandle {
                task: task.clone(),
                background: Some(background),
                driver: TaskDriver::Copilot,
                accounts,
                finished_at: None,
            },
        );
        Ok(task)
    }
}

/// Persist a completed Gemini sign-in and report `(state, user, error)`
/// (shared by the background `wait` completion and `submit`).
async fn settle_gemini(
    accounts: &AccountResolver,
    result: Result<GeminiAccount, SignInError>,
) -> (String, String, String) {
    let account = match result {
        Ok(account) => account,
        Err(err) => return (STATE_FAILED.to_string(), String::new(), err.to_string()),
    };
    let account_id = format!("gemini:{}", account.email);
    let credential = Credential::OAuth {
        provider: "gemini".to_string(),
        account: account.email,
        access_token: account.tokens.access_token,
        refresh_token: account.tokens.refresh_token,
        expires_at_unix: account.tokens.expires_at.map(|at| {
            let secs_left = at
                .saturating_duration_since(std::time::Instant::now())
                .as_secs() as i64;
            unix_now() + secs_left
        }),
    };
    match accounts.store_sign_in(&account_id, credential).await {
        Ok(()) => (STATE_DONE.to_string(), account_id, String::new()),
        Err(err) => (
            STATE_FAILED.to_string(),
            String::new(),
            format!("storing the account failed: {err}"),
        ),
    }
}

/// Persist a completed Copilot device flow and report `(state, user, error)`.
async fn settle_copilot(
    accounts: &AccountResolver,
    result: Result<crate::credentials::copilot::CopilotAccount, SignInError>,
) -> (String, String, String) {
    let account = match result {
        Ok(account) => account,
        Err(err) => return (STATE_FAILED.to_string(), String::new(), err.to_string()),
    };
    let account_id = format!("copilot:{}", account.user_code_hint);
    let credential = Credential::OAuth {
        provider: "copilot".to_string(),
        account: account.user_code_hint,
        access_token: account.access_token,
        refresh_token: None,
        expires_at_unix: None,
    };
    match accounts.store_sign_in(&account_id, credential).await {
        Ok(()) => (STATE_DONE.to_string(), account_id, String::new()),
        Err(err) => (
            STATE_FAILED.to_string(),
            String::new(),
            format!("storing the account failed: {err}"),
        ),
    }
}

/// Apply `f` to the task snapshot for `id`, when still registered.
async fn update_task(
    inner: &Arc<std::sync::Mutex<Registry>>,
    id: &str,
    f: impl FnOnce(&mut SignInTask),
) {
    let mut registry = inner.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(handle) = registry.tasks.get_mut(id) {
        f(&mut handle.task);
    }
}

/// Note that a task has settled, so `prune` may drop it after
/// [`FINISHED_TTL`].
fn mark_finished(inner: &Arc<std::sync::Mutex<Registry>>, id: &str) {
    let mut registry = inner.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(handle) = registry.tasks.get_mut(id) {
        handle.finished_at = Some(std::time::Instant::now());
    }
}

/// Current unix time in seconds (mirrors `account::unix_now`).
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Duration;

    use crate::credentials::FileSecretStore;

    use super::*;

    fn test_accounts(dir: &tempfile::TempDir) -> Arc<AccountResolver> {
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        Arc::new(AccountResolver::new(store, reqwest::Client::new()))
    }

    /// The loopback port the authorize URL redirects to.
    fn gemini_port(task: &SignInTask) -> u16 {
        let url = &task.url;
        let host = "127.0.0.1";
        let start = url.find(host).unwrap_or_else(|| panic!("no host in {url}")) + host.len();
        let encoded = &url[start..];
        let encoded = encoded
            .strip_prefix("%3A")
            .unwrap_or_else(|| panic!("no %3A after host in {url}"));
        encoded
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or_else(|_| panic!("no port in {url}"))
    }

    async fn wait_for_port(port: u16, open: bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let connected = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok();
            if connected == open {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "port {port} never became open={open}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn gemini_start_reports_waiting_with_url_at_once() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let task = tasks.start("gemini", accounts.clone()).await.unwrap();

        assert_eq!(task.state, STATE_WAITING);
        assert!(
            task.url
                .starts_with("https://accounts.google.com/o/oauth2/v2/auth?")
        );
        assert!(task.paste_callback);
        assert!(!task.paste_code);
        assert_eq!(tasks.status(&task.id).unwrap().id, task.id);

        // Tidy up: the background wait would run for 10 minutes otherwise.
        assert!(tasks.cancel(&task.id).await);
    }

    #[tokio::test]
    async fn submit_with_mismatched_state_marks_failed_without_panicking() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let task = tasks.start("gemini", accounts.clone()).await.unwrap();
        let bogus = "http://127.0.0.1:1/oauth2callback?state=wrong-state&code=auth-code";
        let submitted = tasks.submit(&task.id, bogus).await.unwrap();
        assert!(
            submitted.state == STATE_FAILED || submitted.state == STATE_WAITING,
            "unexpected state: {submitted:?}"
        );
        let status = tasks.status(&task.id).unwrap();
        assert!(
            status.state == STATE_FAILED || status.state == STATE_WAITING,
            "unexpected state: {status:?}"
        );
    }

    #[tokio::test]
    async fn copilot_submit_is_refused() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let task = tasks.start("copilot", accounts.clone()).await.unwrap();
        let err = tasks.submit(&task.id, "WDJB-MJHT").await.unwrap_err();
        assert!(err.contains("copilot has no pasted input"), "err: {err}");
        // The task itself keeps waiting.
        assert_eq!(tasks.status(&task.id).unwrap().state, STATE_WAITING);
        assert!(tasks.cancel(&task.id).await);
    }

    #[tokio::test]
    async fn cancel_marks_canceled_and_removes_from_registry() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let task = tasks.start("gemini", accounts.clone()).await.unwrap();
        assert!(tasks.status(&task.id).is_some());

        assert!(tasks.cancel(&task.id).await);
        // Removed from the registry (GET → 404 in the router) — no leak.
        assert!(tasks.status(&task.id).is_none());
        assert!(!tasks.cancel(&task.id).await, "second cancel finds nothing");
    }

    #[tokio::test]
    async fn cancel_closes_the_gemini_loopback_listener() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let task = tasks.start("gemini", accounts.clone()).await.unwrap();
        let port = gemini_port(&task);
        // The background wait's server comes up shortly after start.
        wait_for_port(port, true).await;

        assert!(tasks.cancel(&task.id).await);
        // After cancel the listener must be gone: the abort drops the
        // wait future, whose graceful shutdown closes the server.
        wait_for_port(port, false).await;
    }

    #[tokio::test]
    async fn shutdown_cancels_every_outstanding_task() {
        let tasks = Arc::new(SignInTasks::new(None));
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let a = tasks.start("gemini", accounts.clone()).await.unwrap();
        let b = tasks.start("gemini", accounts.clone()).await.unwrap();

        tasks.shutdown().await;
        assert!(tasks.status(&a.id).is_none());
        assert!(tasks.status(&b.id).is_none());
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let tasks = SignInTasks::new(None);
        let dir = tempfile::tempdir().unwrap();
        let accounts = test_accounts(&dir);

        let err = tasks.start("openai", accounts).await.unwrap_err();
        assert!(err.contains("unknown provider"), "err: {err}");
    }

    #[test]
    fn sign_in_task_serializes_without_tokens() {
        let task = SignInTask {
            id: "signin-1".into(),
            provider: "gemini".into(),
            state: "waiting".into(),
            url: "https://example.test/authorize".into(),
            code: String::new(),
            instructions: "open it".into(),
            paste_callback: true,
            paste_code: false,
            user: String::new(),
            plan: String::new(),
            error: String::new(),
        };
        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["state"], "waiting");
        assert!(json.get("access_token").is_none());
        assert!(json.get("refresh_token").is_none());
    }
}
