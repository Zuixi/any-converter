use std::sync::Arc;
use std::time::Duration;

use any_converter_server::config_store::ConfigStore;
use any_converter_server::home::Home;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};

const STOP_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerRunState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
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
}

struct ServerTask {
    shutdown_tx: oneshot::Sender<()>,
    join_handle: tokio::task::JoinHandle<()>,
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
        self.inner.lock().await.status.clone()
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

    pub async fn stop(&self) -> Result<ServerStatus, ServerManagerError> {
        let _operation = self.operations.lock().await;
        self.stop_inner().await
    }

    async fn stop_inner(&self) -> Result<ServerStatus, ServerManagerError> {
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
}
