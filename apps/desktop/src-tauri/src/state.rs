use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use any_converter_server::config::{ConsoleConfig, DEFAULT_PORT, LoggingConfig};
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
        let mut logging = ConfigStore::from_home(&home)
            .load()
            .map(|document| document.config().logging.clone())
            .unwrap_or_else(|error| {
                log::warn!("could not load canonical logging config; using defaults: {error}");
                LoggingConfig::default()
            });
        apply_desktop_logging_profile(&mut logging, cfg!(debug_assertions));
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
        db.upsert_setting("server.port", &DEFAULT_PORT.to_string())?;
    }
    if !settings.contains_key("logging.max_disk_mb") {
        db.upsert_setting("logging.max_disk_mb", "500")?;
    }
    Ok(())
}

/// Desktop logging profile, layered on top of the user's config.
///
/// `tauri dev` always runs from a terminal, so dev builds mirror the
/// gateway's debug-level logs (including request conversion bodies) to
/// stderr for live debugging. Release builds are windowed apps with no
/// console attached — writing to a detached stdout is wasted work, so
/// console output is off and only the configured file outputs remain.
fn apply_desktop_logging_profile(logging: &mut LoggingConfig, dev: bool) {
    if dev {
        logging.console = ConsoleConfig {
            enabled: true,
            output: "stderr".to_owned(),
            level: None,
            format: "pretty".to_owned(),
        };
        logging.level = raise_to_debug(&logging.level);
    } else {
        logging.console.enabled = false;
    }
}

/// `debug` when `level` is less verbose than debug; otherwise unchanged.
fn raise_to_debug(level: &str) -> String {
    match level.trim().to_lowercase().as_str() {
        "off" | "error" | "warn" | "info" => "debug".to_owned(),
        _ => level.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn dev_profile_forces_stderr_debug_logging() {
        let mut logging = LoggingConfig {
            level: "warn".to_owned(),
            console: ConsoleConfig {
                enabled: false,
                output: "stdout".to_owned(),
                level: Some("error".to_owned()),
                format: "json".to_owned(),
            },
            ..Default::default()
        };
        apply_desktop_logging_profile(&mut logging, true);
        assert!(logging.console.enabled);
        assert_eq!(logging.console.output, "stderr");
        assert_eq!(logging.console.level, None);
        assert_eq!(logging.level, "debug");
    }

    #[test]
    fn release_profile_turns_the_console_off_but_keeps_files() {
        let mut logging = LoggingConfig {
            console: ConsoleConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        apply_desktop_logging_profile(&mut logging, false);
        assert!(!logging.console.enabled);
        // File outputs (name/level/rotation) are the config's business.
        assert!(logging.files.is_empty());
    }

    #[test]
    fn raise_to_debug_never_lowers_a_more_verbose_level() {
        assert_eq!(raise_to_debug("info"), "debug");
        assert_eq!(raise_to_debug("WARN "), "debug");
        assert_eq!(raise_to_debug("off"), "debug");
        assert_eq!(raise_to_debug("debug"), "debug");
        assert_eq!(raise_to_debug("trace"), "trace");
    }
}
