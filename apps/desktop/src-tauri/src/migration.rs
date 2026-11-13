//! One-way migration from the legacy Desktop SQLite configuration to the
//! server-owned canonical home.

use std::path::Path;
use std::sync::Arc;

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use any_converter_server::config::DEFAULT_PORT;
use any_converter_server::config_store::ConfigStore;
use any_converter_server::control_plane::{
    ConfigPatch, ControlPlane, LoggingPatch, ModelRoutePatch, ProviderPatch, ServerPatch,
};
use any_converter_server::home::Home;
use any_converter_server::secrets::{ApiKeyStore, SecretRef};

use crate::db::DesktopDb;
use crate::secrets;

const MIGRATION_VERSION: u8 = 1;

/// Ports older releases used as their default.
///
/// A legacy Desktop database still storing one of these never recorded an
/// explicit operator choice, so migration moves it to [`DEFAULT_PORT`]
/// instead of carrying a retired default forward.
const RETIRED_DEFAULT_PORTS: &[u16] = &[1228];

fn is_retired_default_port(port: u16) -> bool {
    RETIRED_DEFAULT_PORTS.contains(&port)
}

fn normalize_legacy_port(port: u16) -> u16 {
    if is_retired_default_port(port) {
        DEFAULT_PORT
    } else {
        port
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationOutcome {
    Completed,
    ExternalConfigPreserved,
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error(transparent)]
    Secret(#[from] crate::secrets::SecretError),
    #[error(transparent)]
    ServerSecret(#[from] any_converter_server::secrets::SecretError),
    #[error(transparent)]
    ControlPlane(#[from] any_converter_server::control_plane::ControlPlaneError),
    #[error(transparent)]
    ConfigStore(#[from] any_converter_server::config_store::ConfigStoreError),
    #[error("migration marker io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration marker serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported migration marker version {version} phase {phase:?}")]
    UnsupportedMarker { version: u8, phase: String },
    #[error("migration marker is complete but canonical config is missing: {0}")]
    CompleteMarkerMissingConfig(std::path::PathBuf),
}

#[derive(Debug, Serialize, Deserialize)]
struct MigrationMarker {
    version: u8,
    phase: String,
}

pub fn migrate_legacy_database(
    home: &Home,
    db: &DesktopDb,
) -> Result<MigrationOutcome, MigrationError> {
    let marker_path = home.root().join("desktop-migration.json");
    let config_existed = home.config_path().exists();
    match migration_start(&marker_path, config_existed)? {
        MigrationStart::Run => {}
        MigrationStart::Completed => {
            // Homes that completed the v1 import before the port default
            // changed still carry the retired port in their canonical file.
            repair_retired_default_port(home, ApiKeyStore::for_home(home))?;
            return Ok(MigrationOutcome::Completed);
        }
        MigrationStart::ExternalConfigPreserved => {
            return Ok(MigrationOutcome::ExternalConfigPreserved);
        }
    }

    write_marker(&marker_path, "in_progress")?;
    let legacy = db.build_server_config(home.logs_dir())?;
    let keys = ApiKeyStore::for_home(home);
    migrate_provider_keys(&legacy, &keys)?;
    migrate_inference_key(&legacy, &keys)?;

    let prepared = ControlPlane::prepare(home.clone(), keys)?;
    let control_plane = Arc::clone(&prepared.control_plane);
    let view = control_plane.config_view()?;
    let patch = legacy_patch(home, &legacy);
    control_plane.patch_config(&view.revision, patch)?;
    write_marker(&marker_path, "complete")?;

    Ok(MigrationOutcome::Completed)
}

/// Move a canonical config off a retired default port.
///
/// This runs for homes whose v1 migration already completed, so the
/// database-to-TOML import will not run again. It is a no-op once the file
/// no longer carries a retired port, and it leaves explicitly chosen ports
/// untouched.
fn repair_retired_default_port(home: &Home, keys: ApiKeyStore) -> Result<(), MigrationError> {
    let store = ConfigStore::from_home(home);
    let document = store.load()?;
    if !is_retired_default_port(document.config().server.port) {
        return Ok(());
    }

    let prepared = ControlPlane::prepare(home.clone(), keys)?;
    let control_plane = Arc::clone(&prepared.control_plane);
    let view = control_plane.config_view()?;
    let patch = ConfigPatch {
        server: Some(ServerPatch {
            port: Some(DEFAULT_PORT),
            ..Default::default()
        }),
        ..Default::default()
    };
    control_plane.patch_config(&view.revision, patch)?;
    Ok(())
}

#[derive(Debug)]
enum MigrationStart {
    Run,
    Completed,
    ExternalConfigPreserved,
}

fn migration_start(
    marker_path: &Path,
    config_existed: bool,
) -> Result<MigrationStart, MigrationError> {
    if !marker_path.exists() {
        return Ok(if config_existed {
            MigrationStart::ExternalConfigPreserved
        } else {
            MigrationStart::Run
        });
    }

    let marker = std::fs::read_to_string(marker_path)?;
    let marker: MigrationMarker = serde_json::from_str(&marker)?;
    if marker.version != MIGRATION_VERSION {
        return Err(MigrationError::UnsupportedMarker {
            version: marker.version,
            phase: marker.phase,
        });
    }
    match marker.phase.as_str() {
        "complete" => {
            if config_existed {
                Ok(MigrationStart::Completed)
            } else {
                Err(MigrationError::CompleteMarkerMissingConfig(
                    marker_path.to_path_buf(),
                ))
            }
        }
        "in_progress" => Ok(MigrationStart::Run),
        phase => Err(MigrationError::UnsupportedMarker {
            version: marker.version,
            phase: phase.to_owned(),
        }),
    }
}

fn migrate_provider_keys(
    legacy: &any_converter_server::config::ServerConfig,
    keys: &ApiKeyStore,
) -> Result<(), MigrationError> {
    for provider in &legacy.providers {
        let value = if let Some(account) = provider.api_key.strip_prefix("keychain:any-converter:")
        {
            secrets::get_provider_key(account)?
        } else if !provider.api_key.is_empty() {
            provider.api_key.clone()
        } else {
            continue;
        };
        let reference = SecretRef::Provider(provider.name.clone());
        match keys.try_get_provider(&provider.name)? {
            Some(existing) if existing == value => {}
            Some(_) => {
                return Err(MigrationError::ServerSecret(
                    any_converter_server::secrets::SecretError::Conflict(reference),
                ));
            }
            None => {
                keys.set_provider(&provider.name, &value)?;
            }
        }
    }
    Ok(())
}

fn migrate_inference_key(
    legacy: &any_converter_server::config::ServerConfig,
    keys: &ApiKeyStore,
) -> Result<(), MigrationError> {
    if let Some(api_key) = legacy
        .server
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty())
    {
        match keys.try_get(&SecretRef::Inference)? {
            Some(existing) if existing == api_key => {}
            Some(_) => {
                return Err(MigrationError::ServerSecret(
                    any_converter_server::secrets::SecretError::Conflict(SecretRef::Inference),
                ));
            }
            None => {
                keys.set_inference(api_key)?;
            }
        }
    }
    Ok(())
}

fn legacy_patch(home: &Home, legacy: &any_converter_server::config::ServerConfig) -> ConfigPatch {
    ConfigPatch {
        server: Some(ServerPatch {
            host: Some(legacy.server.host.clone()),
            port: Some(normalize_legacy_port(legacy.server.port)),
            proxy: legacy.server.proxy.clone().map(Some),
        }),
        logging: Some(LoggingPatch {
            dir: Some(Some(home.logs_dir().to_string_lossy().to_string())),
            max_disk_mb: Some(legacy.logging.max_disk_mb),
            request_log_enabled: Some(legacy.logging.request_log.enabled),
        }),
        upsert_providers: legacy
            .providers
            .iter()
            .map(|provider| ProviderPatch {
                name: provider.name.clone(),
                format: Some(provider.format),
                base_url: Some(provider.base_url.clone()),
                api_key_ref: provider
                    .plugin
                    .is_none()
                    .then(|| SecretRef::Provider(provider.name.clone()).canonical()),
                model_map: Some(provider.model_map.clone().into_iter().collect()),
                account: provider.account.clone().map(Some),
                accounts: Some(provider.accounts.clone()),
                proxy: provider.proxy.clone().map(Some),
                auth_scheme: None,
            })
            .collect(),
        remove_providers: Vec::new(),
        upsert_model_routes: legacy
            .model_routes
            .iter()
            .map(|route| ModelRoutePatch {
                pattern: route.pattern.clone(),
                providers: route.provider_names(),
                upstream_model: route.upstream_model.clone(),
                strategy: Some(route.strategy.clone()),
            })
            .collect(),
        remove_model_routes: Vec::new(),
    }
}

fn write_marker(path: &Path, phase: &str) -> Result<(), MigrationError> {
    let marker = MigrationMarker {
        version: MIGRATION_VERSION,
        phase: phase.to_owned(),
    };
    let bytes = serde_json::to_vec(&marker)?;
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| {
            use std::io::Write;
            file.write_all(&bytes)
        })
        .map_err(|error| match error {
            atomicwrites::Error::Internal(error) | atomicwrites::Error::User(error) => error,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn complete_marker_never_reimports_legacy_sqlite() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-marker-{}-complete",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.toml"), "[server]\n").unwrap();
        let marker = root.join("desktop-migration.json");
        std::fs::write(&marker, r#"{"version":1,"phase":"complete"}"#).unwrap();

        assert!(matches!(
            migration_start(&marker, true).unwrap(),
            MigrationStart::Completed
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn complete_marker_without_canonical_config_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-marker-{}-missing",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("desktop-migration.json");
        std::fs::write(&marker, r#"{"version":1,"phase":"complete"}"#).unwrap();

        assert!(matches!(
            migration_start(&marker, false).unwrap_err(),
            MigrationError::CompleteMarkerMissingConfig(_)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn marker_states_are_versioned_and_explicit() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-marker-{}-states",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("desktop-migration.json");

        assert!(matches!(
            migration_start(&marker, false).unwrap(),
            MigrationStart::Run
        ));
        std::fs::write(root.join("config.toml"), "[server]\n").unwrap();
        assert!(matches!(
            migration_start(&marker, true).unwrap(),
            MigrationStart::ExternalConfigPreserved
        ));
        std::fs::write(&marker, r#"{"version":1,"phase":"in_progress"}"#).unwrap();
        assert!(matches!(
            migration_start(&marker, true).unwrap(),
            MigrationStart::Run
        ));
        std::fs::write(&marker, r#"{"version":2,"phase":"in_progress"}"#).unwrap();
        assert!(matches!(
            migration_start(&marker, true).unwrap_err(),
            MigrationError::UnsupportedMarker { version: 2, .. }
        ));
        std::fs::write(&marker, r#"{"version":1,"phase":"unknown"}"#).unwrap();
        assert!(matches!(
            migration_start(&marker, true).unwrap_err(),
            MigrationError::UnsupportedMarker { version: 1, .. }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_secret_migration_never_overwrites_a_conflicting_canonical_key() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-secret-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let home = Home::from_root(&root);
        let backend = Arc::new(any_converter_server::secrets::MemoryKeychain::new());
        let keys = any_converter_server::secrets::ApiKeyStore::for_home_with_backend(
            &home,
            backend.clone(),
        );
        let legacy = any_converter_server::config::ServerConfig::from_toml(
            r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-legacy"
"#,
        )
        .unwrap();
        keys.set_provider("openai", "sk-canonical").unwrap();

        let error = migrate_provider_keys(&legacy, &keys).unwrap_err();
        assert!(matches!(
            error,
            MigrationError::ServerSecret(any_converter_server::secrets::SecretError::Conflict(_))
        ));
        assert_eq!(keys.get_provider("openai").unwrap(), "sk-canonical");

        keys.set_provider("openai", "sk-legacy").unwrap();
        migrate_provider_keys(&legacy, &keys).unwrap();

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retired_default_port_moves_to_current_default() {
        assert_eq!(normalize_legacy_port(1228), DEFAULT_PORT);
        assert_eq!(normalize_legacy_port(9090), 9090);
    }

    #[test]
    fn legacy_patch_remaps_retired_default_but_keeps_explicit_ports() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-{}-port-patch",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let home = Home::from_root(&root);

        let retired = any_converter_server::config::ServerConfig::from_toml(
            "[server]\nhost = \"127.0.0.1\"\nport = 1228\n",
        )
        .unwrap();
        assert_eq!(
            legacy_patch(&home, &retired).server.unwrap().port,
            Some(DEFAULT_PORT)
        );

        let explicit = any_converter_server::config::ServerConfig::from_toml(
            "[server]\nhost = \"127.0.0.1\"\nport = 9090\n",
        )
        .unwrap();
        assert_eq!(
            legacy_patch(&home, &explicit).server.unwrap().port,
            Some(9090)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn completed_migration_repairs_a_retired_port_and_is_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "any-converter-migration-{}-port-repair",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.toml"),
            "[server]\nhost = \"127.0.0.1\"\nport = 1228\n",
        )
        .unwrap();
        let home = Home::from_root(&root);

        let keys = ApiKeyStore::for_home_with_backend(
            &home,
            Arc::new(any_converter_server::secrets::MemoryKeychain::new()),
        );
        repair_retired_default_port(&home, keys).unwrap();
        let repaired = ConfigStore::from_home(&home).load().unwrap();
        assert_eq!(repaired.config().server.port, DEFAULT_PORT);

        // The second pass reads the repaired file and must not rewrite it.
        let revisions = repaired.revision().clone();
        let keys = ApiKeyStore::for_home_with_backend(
            &home,
            Arc::new(any_converter_server::secrets::MemoryKeychain::new()),
        );
        repair_retired_default_port(&home, keys).unwrap();
        let unchanged = ConfigStore::from_home(&home).load().unwrap();
        assert_eq!(unchanged.revision(), &revisions);
        assert_eq!(unchanged.config().server.port, DEFAULT_PORT);

        let _ = std::fs::remove_dir_all(root);
    }
}
