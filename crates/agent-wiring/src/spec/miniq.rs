//! MinIQ specification (`settings.json` in the MinIQ data directory).
//!
//! The data directory is `$MINIQ_DATA_DIR` when set, `%LOCALAPPDATA%/miniq`
//! when that variable is set, and `~/.local/share/miniq` otherwise. A
//! `miniq-data-dir.json` pointer beside the default directory with a
//! non-empty `data_dir` relocates the directory. The file holds one top-level
//! `provider` object; the whole object is stashed before connect and restored
//! byte-for-byte on disconnect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, remove_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the MinIQ spec.
pub const OUR_KEYS: &[&str] = &["provider"];

const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_KEY: &str = "provider";
/// Pointer file consulted beside the default data directory.
const POINTER_FILE: &str = "miniq-data-dir.json";
/// Stash key marking a settings file this spec created from nothing.
const CREATED_KEY: &str = "__created";

/// MinIQ wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MiniqSpec;

impl MiniqSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("MINIQ_DATA_DIR") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        // Magpie takes %LOCALAPPDATA%/miniq when that variable is set and
        // falls back to ~/.local/share/miniq otherwise.
        let default = match (ctx.env)("LOCALAPPDATA") {
            Some(value) if !value.trim().is_empty() => PathBuf::from(value.trim()).join("miniq"),
            _ => ctx.home.join(".local").join("share").join("miniq"),
        };
        pointer_dir(&default).unwrap_or(default)
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }
}

/// Reads the data-directory pointer stored beside the default directory.
fn pointer_dir(default: &Path) -> Option<PathBuf> {
    let text = read_config(&default.with_file_name(POINTER_FILE))
        .ok()
        .flatten()?;
    let value = json::get(&text, "data_dir")?;
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

impl AgentSpec for MiniqSpec {
    fn id(&self) -> &'static str {
        "miniq"
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
        let current = read_config(&path)?;
        let created = current.is_none();
        let settings = current.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[PROVIDER_KEY]),
        );
        if created {
            let mut marker = BTreeMap::new();
            marker.insert(CREATED_KEY.to_owned(), "true".to_owned());
            stash.record(self.id(), marker);
        }

        let text = json::set(
            &settings,
            &[KV::new(
                PROVIDER_KEY,
                json!({
                    "baseUrl": gateway_path(wiring.gateway_url, "v1"),
                    "apiKey": wiring.agent_key,
                    "model": wiring.default_model,
                    "apiProtocol": "chat_completions",
                }),
            )],
        )?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let created = old.contains_key(CREATED_KEY);
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let text = restore_provider(&current, &old)?;
            if created && is_empty_object(&text) {
                remove_config(&path)?;
                written.push(path);
            } else {
                written.push(write_config(&path, &text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let Some(base) = json::get(&settings, "provider.baseUrl") else {
            return WiredState::Disconnected;
        };
        if !gw_base_match(&base, gateway_url) {
            return WiredState::Disconnected;
        }

        let key = json::get(&settings, "provider.apiKey").is_some_and(|value| !value.is_empty());
        let model = json::get(&settings, "provider.model").is_some_and(|value| !value.is_empty());
        if key && model {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in ["baseUrl", "apiKey", "model"] {
                let path = format!("{PROVIDER_KEY}.{key}");
                if let Some(value) = json::get(&settings, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }
}

/// Restores the whole prior provider object, or removes the key.
fn restore_provider(text: &str, old: &BTreeMap<String, String>) -> Result<String, WiringError> {
    match old.get(PROVIDER_KEY) {
        Some(raw) if !raw.is_empty() => {
            Ok(json::set(text, &[KV::new(PROVIDER_KEY, parse_value(raw))])?)
        }
        _ => Ok(json::del_pruning(text, &[PROVIDER_KEY])?),
    }
}

/// Parses a stashed compact JSON value, keeping plain strings intact.
fn parse_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn is_empty_object(text: &str) -> bool {
    json::get(text, "").as_deref() == Some("{}")
}
