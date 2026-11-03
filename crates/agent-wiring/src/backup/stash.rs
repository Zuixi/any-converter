//! Pre-connect values, grouped into per-agent namespaces.
//!
//! The whole stash is one JSON file whose path is supplied by the caller (the
//! server decides where it lives). An empty value removes a key, which lets the
//! caller record "this key did not exist before connect".

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{BackupError, move_aside};
use crate::atomic;

const VERSION: u32 = 1;

const fn version() -> u32 {
    VERSION
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StashFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(flatten)]
    agents: BTreeMap<String, BTreeMap<String, String>>,
}

/// Old key/value pairs captured before a connect, per agent.
#[derive(Debug, Default)]
pub struct Stash {
    agents: BTreeMap<String, BTreeMap<String, String>>,
}

impl Stash {
    /// Loads a stash from `path`.
    ///
    /// A missing or corrupt file yields an empty stash. The corrupt file is not
    /// touched here; the next [`Stash::save`] moves it aside before writing.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        let Ok(file) = serde_json::from_slice::<StashFile>(&bytes) else {
            return Self::default();
        };
        Self {
            agents: file.agents,
        }
    }

    /// Returns a copy of the stored values for `agent`.
    pub fn old_values(&self, agent: &str) -> BTreeMap<String, String> {
        self.agents.get(agent).cloned().unwrap_or_default()
    }

    /// Records old values for `agent`.
    ///
    /// An empty value removes the key instead of storing an empty string.
    pub fn record(&mut self, agent: &str, kvs: BTreeMap<String, String>) {
        let namespace = self.agents.entry(agent.to_owned()).or_default();
        for (key, value) in kvs {
            if value.is_empty() {
                namespace.remove(&key);
            } else {
                namespace.insert(key, value);
            }
        }
    }

    /// Returns and removes the namespace for `agent`.
    pub fn take_namespace(&mut self, agent: &str) -> BTreeMap<String, String> {
        self.agents.remove(agent).unwrap_or_default()
    }

    /// Atomically writes the stash to `path`.
    ///
    /// If `path` exists but does not parse, it is first moved aside using the
    /// `.bad-<unix_ts>` convention, so recovery bytes are never overwritten.
    pub fn save(&self, path: &Path) -> Result<(), BackupError> {
        if let Ok(bytes) = fs::read(path) {
            if serde_json::from_slice::<StashFile>(&bytes).is_err() {
                move_aside(path)?;
            }
        }
        let file = StashFile {
            version: VERSION,
            agents: self.agents.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(|error| BackupError::Corrupt(format!("cannot serialize stash: {error}")))?;
        bytes.push(b'\n');
        atomic::write_atomic(path, &bytes)?;
        Ok(())
    }
}
