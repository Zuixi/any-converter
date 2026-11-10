pub mod admin;
pub(crate) mod agent_id;
pub mod agents;
pub mod agents_registry;
pub mod auth;
pub mod config;
pub mod config_store;
pub mod control_plane;
pub mod credentials;
pub mod disk_quota;
pub mod handlers;
pub mod home;
pub mod lockfile;
pub mod logging;
pub(crate) mod model;
pub(crate) mod namespace;
pub mod observability;
pub mod period;
pub mod plugins;
pub mod pricing;
pub mod provider_pool;
pub mod proxy;
pub mod request_log;
pub(crate) mod route_strategy;
pub mod router;
pub mod secrets;
pub mod signin_task;
pub mod storage;
pub mod usage;
pub mod web_auth;
pub mod web_ui;
pub mod wire;

use log::info;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::ServerConfig;
use crate::control_plane::{ControlPlane, PreparedServer};
use crate::home::Home;
use crate::lockfile::{DEFAULT_ACQUIRE_TIMEOUT, InstanceLock, InstanceLockGuard};
use crate::router::create_router;
use crate::secrets::ApiKeyStore;

pub use storage::LedgerRow;

/// RFC 4180 CSV of the ledger rows, header included.
pub fn ledger_csv(rows: &[LedgerRow]) -> String {
    LedgerRow::ledger_csv(rows)
}

/// Start the HTTP proxy server with the given configuration.
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with_shutdown(config, std::future::pending::<()>()).await
}

/// Start the managed gateway for an explicit canonical home.
pub async fn run_from_home(home: Home) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_from_home_with_shutdown_and_ready(home, std::future::pending::<()>(), None).await
}

/// Discover the canonical home and start the managed gateway.
pub async fn run_managed() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_from_home(Home::discover()?).await
}

/// Prepare and run the canonical managed gateway under its instance lock.
pub async fn run_from_home_with_shutdown_and_ready(
    home: Home,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ready: Option<oneshot::Sender<std::net::SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ownership = InstanceOwnership::acquire(&home).await?;
    let keys = ApiKeyStore::for_home(&home);
    let prepared = ControlPlane::prepare(home, keys)?;
    run_prepared_with_shutdown_and_ready(prepared, Some(ownership), shutdown, ready).await
}

/// Start the HTTP proxy server and shut it down when the given future resolves.
pub async fn run_with_shutdown(
    config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ready_sender, _ready_receiver) = oneshot::channel();
    run_config_with_ready(config, shutdown, Some(ready_sender)).await
}

/// Start a legacy in-memory router and report readiness only after bind.
///
/// The legacy router has no control plane; management routes fail closed.
pub async fn run_with_shutdown_and_ready(
    config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ready: oneshot::Sender<std::net::SocketAddr>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_config_with_ready(config, shutdown, Some(ready)).await
}

async fn run_config_with_ready(
    config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    mut ready: Option<oneshot::Sender<std::net::SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!("starting any-converter server on {addr}");

    let app = create_router(config);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    if let Some(ready) = ready.take() {
        let _ = ready.send(bound);
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    // No in-flight sign-in (listener or temp state) may outlive the
    // process: cancel every outstanding task (P10).
    signin_task::SignInTasks::global().shutdown().await;

    Ok(())
}

/// Start a prepared managed router and shut down when `shutdown` resolves.
pub async fn run_prepared(
    prepared: PreparedServer,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ready_sender, _ready_receiver) = oneshot::channel();
    run_prepared_with_shutdown_and_ready(prepared, None, shutdown, Some(ready_sender)).await
}

/// Start a prepared managed router, reporting readiness only after bind.
///
/// When `ownership` is present the instance metadata (host, port, version)
/// is written after the bind and before the ready signal, so readiness means
/// the metadata is visible to attaching clients.
pub async fn run_prepared_with_shutdown_and_ready(
    prepared: PreparedServer,
    ownership: Option<InstanceOwnership>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    mut ready: Option<oneshot::Sender<std::net::SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = format!(
        "{}:{}",
        prepared.config.server.host, prepared.config.server.port
    );
    info!("starting managed any-converter server on {addr}");

    let config_revision = prepared
        .control_plane
        .config_store()
        .load()?
        .revision()
        .clone();
    let web_auth = std::sync::Arc::clone(&prepared.web_auth);
    let control_plane = std::sync::Arc::clone(&prepared.control_plane);
    let app = crate::router::create_router_from_prepared(prepared);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    // Metadata before readiness: if the write fails the process exits with
    // the listener dropped and the control plane never claimed ready for a
    // dead instance.
    if let Some(ownership) = &ownership {
        ownership.write_metadata(config_revision, bound)?;
    }
    control_plane.mark_ready(bound);
    // A persistent key never appears in logs: the operator unlocks with
    // `any-converter daemon status` or the `web-key` file. An ephemeral
    // run's URL is the only way to learn that key, so it is printed with
    // the key — and log files are 0600.
    if web_auth.is_persistent() {
        info!(
            "UI: {} (unlock with the page key from `any-converter daemon status`)",
            web_auth.base_url(bound)
        );
    } else {
        info!("UI: {}", web_auth.url(bound));
    }
    if let Some(ready) = ready.take() {
        let _ = ready.send(bound);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    signin_task::SignInTasks::global().shutdown().await;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct InstanceMetadata {
    pid: u32,
    started_at_unix: u64,
    config_revision: crate::config_store::ConfigRevision,
    ownership_token: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    version: String,
}

/// Owns the canonical instance lock and the `instance.json` metadata file.
///
/// The fields are private on purpose: only acquisition and the post-bind
/// metadata write are exposed, and dropping the value removes the metadata
/// file while the ownership token still matches.
pub struct InstanceOwnership {
    path: std::path::PathBuf,
    ownership_token: String,
    #[allow(dead_code)]
    guard: InstanceLockGuard,
}

impl InstanceOwnership {
    pub async fn acquire(home: &Home) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(home.root())?;
        let guard = InstanceLock::new(home.instance_lock_path())
            .acquire(DEFAULT_ACQUIRE_TIMEOUT)
            .await?;
        Ok(Self {
            path: home.instance_path(),
            ownership_token: uuid::Uuid::new_v4().to_string(),
            guard,
        })
    }

    pub fn write_metadata(
        &self,
        config_revision: crate::config_store::ConfigRevision,
        bound: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let metadata = InstanceMetadata {
            pid: std::process::id(),
            started_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            config_revision,
            ownership_token: self.ownership_token.clone(),
            host: dial_host(bound.ip()),
            port: bound.port(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let bytes = serde_json::to_vec(&metadata)?;
        atomic_replace(&self.path, &bytes)?;
        Ok(())
    }
}

/// A bind IP rewritten to something a client can dial. Wildcard binds have
/// no dialable address, so they report loopback; everything else is the
/// bare IP (IPv6 stays unbracketed — the reader adds brackets).
fn dial_host(ip: std::net::IpAddr) -> String {
    if ip.is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        ip.to_string()
    }
}

impl Drop for InstanceOwnership {
    fn drop(&mut self) {
        let owned = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|source| serde_json::from_str::<InstanceMetadata>(&source).ok())
            .is_some_and(|metadata| metadata.ownership_token == self.ownership_token);
        if owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn atomic_replace(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write_with_options(
            |file| {
                file.write_all(bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                Ok::<_, std::io::Error>(())
            },
            options,
        )
        .map_err(|error| match error {
            atomicwrites::Error::Internal(error) | atomicwrites::Error::User(error) => error,
        })
}
