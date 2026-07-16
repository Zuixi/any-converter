pub mod auth;
pub mod config;
pub mod disk_quota;
pub mod handlers;
pub(crate) mod model;
pub(crate) mod namespace;
pub mod proxy;
pub mod request_log;
pub(crate) mod route_strategy;
pub mod router;
pub mod usage;

use log::info;

use crate::config::ServerConfig;
use crate::router::create_router;

/// Start the HTTP proxy server with the given configuration.
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!("starting any-converter server on {addr}");

    let app = create_router(config);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
