//! Helpers shared by the family specs.
//!
//! The wiring shape repeats across agents that historically copied one
//! another (Pi and its forks, OpenCode and its forks): a provider block keyed
//! under the gateway's provider id, a default-model key, and a disconnect
//! that restores stashed values and prunes emptied containers. These helpers
//! keep each spec to its own paths and quirks.

use std::path::PathBuf;

use serde_json::Value;

use super::{DetectResult, WiringCtx, gateway_path};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// The provider id this gateway registers itself under in agent configs.
pub const PROVIDER: &str = "any-converter";

/// Detects the agent from the first path that exists.
pub fn detect_any(paths: &[PathBuf], config_dir: PathBuf) -> DetectResult {
    if paths.iter().any(|path| path.exists()) {
        DetectResult::Detected { config_dir }
    } else {
        DetectResult::Absent
    }
}

/// The gateway base URL agents expect: the root with `/v1`.
pub fn base_url(wiring: &WiringCtx<'_>) -> String {
    gateway_path(wiring.gateway_url, "v1")
}

/// Restores stashed values over `text` and deletes the keys that were absent.
///
/// `old` maps dotted key paths to their pre-connect JSON (as compact JSON
/// strings, the shape [`json::get`] returns). Values that were present are
/// written back; keys whose stashed value is empty are deleted. Containers
/// the deletes emptied are pruned.
pub fn restore_keys(
    text: &str,
    old: &std::collections::BTreeMap<String, String>,
    keys: &[&str],
) -> Result<String, WiringError> {
    let mut sets = Vec::new();
    let mut deletes = Vec::new();
    for key in keys {
        match old.get(*key) {
            Some(value) if !value.is_empty() => {
                sets.push(KV::new((*key).to_owned(), serde_json::json!(value)))
            }
            _ => deletes.push(*key),
        }
    }
    let mut restored = json::set(text, &sets)?;
    restored = json::del_pruning(&restored, &deletes)?;
    Ok(restored)
}

/// Collects the pre-connect values of `keys` that are currently present.
pub fn record_keys(text: &str, keys: &[&str]) -> std::collections::BTreeMap<String, String> {
    keys.iter()
        .filter_map(|key| json::get(text, key).map(|value| ((*key).to_owned(), value)))
        .collect()
}

/// Builds one JSON array entry per catalog model with `entry`.
pub fn catalog_models(
    wiring: &WiringCtx<'_>,
    entry: impl Fn(&crate::sync::catalog::CatalogModel) -> Value,
) -> Vec<Value> {
    wiring.catalog.models.iter().map(entry).collect()
}
