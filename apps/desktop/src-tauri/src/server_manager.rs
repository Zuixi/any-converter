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
    #[error(
        "provider \"{0}\" has no readable key in the system keychain (keychain entry missing or unreadable; see log); open the app and re-enter its API key"
    )]
    UnresolvedProviderKey(String),
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
                    host: "0.0.0.0".to_string(),
                    port: 1228,
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
        let unresolved = resolve_provider_keys(&mut config);
        if let Some(provider) = unresolved.first() {
            return Err(ServerManagerError::UnresolvedProviderKey(provider.clone()));
        }

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

fn resolve_provider_keys(config: &mut ServerConfig) -> Vec<String> {
    resolve_keys_with(config, secrets::get_provider_key)
}

/// Replace `keychain:any-converter:<account>` references with the secret from
/// the system keychain, returning the names of providers that failed to
/// resolve.
///
/// On failure the `api_key` is cleared rather than left as the reference
/// string: a reference is not a credential, and forwarding it upstream would
/// both leak the reference and produce a confusing auth failure.
fn resolve_keys_with<F, E>(config: &mut ServerConfig, get: F) -> Vec<String>
where
    F: Fn(&str) -> Result<String, E>,
    E: std::fmt::Display,
{
    let mut unresolved = Vec::new();
    for provider in &mut config.providers {
        if let Some(account) = provider.api_key.strip_prefix("keychain:any-converter:") {
            match get(account) {
                Ok(secret) => provider.api_key = secret,
                Err(err) => {
                    log::error!(
                        "failed to resolve provider \"{}\" secret from keychain: {err}",
                        provider.name
                    );
                    provider.api_key = String::new();
                    unresolved.push(provider.name.clone());
                }
            }
        }
    }
    unresolved
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn config_with_providers(providers: &[(&str, &str)]) -> ServerConfig {
        let mut toml = String::from("[server]\nhost = \"127.0.0.1\"\nport = 8080\n");
        for (name, api_key) in providers {
            toml.push_str(&format!(
                r#"
[[providers]]
name = "{name}"
format = "openai_chat"
base_url = "https://example.com"
api_key = "{api_key}"
"#
            ));
        }
        ServerConfig::from_toml(&toml).unwrap()
    }

    #[test]
    fn resolves_keychain_reference_to_secret() {
        let mut config = config_with_providers(&[("openai", "keychain:any-converter:acct-1")]);

        let unresolved = resolve_keys_with(&mut config, |_| Ok::<_, String>("sk-real".into()));

        assert!(unresolved.is_empty());
        assert_eq!(config.providers[0].api_key, "sk-real");
    }

    #[test]
    fn failed_resolution_clears_key_and_reports_provider() {
        let mut config = config_with_providers(&[
            ("openai", "keychain:any-converter:acct-1"),
            ("gemini", "keychain:any-converter:missing"),
            ("literal", "sk-literal"),
        ]);

        let unresolved = resolve_keys_with(&mut config, |account| {
            if account == "acct-1" {
                Ok("sk-real".to_string())
            } else {
                Err("no such keychain entry".to_string())
            }
        });

        assert_eq!(unresolved, vec!["gemini".to_string()]);
        assert_eq!(config.providers[0].api_key, "sk-real");
        assert_eq!(config.providers[1].api_key, "");
        // Literal keys are not touched by resolution.
        assert_eq!(config.providers[2].api_key, "sk-literal");
    }
}
