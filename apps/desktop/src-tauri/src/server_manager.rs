use std::path::PathBuf;
use std::sync::Arc;

use any_converter_server::config::ServerConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};

use crate::db::{DbError, DesktopDb};
use crate::secrets;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRunState {
    Stopped,
    Starting,
    Running,
    Error,
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
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("server already running")]
    AlreadyRunning,
    #[error("server is not running")]
    NotRunning,
}

struct ServerTask {
    shutdown_tx: oneshot::Sender<()>,
    join_handle: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct ServerManager {
    inner: Arc<Mutex<ServerManagerInner>>,
}

struct ServerManagerInner {
    task: Option<ServerTask>,
    status: ServerStatus,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ServerManagerInner {
                task: None,
                status: ServerStatus {
                    state: ServerRunState::Stopped,
                    host: "127.0.0.1".to_string(),
                    port: 8080,
                    last_error: None,
                },
            })),
        }
    }

    pub async fn status(&self) -> ServerStatus {
        self.inner.lock().await.status.clone()
    }

    pub async fn start(
        &self,
        db: DesktopDb,
        log_dir: PathBuf,
    ) -> Result<ServerStatus, ServerManagerError> {
        let mut config = db.build_server_config(log_dir)?;
        resolve_provider_keys(&mut config);

        let mut inner = self.inner.lock().await;
        if inner.task.is_some() {
            return Err(ServerManagerError::AlreadyRunning);
        }
        inner.status = ServerStatus {
            state: ServerRunState::Starting,
            host: config.server.host.clone(),
            port: config.server.port,
            last_error: None,
        };

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let status_handle = self.inner.clone();
        let join_handle = tokio::spawn(async move {
            let server_error = any_converter_server::run_with_shutdown(config, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .err()
            .map(|error| error.to_string());

            let mut inner = status_handle.lock().await;
            inner.task = None;
            match server_error {
                None => {
                    inner.status.state = ServerRunState::Stopped;
                    inner.status.last_error = None;
                }
                Some(error) => {
                    inner.status.state = ServerRunState::Error;
                    inner.status.last_error = Some(error);
                }
            }
        });
        inner.task = Some(ServerTask {
            shutdown_tx,
            join_handle,
        });
        inner.status.state = ServerRunState::Running;
        Ok(inner.status.clone())
    }

    pub async fn stop(&self) -> Result<ServerStatus, ServerManagerError> {
        let task = {
            let mut inner = self.inner.lock().await;
            let Some(task) = inner.task.take() else {
                return Err(ServerManagerError::NotRunning);
            };
            inner.status.state = ServerRunState::Stopped;
            task
        };
        let _ = task.shutdown_tx.send(());
        let _ = task.join_handle.await;
        Ok(self.status().await)
    }

    pub async fn restart(
        &self,
        db: DesktopDb,
        log_dir: PathBuf,
    ) -> Result<ServerStatus, ServerManagerError> {
        if self.inner.lock().await.task.is_some() {
            let _ = self.stop().await;
        }
        self.start(db, log_dir).await
    }
}

impl Default for ServerManager {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_provider_keys(config: &mut ServerConfig) {
    for provider in &mut config.providers {
        if let Some(account) = provider.api_key.strip_prefix("keychain:any-converter:") {
            match secrets::get_provider_key(account) {
                Ok(secret) => provider.api_key = secret,
                Err(err) => log::warn!("failed to resolve provider secret from keychain: {err}"),
            }
        }
    }
}
