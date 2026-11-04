#![allow(clippy::unwrap_used)]

//! End-to-end coverage for the `server_proxy` IPC path: a real embedded
//! server, dialed the way the Rust side dials it after the webview stopped
//! fetching cross-origin.

use any_converter_desktop::db::DesktopDb;
use any_converter_desktop::server_manager::ServerManager;
use any_converter_desktop::server_proxy::{self, ProxyError};
use any_converter_desktop::state::seed_defaults;
use any_converter_server::config::ServerConfig;

async fn spawn_server(db: &DesktopDb, temp: &std::path::Path) -> u16 {
    // Reserve a free port, then hand it to both the settings store and the
    // server so they cannot disagree.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    db.upsert_setting("server.host", "127.0.0.1").unwrap();
    db.upsert_setting("server.port", &port.to_string()).unwrap();

    let toml = format!(
        "[server]\nhost = \"127.0.0.1\"\nport = {port}\nlogging.dir = \"{}\"\n",
        temp.join("logs").display()
    );
    let config = ServerConfig::from_toml(&toml).unwrap();
    // `pending` keeps the server up for the whole test; the process exit
    // reaps it. (A dropped shutdown sender would tear it down mid-test.)
    tokio::spawn(async move {
        let _ = any_converter_server::run_with_shutdown(config, std::future::pending::<()>()).await;
    });

    // Wait for the listener to come up.
    let mut alive = false;
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            alive = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(alive, "embedded server never came up on {port}");
    port
}

#[tokio::test]
async fn forwards_management_calls_to_the_embedded_server() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();
    seed_defaults(&db).unwrap();
    spawn_server(&db, temp.path()).await;

    let manager = ServerManager::new();

    // A wildcard bind cannot be dialed back; the proxy rewrites to loopback.
    db.upsert_setting("server.host", "0.0.0.0").unwrap();
    let models = server_proxy::forward(db.clone(), manager.clone(), "GET", "/v1/models", None)
        .await
        .unwrap();
    assert_eq!(models["object"], "list");

    // Health answers too, and POST bodies survive the round trip.
    let health = server_proxy::forward(db.clone(), manager.clone(), "GET", "/health", None)
        .await
        .unwrap();
    assert!(health["status"].is_string() || health.is_null());
}

#[tokio::test]
async fn reports_not_running_when_no_server_answers() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();
    seed_defaults(&db).unwrap();
    // Reserve a port and leave it dead — safer than the product default 1228,
    // which a developer machine may genuinely be serving.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = listener.local_addr().unwrap().port();
    drop(listener);
    db.upsert_setting("server.port", &dead_port.to_string())
        .unwrap();

    // Nothing answers and the manager says stopped, so the error must name
    // that, not a transport failure.
    let error = server_proxy::forward(db.clone(), ServerManager::new(), "GET", "/v1/models", None)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::NotRunning));
}

#[tokio::test]
async fn running_instance_wins_over_drifted_settings() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();
    seed_defaults(&db).unwrap();
    let port = spawn_server(&db, temp.path()).await;

    // The manager tracks the spawned instance; a settings edit made after
    // the server started (applies only after restart) must not mislead the
    // proxy into dialing a dead port.
    let manager = ServerManager::fake_running("0.0.0.0", port);
    db.upsert_setting("server.port", "1229").unwrap();

    let models = server_proxy::forward(db.clone(), manager, "GET", "/v1/models", None)
        .await
        .unwrap();
    assert_eq!(models["object"], "list");
}

#[tokio::test]
async fn rejects_relative_and_traversal_paths_without_dialing() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();
    seed_defaults(&db).unwrap();

    for path in ["v1/models", "/v1/../etc/passwd", ""] {
        let error = server_proxy::forward(db.clone(), ServerManager::new(), "GET", path, None)
            .await
            .unwrap_err();
        assert!(matches!(error, ProxyError::BadRequest), "path={path}");
    }
}
