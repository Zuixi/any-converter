//! Connect / disconnect orchestration.
//!
//! This module owns recovery and transaction boundaries; it never contains
//! agent-specific key knowledge. A connect takes an idempotent byte snapshot
//! before calling the spec, records the values the spec wrote for drift
//! detection, and restores already-written files on a mid-connect failure.
//!
//! Disconnect is deliberately three-step: the spec first performs semantic
//! extraction (removing our keys and restoring stashed values), then every
//! covered file is compared against its pristine backup. A semantically equal
//! result is byte-restored from the backup so user formatting and comments
//! return exactly. A genuinely changed file is kept as-is and its backup is
//! retained so recovery data is never lost.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::Document;

use crate::atomic;
use crate::backup::{Applied, Snapshot, Stash};
use crate::edit::{env as env_edit, json};
use crate::error::WiringError;
use crate::spec::{AgentSpec, HomeCtx, StashScope, WiringCtx};

/// Result of a successful connect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectReport {
    /// Files written by the agent specification.
    pub files_written: Vec<PathBuf>,
}

/// Result of a disconnect, including degraded-restore information.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisconnectReport {
    /// Files written (or removed) during semantic extraction.
    pub files_written: Vec<PathBuf>,
    /// Files whose backup was retained because user changes were detected.
    pub kept_backup: Vec<PathBuf>,
    /// Non-fatal warnings such as `stash_missing`.
    pub warnings: Vec<String>,
}

/// Recovery store locations for all agents.
#[derive(Debug, Clone)]
pub struct Wiring {
    data_dir: PathBuf,
}

impl Wiring {
    /// Creates a wiring orchestrator whose recovery files live under `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Returns the recovery data directory.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn stash_file(&self) -> PathBuf {
        self.data_dir.join("agent_stash.json")
    }

    fn applied_file(&self) -> PathBuf {
        self.data_dir.join("agent_applied.json")
    }

    /// Snapshots, connects, and records the applied values for `spec`.
    ///
    /// A snapshot failure aborts before any agent file is touched. A failure
    /// during the spec's own per-file writes triggers a best-effort byte restore
    /// of every covered file that no longer matches its snapshot.
    pub fn connect(
        &self,
        spec: &dyn AgentSpec,
        home: &HomeCtx,
        wiring: &WiringCtx<'_>,
    ) -> Result<ConnectReport, WiringError> {
        let files = spec.config_files(home);
        let snapshot = Snapshot::new(files.clone());
        snapshot.take()?;

        let _scope = StashScope::enter(self.stash_file());
        match spec.connect(home, wiring) {
            Ok(files_written) => {
                let recorded: Vec<(String, String)> =
                    spec.applied_values(home).into_iter().collect();
                let mut applied = Applied::load(&self.applied_file());
                applied.record(spec.id(), &recorded);
                applied.save(&self.applied_file())?;
                Ok(ConnectReport { files_written })
            }
            Err(error) => {
                self.restore_after_failed_connect(&snapshot, &files);
                Err(error)
            }
        }
    }

    /// Extracts our keys and performs the three-step restore decision.
    pub fn disconnect(
        &self,
        spec: &dyn AgentSpec,
        home: &HomeCtx,
    ) -> Result<DisconnectReport, WiringError> {
        let files = spec.config_files(home);
        let snapshot = Snapshot::new(files.clone());
        let stash_before = Stash::load(&self.stash_file()).old_values(spec.id());
        let _scope = StashScope::enter(self.stash_file());

        let files_written = spec.disconnect(home)?;

        let mut warnings = Vec::new();
        if stash_before.is_empty() {
            warnings.push("stash_missing".to_owned());
        }
        let mut kept_backup = Vec::new();

        for file in &files {
            let backup = match snapshot.read_backup(file) {
                Ok(backup) => backup,
                Err(_) => {
                    snapshot.preserve_bad(file);
                    warnings.push("backup_corrupt".to_owned());
                    continue;
                }
            };
            let current = atomic::read(file)?;

            match (backup, current) {
                (Some(backup_bytes), Some(current_bytes)) => {
                    if current_bytes == backup_bytes {
                        snapshot.clear(file);
                    } else if semantically_equal(file, &current_bytes, &backup_bytes) {
                        atomic::write_atomic(file, &backup_bytes)?;
                        snapshot.clear(file);
                    } else {
                        kept_backup.push(file.clone());
                    }
                }
                (Some(_), None) => {
                    // Extraction removed a file that existed before connect.
                    kept_backup.push(file.clone());
                }
                (None, Some(current_bytes)) => {
                    if !has_absent_marker(file) {
                        continue;
                    }
                    if is_empty_document(file, &current_bytes) {
                        remove_if_exists(file)?;
                        snapshot.clear(file);
                    } else {
                        kept_backup.push(file.clone());
                    }
                }
                (None, None) => {
                    if has_absent_marker(file) {
                        snapshot.clear(file);
                    }
                }
            }
        }

        if !kept_backup.is_empty() {
            warnings.push("kept_backup".to_owned());
        }

        let mut stash = Stash::load(&self.stash_file());
        stash.take_namespace(spec.id());
        stash.save(&self.stash_file())?;

        let mut applied = Applied::load(&self.applied_file());
        applied.forget(spec.id());
        applied.save(&self.applied_file())?;

        Ok(DisconnectReport {
            files_written,
            kept_backup,
            warnings,
        })
    }

    /// Restores files after a failed connect (best effort).
    fn restore_after_failed_connect(&self, snapshot: &Snapshot, files: &[PathBuf]) {
        for file in files {
            let Ok(backup) = snapshot.read_backup(file) else {
                continue;
            };
            let Ok(current) = atomic::read(file) else {
                continue;
            };
            match (backup, current) {
                (Some(backup_bytes), Some(current_bytes)) => {
                    if backup_bytes != current_bytes {
                        let _ = atomic::write_atomic(file, &backup_bytes);
                    }
                }
                (Some(backup_bytes), None) => {
                    let _ = atomic::write_atomic(file, &backup_bytes);
                }
                (None, Some(_)) => {
                    let _ = fs::remove_file(file);
                }
                (None, None) => {}
            }
        }
    }
}

