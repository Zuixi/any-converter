use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use any_converter_server::config_store::ConfigStore;
use any_converter_server::home::Home;

use crate::db::{DbError, DesktopDb};
use crate::server_manager::ServerManager;

#[derive(Clone)]
pub struct AppState {
    db: DesktopDb,
    data_dir: PathBuf,
    log_dir: PathBuf,
    home: Home,
    server_manager: ServerManager,
    startup_error: Option<String>,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&data_dir)?;
        let home = Home::discover()?;
        let log_dir = home.logs_dir();
        std::fs::create_dir_all(&log_dir)?;
        let db = DesktopDb::open(data_dir.join("any-converter.sqlite3"))?;
        seed_defaults(&db)?;
        let migration_result = crate::migration::migrate_legacy_database(&home, &db);
        let migration_error = migration_result.err().map(|error| error.to_string());
        if let Some(error) = &migration_error {
            log::error!("legacy configuration migration failed: {error}");
        }
        let logging = ConfigStore::from_home(&home)
            .load()
            .map(|document| document.config().logging.clone())
            .unwrap_or_else(|error| {
                log::warn!("could not load canonical logging config; using defaults: {error}");
                Default::default()
            });
        let _ = any_converter_server::logging::init_logging(&logging)?;
        let server_manager = ServerManager::new();
        let startup_error = migration_error
            .as_ref()
            .map(|error| format!("configuration migration failed: {error}"));
        if let Some(error) = &startup_error {
            server_manager.mark_initial_failure(error);
        }
        Ok(Self {
            db,
            data_dir,
            log_dir,
            home,
            server_manager,
            startup_error,
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

    pub fn home(&self) -> Home {
        self.home.clone()
    }

    pub fn server_manager(&self) -> ServerManager {
        self.server_manager.clone()
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }
}

pub fn seed_defaults(db: &DesktopDb) -> Result<(), DbError> {
    let settings = db.settings()?;
    // Bind loopback by default; a LAN bind is an explicit operator choice.
    if !settings.contains_key("server.host") {
        db.upsert_setting("server.host", "127.0.0.1")?;
    }
    // Remove the retired LAN-default marker so it stops showing in Settings.
    if settings.contains_key("server.host.lan_default_applied") {
        db.delete_setting("server.host.lan_default_applied")?;
    }
    if !settings.contains_key("server.port") {
        db.upsert_setting("server.port", "3028")?;
    }
    if !settings.contains_key("logging.max_disk_mb") {
        db.upsert_setting("logging.max_disk_mb", "500")?;
    }
    Ok(())
}
