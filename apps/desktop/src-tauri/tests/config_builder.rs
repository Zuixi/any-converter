#![allow(clippy::unwrap_used)]

use any_converter_core::convert::Format;
use any_converter_desktop::db::DesktopDb;

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
