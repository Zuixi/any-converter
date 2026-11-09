#![allow(clippy::unwrap_used)]

//! The proxy is an allowlisted management client, not a generic HTTP bridge.

use any_converter_desktop::server_manager::ServerManager;
use any_converter_desktop::server_proxy::{self, ProxyError};
use any_converter_server::home::Home;

#[tokio::test]
async fn rejects_non_management_paths_without_dialing() {
    for path in [
        "v1/accounts",
        "/v1/../etc/passwd",
        "",
        "/health",
        "/v1/models",
    ] {
        let error = server_proxy::forward(
            ServerManager::new(),
            &Home::from_root("unused-any-converter-home"),
            "GET",
            path,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ProxyError::BadRequest), "path={path}");
    }
}

#[tokio::test]
async fn never_falls_back_to_an_external_server_when_stopped() {
    let error = server_proxy::forward(
        ServerManager::new(),
        &Home::from_root("unused-any-converter-home"),
        "GET",
        "/v1/admin/config",
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ProxyError::NotRunning));
}