fn has_absent_marker(file: &Path) -> bool {
    PathBuf::from(format!("{}.before-any-converter.absent", file.display())).exists()
}

fn remove_if_exists(path: &Path) -> Result<(), WiringError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WiringError::Io(error)),
    }
}

fn semantically_equal(path: &Path, left: &[u8], right: &[u8]) -> bool {
    if left == right {
        return true;
    }
    let (Ok(left), Ok(right)) = (std::str::from_utf8(left), std::str::from_utf8(right)) else {
        return false;
    };
    match extension(path) {
        "json" => normalize_json(left) == normalize_json(right),
        "toml" => normalize_toml(left) == normalize_toml(right),
        _ if is_dotenv(path) => normalize_env(left) == normalize_env(right),
        _ => false,
    }
}

fn is_empty_document(path: &Path, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    match extension(path) {
        "json" => text.trim().is_empty() || is_empty_json(text),
        "toml" => {
            text.trim().is_empty()
                || text
                    .trim_start_matches('\u{feff}')
                    .parse::<Document>()
                    .map(|document| document.as_table().is_empty())
                    .unwrap_or(false)
        }
        _ if is_dotenv(path) => text.trim().is_empty(),
        _ => text.trim().is_empty(),
    }
}

fn extension(path: &Path) -> &str {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
}

fn is_dotenv(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name == ".env")
}

fn normalize_json(text: &str) -> Option<String> {
    json::get(text, "")
}

fn is_empty_json(text: &str) -> bool {
    let Some(normalized) = normalize_json(text) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&normalized) else {
        return false;
    };
    empty_json_value(&value)
}

fn empty_json_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.values().all(empty_json_value),
        serde_json::Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn normalize_toml(text: &str) -> Option<String> {
    let body = text.trim_start_matches('\u{feff}');
    body.parse::<Document>()
        .ok()
        .map(|document| document.to_string().replace("\r\n", "\n").trim().to_owned())
}

fn normalize_env(text: &str) -> BTreeMap<String, String> {
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty() {
            keys.push(key.to_owned());
        }
    }
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter_map(|key| env_edit::get(text, &key).map(|value| (key, value)))
        .collect()
}
