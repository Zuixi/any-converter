use std::sync::Arc;
use std::time::Duration;

use any_converter_server::config_store::ConfigStore;
use any_converter_server::home::Home;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};

const STOP_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an attach probe may wait for `/health` before giving up.
const ATTACH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerRunState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    Attached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub state: ServerRunState,
    pub host: String,
    pub port: u16,
    pub last_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum ServerManagerError {
    #[error("server start failed: {0}")]
    StartFailed(String),
    #[error("server is already starting or running")]
    AlreadyRunning,
    #[error("server is not running")]
    NotRunning,
    #[error("server failed to stop within {0}s")]
    StopTimeout(u64),
    #[error("server stop failed: {0}")]
    StopFailed(String),
    #[error(
        "the gateway is managed by another process; stop it with `any-converter daemon stop` or the command that started it"
    )]
    ExternallyManaged,
}

struct ServerTask {
    shutdown_tx: oneshot::Sender<()>,
    join_handle: tokio::task::JoinHandle<()>,
}

/// The subset of `instance.json` v2 an attaching desktop reads.
#[derive(Deserialize)]
struct AttachInfo {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

/// Resolve instance metadata to a dialable `(host, port)` pair, or `None`
/// when the metadata is absent, unparsable, or not yet bound.
fn attach_target(source: &str) -> Option<(String, u16)> {
    let info: AttachInfo = serde_json::from_str(source).ok()?;
    let host = info.host?;
    let port = info.port?;
    if host.is_empty() || port == 0 {
        return None;
    }
    let dial = match host.as_str() {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1".to_string(),
        other => {
            // Bracket bare IPv6 literals so they survive URL parsing.
            if other.contains(':') && !other.starts_with('[') {
                format!("[{other}]")
            } else {
                other.to_string()
            }
        }
    };
    Some((dial, port))
}

async fn gateway_is_healthy(host: &str, port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(ATTACH_PROBE_TIMEOUT)
        .no_proxy()
        .build()
    else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://{host}:{port}/health"))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    response
        .json::<serde_json::Value>()
        .await
        .is_ok_and(|body| body["status"] == "ok")
}

#[derive(Clone)]
pub struct ServerManager {
    inner: Arc<Mutex<ServerManagerInner>>,
    operations: Arc<Mutex<()>>,
}

struct ServerManagerInner {
    task: Option<ServerTask>,
    status: ServerStatus,
    busy: bool,
    stopping: bool,
    stop_timed_out: bool,
    generation: u64,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ServerManagerInner {
                task: None,
                status: ServerStatus {
                    state: ServerRunState::Stopped,
                    host: "127.0.0.1".to_string(),
                    port: 1228,
                    last_error: None,
                },
                busy: false,
                stopping: false,
                stop_timed_out: false,
                generation: 0,
            })),
            operations: Arc::new(Mutex::new(())),
        }
    }

    pub async fn status(&self) -> ServerStatus {
        let status = self.inner.lock().await.status.clone();
        if status.state != ServerRunState::Attached
            || gateway_is_healthy(&status.host, status.port).await
        {
            return status;
        }

        let mut inner = self.inner.lock().await;
        if inner.status.state == ServerRunState::Attached
            && inner.status.host == status.host
            && inner.status.port == status.port
        {
            inner.status.state = ServerRunState::Stopped;
            inner.status.last_error =
                Some("the externally managed gateway is no longer reachable".to_string());
        }
        inner.status.clone()
    }

    pub async fn start(&self, home: Home) -> Result<ServerStatus, ServerManagerError> {
        let _operation = self.operations.lock().await;
        self.start_inner(home).await
    }

    async fn start_inner(&self, home: Home) -> Result<ServerStatus, ServerManagerError> {
        {
            let mut inner = self.inner.lock().await;
            if inner.busy {
                return Err(ServerManagerError::AlreadyRunning);
            }
            inner.busy = true;
            inner.stopping = false;
            inner.stop_timed_out = false;
            inner.generation += 1;
            inner.status = ServerStatus {
                state: ServerRunState::Starting,
                host: "127.0.0.1".to_string(),
                port: 1228,
                last_error: None,
            };
        }

        // A gateway we did not start may already be running for this home:
        // attach instead of spawning a second instance.
        if let Some(status) = self.attach_if_running(&home).await {
            let mut inner = self.inner.lock().await;
            inner.busy = false;
            inner.status = status.clone();
            return Ok(status);
        }

        let config = match ConfigStore::from_home(&home).load() {
            Ok(document) => document.config().clone(),
            Err(error) => {
                let message = error.to_string();
                self.finish_prepare_failure(&message).await;
                return Err(ServerManagerError::StartFailed(message));
            }
        };

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (ready_tx, mut ready_rx) = oneshot::channel();
        let (done_tx, mut done_rx) = oneshot::channel();
        let status_handle = Arc::clone(&self.inner);
        {
            let mut inner = self.inner.lock().await;
            inner.status.host = config.server.host.clone();
            inner.status.port = config.server.port;
        }
        let generation = {
            let inner = self.inner.lock().await;
            inner.generation
        };

        let join_handle = tokio::spawn(async move {
            let server_result = any_converter_server::run_from_home_with_shutdown_and_ready(
                home,
                async move {
                    let _ = shutdown_rx.await;
                },
                Some(ready_tx),
            )
            .await;
            let error = server_result.err().map(|error| error.to_string());

            let mut inner = status_handle.lock().await;
            if generation != inner.generation {
                return;
            }
            inner.busy = false;
            inner.task = None;
            let stopping = inner.stopping;
            let stop_timed_out = inner.stop_timed_out;
            if stop_timed_out {
                inner.status.state = ServerRunState::Failed;
                inner.status.last_error = Some(format!(
                    "server did not stop within {} seconds",
                    STOP_DRAIN_TIMEOUT.as_secs()
                ));
            } else if stopping && error.is_none() {
                inner.status.state = ServerRunState::Stopped;
                inner.status.last_error = None;
            } else {
                inner.status.state = ServerRunState::Failed;
                inner.status.last_error =
                    Some(error.unwrap_or_else(|| "server task exited unexpectedly".to_string()));
            }
            let _ = done_tx.send(());
        });

        {
            let mut inner = self.inner.lock().await;
            inner.task = Some(ServerTask {
                shutdown_tx,
                join_handle,
            });
        }

        let startup = tokio::select! {
            ready = &mut ready_rx => ready.map_err(|_| "server exited before readiness".to_string()),
            done = &mut done_rx => done
                .map_err(|_| "server task crashed".to_string())
                .and_then(|_| Err("server exited before readiness".to_string())),
        };
        match startup {
            Ok(address) => {
                let mut inner = self.inner.lock().await;
                if generation != inner.generation || inner.task.is_none() {
                    let status = inner.status.clone();
                    return Err(ServerManagerError::StartFailed(
                        status
                            .last_error
                            .unwrap_or_else(|| "server exited early".to_string()),
                    ));
                }
                inner.status.state = ServerRunState::Running;
                inner.status.host = address.ip().to_string();
                inner.status.port = address.port();
                inner.status.last_error = None;
                Ok(inner.status.clone())
            }
            Err(message) => {
                let status = self.status().await;
                Err(ServerManagerError::StartFailed(
                    status.last_error.unwrap_or(message),
                ))
            }
        }
    }

    /// Detect a live gateway for this home (valid `instance.json` whose
    /// `/health` answers) and enter the `Attached` state without touching
    /// `busy` or the task slot.
    async fn attach_if_running(&self, home: &Home) -> Option<ServerStatus> {
        let source = std::fs::read_to_string(home.instance_path()).ok()?;
        let (host, port) = attach_target(&source)?;
        // Any 200 is not enough: a crashed gateway's reassigned port may
        // host another local HTTP server. Require the gateway's own health
        // body so attach never wedges on a stranger.
        if !gateway_is_healthy(&host, port).await {
            return None;
        }
        let status = ServerStatus {
            state: ServerRunState::Attached,
            host,
            port,
            last_error: None,
        };
        // Only enter `attached` after the probe proves the gateway is alive.
        self.inner.lock().await.status = status.clone();
        Some(status)
    }

    pub async fn stop(&self) -> Result<ServerStatus, ServerManagerError> {
        let _operation = self.operations.lock().await;
        self.stop_inner().await
    }

    async fn stop_inner(&self) -> Result<ServerStatus, ServerManagerError> {
        {
            let inner = self.inner.lock().await;
            if inner.status.state == ServerRunState::Attached {
                return Err(ServerManagerError::ExternallyManaged);
            }
        }
        let task = {
            let mut inner = self.inner.lock().await;
            let Some(task) = inner.task.take() else {
                if inner.busy {
                    return Err(ServerManagerError::AlreadyRunning);
                }
                return Err(ServerManagerError::NotRunning);
            };
            inner.stopping = true;
            inner.stop_timed_out = false;
            inner.status.state = ServerRunState::Stopping;
            task
        };

        let ServerTask {
            shutdown_tx,
            join_handle,
        } = task;
        let _ = shutdown_tx.send(());
        let abort_handle = join_handle.abort_handle();
        let join_result = tokio::time::timeout(STOP_DRAIN_TIMEOUT, join_handle).await;
        match join_result {
            Ok(result) => {
                if result.is_err() {
                    let message = "server task panicked during shutdown".to_string();
                    self.force_failed_state(&message).await;
                    return Err(ServerManagerError::StopFailed(message));
                }
                let status = self.status().await;
                if status.state == ServerRunState::Failed {
                    return Err(ServerManagerError::StopFailed(
                        status
                            .last_error
                            .unwrap_or_else(|| "server failed".to_string()),
                    ));
                }
                Ok(status)
            }
            Err(_) => {
                {
                    let mut inner = self.inner.lock().await;
                    inner.stop_timed_out = true;
                }
                abort_handle.abort();
                self.force_failed_state(&format!(
                    "server did not stop within {} seconds",
                    STOP_DRAIN_TIMEOUT.as_secs()
                ))
                .await;
                Err(ServerManagerError::StopTimeout(
                    STOP_DRAIN_TIMEOUT.as_secs(),
                ))
            }
        }
    }

    pub async fn restart(&self, home: Home) -> Result<ServerStatus, ServerManagerError> {
        let _operation = self.operations.lock().await;
        if self.inner.lock().await.status.state == ServerRunState::Attached {
            return Err(ServerManagerError::ExternallyManaged);
        }
        if self.inner.lock().await.busy {
            let _ = self.stop_inner().await;
        }
        self.start_inner(home).await
    }

    pub fn mark_initial_failure(&self, message: &str) {
        if let Ok(mut inner) = self.inner.try_lock() {
            if !inner.busy {
                inner.status.state = ServerRunState::Failed;
                inner.status.last_error = Some(message.to_string());
            }
        }
    }

    #[doc(hidden)]
    pub fn fake_running(host: &str, port: u16) -> Self {
        let manager = Self::new();
        if let Ok(mut inner) = manager.inner.try_lock() {
            inner.status.state = ServerRunState::Running;
            inner.status.host = host.to_string();
            inner.status.port = port;
        }
        manager
    }

    #[doc(hidden)]
    pub fn fake_attached(host: &str, port: u16) -> Self {
        let manager = Self::new();
        if let Ok(mut inner) = manager.inner.try_lock() {
            inner.status.state = ServerRunState::Attached;
            inner.status.host = host.to_string();
            inner.status.port = port;
        }
        manager
    }

    async fn finish_prepare_failure(&self, message: &str) {
        let mut inner = self.inner.lock().await;
        inner.busy = false;
        inner.status.state = ServerRunState::Failed;
        inner.status.last_error = Some(message.to_string());
    }

    async fn force_failed_state(&self, message: &str) {
        let mut inner = self.inner.lock().await;
        inner.busy = false;
        inner.status.state = ServerRunState::Failed;
        inner.status.last_error = Some(message.to_string());
    }
}

