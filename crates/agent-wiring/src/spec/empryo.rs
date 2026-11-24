//! Empryo specification (`~/.empryo/config.json`).
//!
//! Empryo keeps providers in an array. Our entry is identified by its
//! `id` (`any-converter`); entries written by this spec are removed and
//! re-appended on every connect so the user's own providers keep their place.
//! `defaultModel` selects the model as `<provider>/<model>`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, remove_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Empryo spec.
pub const OUR_KEYS: &[&str] = &["providers", "defaultModel"];

const PROVIDER: &str = super::support::PROVIDER;
const CONFIG_FILE: &str = "config.json";
const PROVIDERS: &str = "providers";
const DEFAULT_MODEL: &str = "defaultModel";
/// Stash key marking a config file this spec created from nothing.
const CREATED_KEY: &str = "__created";

/// Empryo wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmpryoSpec;

impl EmpryoSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".empryo")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(CONFIG_FILE)
    }
}

impl AgentSpec for EmpryoSpec {
    fn id(&self) -> &'static str {
        "empryo"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::config_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let current = read_config(&path)?;
        let created = current.is_none();
        let config = current.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&config, &[DEFAULT_MODEL]),
        );
        if created {
            let mut marker = BTreeMap::new();
            marker.insert(CREATED_KEY.to_owned(), "true".to_owned());
            stash.record(self.id(), marker);
        }

        let mut providers = read_array(&config, PROVIDERS)?;
        providers.retain(|entry| entry_id(entry) != Some(PROVIDER));
        providers.push(json!({
            "id": PROVIDER,
            "name": PROVIDER,
            "baseURL": gateway_path(wiring.gateway_url, "v1"),
            "modelsAPI": gateway_path(wiring.gateway_url, "v1/models"),
        }));

        let text = json::set(
            &config,
            &[
                KV::new(PROVIDERS, Value::Array(providers)),
                KV::new(
                    DEFAULT_MODEL,
                    json!(format!("{PROVIDER}/{}", wiring.default_model)),
                ),
            ],
        )?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let created = old.contains_key(CREATED_KEY);
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let kept: Vec<Value> = read_array(&current, PROVIDERS)?
                .into_iter()
                .filter(|entry| entry_id(entry) != Some(PROVIDER))
                .collect();
            let mut text = if kept.is_empty() {
                json::del_pruning(&current, &[PROVIDERS])?
            } else {
                json::set(&current, &[KV::new(PROVIDERS, Value::Array(kept))])?
            };
            text = super::support::restore_keys(&text, &old, &[DEFAULT_MODEL])?;

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
        let Ok(Some(config)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let ours = ours(&config);
        let selected = json::get(&config, DEFAULT_MODEL);

        let prefix = format!("{PROVIDER}/");
        let selected = selected
            .as_deref()
            .is_some_and(|value| value.starts_with(&prefix));
        if ours.is_empty() && !selected {
            return WiredState::Disconnected;
        }

        let gateway = ours
            .iter()
            .filter_map(entry_base)
            .any(|value| gw_base_match(value, gateway_url));
        if gateway && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            if let Some(value) = json::get(&config, DEFAULT_MODEL) {
                values.insert(DEFAULT_MODEL.to_owned(), value);
            }
            if let Some(value) = json::get(&config, PROVIDERS) {
                values.insert(PROVIDERS.to_owned(), value);
            }
        }
        values
    }
}

/// Reads `path` as an array, refusing to clobber a non-array value.
fn read_array(text: &str, path: &str) -> Result<Vec<Value>, WiringError> {
    let Some(raw) = json::get(text, path) else {
        return Ok(Vec::new());
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(WiringError::Refused(format!("{path} is not an array"))),
        Err(error) => Err(WiringError::Refused(format!(
            "{path} is not valid JSON: {error}"
        ))),
    }
}

/// The provider entries this spec wrote.
fn ours(text: &str) -> Vec<Value> {
    read_array(text, PROVIDERS)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry_id(entry) == Some(PROVIDER))
        .collect()
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id")?.as_str()
}

fn entry_base(entry: &Value) -> Option<&str> {
    entry.get("baseURL")?.as_str()
}

fn is_empty_object(text: &str) -> bool {
    json::get(text, "").as_deref() == Some("{}")
}
