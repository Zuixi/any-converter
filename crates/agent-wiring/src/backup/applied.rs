//! Audit of the last values we wrote into an agent configuration.
//!
//! This store is only used for drift detection: losing it costs visibility, not
//! the ability to restore pristine bytes. It is one JSON file whose path is
//! supplied by the caller.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{BackupError, move_aside, now_unix};
use crate::atomic;

const VERSION: u32 = 1;

const fn version() -> u32 {
    VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    value: String,
    at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AppliedFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(flatten)]
    agents: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// Values written by us during the last connect or sync, per agent.
#[derive(Debug, Default)]
pub struct Applied {
    agents: BTreeMap<String, BTreeMap<String, Entry>>,
}

impl Applied {
    /// Loads the audit from `path`; missing or corrupt data yields an empty audit.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        let Ok(file) = serde_json::from_slice::<AppliedFile>(&bytes) else {
            return Self::default();
        };
        Self {
            agents: file.agents,
        }
    }

    /// Records the values we just wrote for `agent`, stamping all of them with
    /// the current Unix time. An empty value removes the key from the audit.
    pub fn record(&mut self, agent: &str, kvs: &[(String, String)]) {
        let at = now_unix();
        let namespace = self.agents.entry(agent.to_owned()).or_default();
        for (key, value) in kvs {
            if value.is_empty() {
                namespace.remove(key);
            } else {
                namespace.insert(
                    key.clone(),
                    Entry {
                        value: value.clone(),
                        at,
                    },
                );
            }
        }
    }

    /// Drops all recorded values for `agent`.
    pub fn forget(&mut self, agent: &str) {
        self.agents.remove(agent);
    }

    /// Returns the values we recorded for `agent`.
    pub fn recorded(&self, agent: &str) -> BTreeMap<String, String> {
        self.agents
            .get(agent)
            .map(|namespace| {
                namespace
                    .iter()
                    .map(|(key, entry)| (key.clone(), entry.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns recorded keys whose current value differs from ours or vanished.
    pub fn drifted_keys(
        &self,
        agent: &str,
        current: &BTreeMap<String, String>,
    ) -> Vec<String> {
        let Some(namespace) = self.agents.get(agent) else {
            return Vec::new();
        };
        let mut drifted = Vec::new();
        for (key, entry) in namespace {
            if current.get(key) != Some(&entry.value) {
                drifted.push(key.clone());
            }
        }
        drifted
    }

    /// Atomically writes the audit to `path`.
    ///
    /// If `path` exists but does not parse, it is moved aside first using the
    /// `.bad-<unix_ts>` convention.
    pub fn save(&self, path: &Path) -> Result<(), BackupError> {
        if let Ok(bytes) = fs::read(path) {
            if serde_json::from_slice::<AppliedFile>(&bytes).is_err() {
                move_aside(path)?;
            }
        }
        let file = AppliedFile {
            version: VERSION,
            agents: self.agents.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(|error| BackupError::Corrupt(format!("cannot serialize audit: {error}")))?;
        bytes.push(b'\n');
        atomic::write_atomic(path, &bytes)?;
        Ok(())
    }
}
