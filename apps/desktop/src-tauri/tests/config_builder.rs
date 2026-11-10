#![allow(clippy::unwrap_used)]

use any_converter_core::convert::Format;
use any_converter_desktop::db::DesktopDb;
use any_converter_desktop::state::seed_defaults;
use std::path::PathBuf;

fn desktop_tauri_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn defaults_to_loopback_and_cleans_up_the_retired_lan_marker() {
    let temp = tempfile::tempdir().unwrap();
    let db = DesktopDb::open(temp.path().join("desktop.sqlite3")).unwrap();

    seed_defaults(&db).unwrap();
    let config = db.build_server_config(temp.path().join("logs")).unwrap();
    assert_eq!(config.server.host, "127.0.0.1");

    // The retired LAN-default marker is removed so it stops showing in Settings.
    assert!(
        !db.settings()
            .unwrap()
            .contains_key("server.host.lan_default_applied")
    );

    // A fresh install that already saw the old LAN default keeps the host the
    // user had (an explicit choice is never rewritten), but drops the marker.
    let temp2 = tempfile::tempdir().unwrap();
    let db2 = DesktopDb::open(temp2.path().join("desktop.sqlite3")).unwrap();
    db2.upsert_setting("server.host", "0.0.0.0").unwrap();
    db2.upsert_setting("server.host.lan_default_applied", "1")
        .unwrap();
    seed_defaults(&db2).unwrap();
    assert_eq!(
        db2.settings()
            .unwrap()
            .get("server.host")
            .map(String::as_str),
        Some("0.0.0.0")
    );
    assert!(
        !db2.settings()
            .unwrap()
            .contains_key("server.host.lan_default_applied")
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
    assert_eq!(
        config.logging.level,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "warn"
        }
    );
    assert_eq!(config.logging.console.enabled, cfg!(debug_assertions));
    assert_eq!(config.logging.files.len(), 1);
    assert_eq!(config.logging.files[0].level.as_deref(), Some("warn"));
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
