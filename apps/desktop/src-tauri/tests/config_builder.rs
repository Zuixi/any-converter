#![allow(clippy::unwrap_used)]

use any_converter_core::convert::Format;
use any_converter_core::ir::Usage;
use any_converter_desktop::commands::{
    get_usage_summary_from_log_dir, list_request_logs_from_log_dir,
};
use any_converter_desktop::db::DesktopDb;
use any_converter_desktop::state::seed_defaults;
use any_converter_server::request_log::{RequestLogRecord, ResponseBodyKind};
use any_converter_server::storage::SqliteStorage;
use std::path::PathBuf;

fn desktop_tauri_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn defaults_and_migrates_server_host_to_all_interfaces() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();

    seed_defaults(&db).unwrap();
    let config = db.build_server_config(temp.path().join("logs")).unwrap();
    assert_eq!(config.server.host, "0.0.0.0");

    // Legacy localhost default is migrated once when the marker is absent.
    let temp2 = tempfile::tempdir().unwrap();
    let db2 = DesktopDb::open(temp2.path().join("desktop.sqlite3")).unwrap();
    db2.upsert_setting("server.host", "127.0.0.1").unwrap();
    seed_defaults(&db2).unwrap();
    assert_eq!(
        db2.settings()
            .unwrap()
            .get("server.host")
            .map(String::as_str),
        Some("0.0.0.0")
    );

    // An intentional localhost bind after migration is preserved.
    db2.upsert_setting("server.host", "127.0.0.1").unwrap();
    seed_defaults(&db2).unwrap();
    assert_eq!(
        db2.settings()
            .unwrap()
            .get("server.host")
            .map(String::as_str),
        Some("127.0.0.1")
    );
}

#[test]
fn builds_server_config_from_sqlite_provider_routes_and_settings() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();

    db.upsert_setting("server.host", "127.0.0.1").unwrap();
    db.upsert_setting("server.port", "18080").unwrap();
    db.upsert_setting("server.api_key", "hello-any").unwrap();
    db.upsert_setting("logging.max_disk_mb", "256").unwrap();

    let provider_id = db
        .create_provider(
            "kimi",
            Format::OpenAIResponses,
            "https://api.moonshot.cn",
            "keychain:any-converter:kimi",
        )
        .unwrap();
    db.upsert_model_map(provider_id, "*", "kimi-k2").unwrap();
    db.create_model_route("gpt-*", vec![provider_id], Some("kimi-k2"), "priority")
        .unwrap();

    let config = db.build_server_config(temp.path().join("logs")).unwrap();

    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 18080);
    assert_eq!(config.server.api_key.as_deref(), Some("hello-any"));
    assert_eq!(config.providers.len(), 1);
    assert_eq!(config.providers[0].name, "kimi");
    assert_eq!(config.providers[0].format, Format::OpenAIResponses);
    assert_eq!(config.providers[0].base_url, "https://api.moonshot.cn");
    assert_eq!(config.providers[0].api_key, "keychain:any-converter:kimi");
    assert_eq!(config.providers[0].model_map.get("*").unwrap(), "kimi-k2");
    assert_eq!(config.model_routes.len(), 1);
    assert_eq!(config.model_routes[0].pattern, "gpt-*");
    assert_eq!(config.model_routes[0].providers, vec!["kimi".to_string()]);
    assert_eq!(config.logging.max_disk_mb, 256);
    assert!(config.logging.request_log.enabled);
}

#[test]
fn tauri_bundle_declares_platform_icon_assets() {
    let tauri_dir = desktop_tauri_dir();
    let config_path = tauri_dir.join("tauri.conf.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();

    let icons = config["bundle"]["icon"].as_array().unwrap();
    let icon_paths = icons
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();

    assert!(
        icon_paths.iter().any(|path| path.ends_with(".ico")),
        "Windows bundles require an .ico asset in bundle.icon"
    );
    assert!(
        icon_paths.iter().any(|path| path.ends_with("32x32.png")),
        "bundle.icon should include a 32x32 PNG asset"
    );
    assert!(
        icon_paths.iter().any(|path| path.ends_with("128x128.png")),
        "bundle.icon should include a 128x128 PNG asset"
    );
    assert!(
        icon_paths
            .iter()
            .any(|path| path.ends_with("128x128@2x.png")),
        "bundle.icon should include a 128x128@2x PNG asset"
    );
    assert!(
        icon_paths.iter().any(|path| path.ends_with("icon.png")),
        "bundle.icon should include the base icon.png asset"
    );

    for relative_path in icon_paths {
        let asset_path = tauri_dir.join(relative_path);
        assert!(
            asset_path.exists(),
            "missing icon asset: {}",
            asset_path.display()
        );
    }
}

#[test]
fn reads_request_logs_and_usage_from_server_log_sqlite() {
    let temp = tempfile::tempdir().unwrap();
    let app_db = DesktopDb::open(temp.path().join("any-converter.sqlite3")).unwrap();
    app_db
        .create_provider(
            "empty-app-db",
            Format::OpenAIChat,
            "https://api.openai.com",
            "keychain:any-converter:empty-app-db",
        )
        .unwrap();

    let log_dir = temp.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let storage = SqliteStorage::open_in_log_dir(&log_dir).unwrap();
    storage
        .insert_request_log(&RequestLogRecord {
            request_id: "req-from-log-db".to_string(),
            timestamp: "2026-07-16T12:00:00Z".to_string(),
            client_format: "openai_chat".to_string(),
            client_id: None,
            provider: "openai".to_string(),
            client_model: "gpt-4.1".to_string(),
            upstream_model: "gpt-4.1".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            request_body: None,
            upstream_request_body: None,
            response_status: 200,
            response_body: ResponseBodyKind::Json {
                text: "{}".to_string(),
            },
            latency_ms: 42,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
            trace: None,
            truncated: false,
        })
        .unwrap();

    let logs = list_request_logs_from_log_dir(&log_dir, 10).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].request_id, "req-from-log-db");

    let usage = get_usage_summary_from_log_dir(&log_dir, 10).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].total_tokens, 15);

    let missing = temp.path().join("missing-logs");
    assert!(
        list_request_logs_from_log_dir(&missing, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        get_usage_summary_from_log_dir(&missing, 10)
            .unwrap()
            .is_empty()
    );
}
