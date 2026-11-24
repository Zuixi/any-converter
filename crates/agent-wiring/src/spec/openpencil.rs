//! OpenPencil specification (`<app data>/openpencil/settings.json`).
//!
//! OpenPencil keeps built-in agent presets in a `builtin_agents` array. Wiring
//! appends one custom OpenAI-compatible preset named `any-converter` and
//! removes it again on disconnect. When the file only ever held the version
//! marker and that array, disconnect deletes the file rather than leaving an
//! empty document behind.
//!
//! Files with an unsupported `version` (anything other than `1`) or a
//! `builtin_agents` value that is not an array are refused untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, app_data_dir, gateway_path,
    gw_base_match, read_config, remove_config, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenPencil spec.
pub const OUR_KEYS: &[&str] = &["version", "builtin_agents"];

const PROVIDER: &str = "any-converter";
const AGENTS: &str = "builtin_agents";
const VERSION: &str = "version";
const ID: &str = "id";
/// The only settings version this spec understands.
const SUPPORTED_VERSION: &str = "1";
/// Upper bound on the model list written into our preset.
const MAX_MODELS: usize = 64;

/// OpenPencil wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenPencilSpec;

impl OpenPencilSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        app_data_dir(ctx).join("openpencil")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for OpenPencilSpec {
    fn id(&self) -> &'static str {
        "openpencil"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::settings_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let current = read_config(&path)?.unwrap_or_default();
        validate(&current)?;

        let existing = our_entry(&current);
        let mut entries = user_agents(&current);
        entries.push(merge_agent(agent(wiring), existing.as_ref()));
        let text = json::set(
            &current,
            &[
                KV::new(VERSION, json!(1)),
                KV::new(AGENTS, Value::Array(entries)),
            ],
        )?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let mut written = Vec::new();
        let Some(current) = read_config(&path)? else {
            return Ok(written);
        };
        validate(&current)?;

        let kept = user_agents(&current);
        if kept.is_empty() && has_no_user_data(&current) {
            remove_config(&path)?;
            written.push(path);
            return Ok(written);
        }

        let text = json::set(&current, &[KV::new(AGENTS, Value::Array(kept))])?;
        written.push(write_config(&path, &text)?);
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let Some(entry) = our_entry(&current) else {
            return WiredState::Disconnected;
        };
        let base = entry.get("base_url").and_then(Value::as_str);
        let key = entry.get("api_key").and_then(Value::as_str);
        let gateway = base.is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) {
            if let Some(entry) = our_entry(&current) {
                for key in ["base_url", "api_key"] {
                    if let Some(value) = entry.get(key).and_then(Value::as_str) {
                        values.insert(format!("{AGENTS}.{PROVIDER}.{key}"), value.to_owned());
                    }
                }
            }
        }
        values
    }
}

/// Refuses files this spec cannot safely edit.
fn validate(text: &str) -> Result<(), WiringError> {
    if let Some(version) = json::get(text, VERSION) {
        if version != SUPPORTED_VERSION {
            return Err(WiringError::Refused(format!(
                "unsupported settings version: {version}"
            )));
        }
    }
    if let Some(raw) = json::get(text, AGENTS) {
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Array(_)) => {}
            Ok(_) => {
                return Err(WiringError::Refused(format!("{AGENTS} is not an array")));
            }
            Err(error) => {
                return Err(WiringError::Refused(format!(
                    "{AGENTS} is not valid JSON: {error}"
                )));
            }
        }
    }
    Ok(())
}

/// The preset OpenPencil reads.
fn agent(wiring: &WiringCtx<'_>) -> Value {
    let first = wiring
        .catalog
        .first_model()
        .map(|model| model.id.clone())
        .unwrap_or_else(|| wiring.default_model.to_owned());

    let mut models: Vec<String> = Vec::new();
    for model in &wiring.catalog.models {
        if !models.contains(&model.id) {
            models.push(model.id.clone());
        }
        if models.len() == MAX_MODELS {
            break;
        }
    }

    let mut entry = json!({
        "id": PROVIDER,
        "preset": "custom",
        "display_name": PROVIDER,
        "kind": "openai-compat",
        "api_key": wiring.agent_key,
        "model": first,
        "base_url": gateway_path(wiring.gateway_url, "v1"),
        "enabled": true,
    });
    // A single model adds nothing; OpenPencil falls back to `model`.
    if models.len() >= 2 {
        entry["models"] = json!(models);
    }
    entry
}

/// Keeps a pre-existing preset's display name and enabled flag.
fn merge_agent(mut entry: Value, existing: Option<&Value>) -> Value {
    let Some(existing) = existing.and_then(Value::as_object) else {
        return entry;
    };
    let Some(target) = entry.as_object_mut() else {
        return entry;
    };
    for key in ["display_name", "enabled"] {
        if let Some(value) = existing.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
    entry
}

/// Entries in `builtin_agents` that this spec did not write.
fn user_agents(text: &str) -> Vec<Value> {
    entries(text)
        .into_iter()
        .filter(|entry| entry.get(ID).and_then(Value::as_str) != Some(PROVIDER))
        .collect()
}

/// This spec's entry, when present.
fn our_entry(text: &str) -> Option<Value> {
    entries(text)
        .into_iter()
        .find(|entry| entry.get(ID).and_then(Value::as_str) == Some(PROVIDER))
}

fn entries(text: &str) -> Vec<Value> {
    match value_at(text, AGENTS) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

/// Whether the document carries no user data beyond the version and the
/// agent array.
///
/// Called after our entry is filtered out, so the array itself may still hold
/// our entry in `text`; the caller has already established that nothing else
/// remains in it.
fn has_no_user_data(text: &str) -> bool {
    let Some(Value::Object(document)) = value_at(text, "") else {
        return false;
    };
    document.keys().all(|key| key == VERSION || key == AGENTS)
        && document.get(AGENTS).is_none_or(Value::is_array)
}

/// Parses the value at `path`, returning `None` when it is missing or invalid.
fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}