impl Default for ServerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_home() -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path().to_path_buf());
        (dir, home)
    }

    #[test]
    fn initial_failure_is_reported_once() {
        let manager = ServerManager::new();
        manager.mark_initial_failure("keychain locked");

        let status = manager.inner.try_lock().unwrap().status.clone();
        assert_eq!(status.state, ServerRunState::Failed);
        assert_eq!(status.last_error.as_deref(), Some("keychain locked"));
    }

    #[test]
    fn fake_running_reports_the_supervised_address() {
        let manager = ServerManager::fake_running("127.0.0.1", 18080);
        let status = manager.inner.try_lock().unwrap().status.clone();
        assert_eq!(status.state, ServerRunState::Running);
        assert_eq!(status.port, 18080);
    }

    #[test]
    fn attach_target_resolves_metadata_hosts() {
        // Valid host/port pairs pass through.
        assert_eq!(
            attach_target(r#"{"host":"127.0.0.1","port":1228}"#),
            Some(("127.0.0.1".to_string(), 1228))
        );
        assert_eq!(
            attach_target(r#"{"host":"localhost","port":1228}"#),
            Some(("localhost".to_string(), 1228))
        );
        assert_eq!(
            attach_target(r#"{"host":"192.168.1.4","port":1228}"#),
            Some(("192.168.1.4".to_string(), 1228))
        );
        // Wildcard binds are not dialable: they resolve to loopback.
        assert_eq!(
            attach_target(r#"{"host":"0.0.0.0","port":1228}"#),
            Some(("127.0.0.1".to_string(), 1228))
        );
        assert_eq!(
            attach_target(r#"{"host":"::","port":1228}"#),
            Some(("127.0.0.1".to_string(), 1228))
        );
        assert_eq!(
            attach_target(r#"{"host":"[::]","port":1228}"#),
            Some(("127.0.0.1".to_string(), 1228))
        );
        // A bare IPv6 literal is bracketed so it survives URL parsing.
        assert_eq!(
            attach_target(r#"{"host":"::1","port":1228}"#),
            Some(("[::1]".to_string(), 1228))
        );
        // Missing host, missing port, unbound port, empty host, garbage.
        assert_eq!(attach_target(r#"{"port":1228}"#), None);
        assert_eq!(attach_target(r#"{"host":"127.0.0.1"}"#), None);
        assert_eq!(attach_target(r#"{"host":"127.0.0.1","port":0}"#), None);
        assert_eq!(attach_target(r#"{"host":"","port":1228}"#), None);
        assert_eq!(attach_target("not json"), None);
    }

    #[tokio::test]
    async fn attach_probe_recognizes_a_running_gateway() {
        let (_dir, home) = temp_home();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 512];
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";
            socket.write_all(response).await.unwrap();
        });

        std::fs::write(
            home.instance_path(),
            serde_json::json!({ "host": "127.0.0.1", "port": port }).to_string(),
        )
        .unwrap();

        let manager = ServerManager::new();
        let status = manager.attach_if_running(&home).await.unwrap();
        assert_eq!(status.state, ServerRunState::Attached);
        assert_eq!(status.host, "127.0.0.1");
        assert_eq!(status.port, port);

        let error = manager.stop().await.unwrap_err();
        assert!(matches!(error, ServerManagerError::ExternallyManaged));

        let _ = probe.await;
    }

    #[tokio::test]
    async fn a_foreign_200_on_health_does_not_attach() {
        // Port reuse after a crash: a stranger answering 200 with any body
        // must not wedge the app in `attached`.
        let (_dir, home) = temp_home();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 512];
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\n\r\n{\"status\":\"imposter\"}";
            socket.write_all(response).await.unwrap();
        });

        std::fs::write(
            home.instance_path(),
            serde_json::json!({ "host": "127.0.0.1", "port": port }).to_string(),
        )
        .unwrap();

        let manager = ServerManager::new();
        assert!(manager.attach_if_running(&home).await.is_none());

        let _ = probe.await;
    }

    #[tokio::test]
    async fn stale_metadata_does_not_attach() {
        let (_dir, home) = temp_home();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        std::fs::write(
            home.instance_path(),
            serde_json::json!({ "host": "127.0.0.1", "port": port }).to_string(),
        )
        .unwrap();

        let manager = ServerManager::new();
        assert!(manager.attach_if_running(&home).await.is_none());
    }

    #[tokio::test]
    async fn attached_manager_refuses_stop_and_restart() {
        let (_dir, home) = temp_home();
        let manager = ServerManager::fake_attached("127.0.0.1", 1228);

        let status = manager.inner.try_lock().unwrap().status.clone();
        assert_eq!(status.state, ServerRunState::Attached);
        assert_eq!(status.port, 1228);

        let error = manager.stop().await.unwrap_err();
        assert!(matches!(error, ServerManagerError::ExternallyManaged));
        let error = manager.restart(home).await.unwrap_err();
        assert!(matches!(error, ServerManagerError::ExternallyManaged));
    }

    #[tokio::test]
    async fn status_leaves_attached_when_the_external_gateway_dies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let manager = ServerManager::fake_attached("127.0.0.1", port);
        drop(listener);

        let status = manager.status().await;
        assert_eq!(status.state, ServerRunState::Stopped);
        assert!(status.last_error.is_some());
    }
}
