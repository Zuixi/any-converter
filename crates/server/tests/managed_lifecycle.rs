#![allow(clippy::unwrap_used, clippy::expect_used)]

use any_converter_server::control_plane::ControlPlane;
use any_converter_server::home::Home;
use any_converter_server::secrets::{ApiKeyStore, KEYCHAIN_SERVICE, MemoryKeychain};
use std::sync::Arc;
use std::time::Duration;

fn temp_home() -> Home {
    let root = std::env::temp_dir().join(format!(
        "any-converter-managed-lifecycle-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Home::from_root(root)
}

#[tokio::test]
async fn readiness_is_sent_only_after_the_listener_binds() {
    let home = temp_home();
    std::fs::write(
        home.config_path(),
        r#"
[server]
host = "127.0.0.1"
port = 0
"#,
    )
    .unwrap();
    let keys = ApiKeyStore::with_backend(KEYCHAIN_SERVICE, Arc::new(MemoryKeychain::new()));
    let prepared = ControlPlane::prepare(home.clone(), keys).unwrap();
    assert_eq!(prepared.config.server.port, 0);

    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        any_converter_server::run_prepared_with_shutdown_and_ready(
            prepared,
            async move {
                let _ = shutdown_receiver.await;
            },
            Some(ready_sender),
        )
        .await
    });

    let address = tokio::time::timeout(Duration::from_secs(10), ready_receiver)
        .await
        .expect("ready signal timed out")
        .unwrap();
    assert_eq!(address.ip().to_string(), "127.0.0.1");
    assert_ne!(address.port(), 0);

    let response = reqwest::get(format!("http://{address}/health"))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    shutdown_sender.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("server did not stop in time")
        .unwrap()
        .unwrap();
    assert_eq!(result, ());

    let _ = std::fs::remove_dir_all(home.root());
}
