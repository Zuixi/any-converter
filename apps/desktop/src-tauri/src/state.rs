use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::db::{DbError, DesktopDb};
use crate::server_manager::ServerManager;

#[derive(Clone)]
pub struct AppState {
    db: DesktopDb,
    data_dir: PathBuf,
    log_dir: PathBuf,
    server_manager: ServerManager,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        let log_dir = data_dir.join("logs");
        std::fs::create_dir_all(&log_dir)?;
        let db = DesktopDb::open(data_dir.join("any-converter.sqlite3"))?;
        seed_defaults(&db)?;
        Ok(Self {
            db,
            data_dir,
            log_dir,
            server_manager: ServerManager::new(),
        })
    }

    pub fn db(&self) -> DesktopDb {
        self.db.clone()
    }

    pub fn log_dir(&self) -> PathBuf {
        self.log_dir.clone()
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    pub fn server_manager(&self) -> ServerManager {
        self.server_manager.clone()
    }
}

pub fn seed_defaults(db: &DesktopDb) -> Result<(), DbError> {
    let settings = db.settings()?;
    // Bind all interfaces by default so LAN clients can reach the embedded server.
    if !settings.contains_key("server.host") {
        db.upsert_setting("server.host", "0.0.0.0")?;
    } else if !settings.contains_key("server.host.lan_default_applied")
        && settings.get("server.host").map(String::as_str) == Some("127.0.0.1")
    {
        // One-time migration from the previous localhost-only product default.
        db.upsert_setting("server.host", "0.0.0.0")?;
    }
    if !settings.contains_key("server.host.lan_default_applied") {
        db.upsert_setting("server.host.lan_default_applied", "1")?;
    }
    if !settings.contains_key("server.port") {
        db.upsert_setting("server.port", "8080")?;
    }
    if !settings.contains_key("logging.max_disk_mb") {
        db.upsert_setting("logging.max_disk_mb", "500")?;
    }
    Ok(())
}
