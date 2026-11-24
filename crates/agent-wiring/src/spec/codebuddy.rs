//! CodeBuddy specification (`$CODEBUDDY_CONFIG_DIR` or `~/.codebuddy`).
//!
//! CodeBuddy uses the same `models.json` shape as WorkBuddy
//! (`{"models": [...], "availableModels": [...]}`, or a bare array root), plus
//! a `settings.json` whose top-level `model` selects the model a session
//! starts on. Wiring appends our model entries and pins that selection,
//! restoring the previous value on disconnect.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the CodeBuddy spec.
pub const OUR_KEYS: &[&str] = &["models", "availableModels", "settings.model"];

const PROVIDER: &str = "any-converter";
const MODELS: &str = "models";
const AVAILABLE: &str = "availableModels";
const SETTINGS_MODEL: &str = "model";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 200_000;
/// Reasoning levels CodeBuddy accepts per model.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// CodeBuddy wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodeBuddySpec;

impl CodeBuddySpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "CODEBUDDY_CONFIG_DIR", |ctx| {
            ctx.home.join(".codebuddy")
        })
    }

    fn models_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("models.json")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for CodeBuddySpec {
    fn id(&self) -> &'static str {
        "codebuddy"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::models_path(ctx).exists()
            || Self::settings_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::models_path(ctx), Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let models_path = Self::models_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let models_text = read_config(&models_path)?.unwrap_or_default();
        let settings = read_config(&settings_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        if let Some(value) = json::get(&settings, SETTINGS_MODEL) {
            old.insert(SETTINGS_MODEL.to_owned(), value);
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let our_entries: Vec<Value> = wiring
            .catalog
            .models
            .iter()
            .map(|model| entry(wiring, model))
            .collect();
        let our_ids: Vec<String> = our_entries
            .iter()
            .filter_map(id_of)
            .map(str::to_owned)
            .collect();

        let next_models = if let Some(Value::Array(items)) = root_value(&models_text) {
            let mut entries: Vec<Value> = items
                .into_iter()
                .filter(|entry| !is_ours(entry, Some(wiring.agent_key)))
                .collect();
            entries.extend(our_entries);
            write_array(&entries)?
        } else {
            let mut entries = user_models(&models_text, Some(wiring.agent_key));
            entries.extend(our_entries);
            let mut sets = vec![KV::new(MODELS, Value::Array(entries))];
            if let Some(Value::Array(mut ids)) = value_at(&models_text, AVAILABLE) {
                if !ids.is_empty() {
                    for id in &our_ids {
                        if !ids.iter().any(|seen| seen.as_str() == Some(id)) {
                            ids.push(json!(id));
                        }
                    }
                    sets.push(KV::new(AVAILABLE, Value::Array(ids)));
                }
            }
            json::set(&models_text, &sets)?
        };

        let next_settings = json::set(
            &settings,
            &[KV::new(SETTINGS_MODEL, json!(wiring.default_model))],
        )?;

        let written_models = write_config(&models_path, &next_models)?;
        let written_settings = write_config(&settings_path, &next_settings)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_models, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let models_path = Self::models_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&models_path)? {
            let text = if let Some(Value::Array(items)) = root_value(&current) {
                let key = our_key(&items);
                let kept: Vec<Value> = items
                    .into_iter()
                    .filter(|entry| !is_ours(entry, key.as_deref()))
                    .collect();
                write_array(&kept)?
            } else {
                let entries = models(&current);
                let key = our_key(&entries);
                let ours: Vec<String> = entries
                    .iter()
                    .filter(|entry| is_ours(entry, key.as_deref()))
                    .filter_map(id_of)
                    .map(str::to_owned)
                    .collect();
                let kept: Vec<Value> = entries
                    .into_iter()
                    .filter(|entry| !is_ours(entry, key.as_deref()))
                    .collect();

                let mut sets = Vec::new();
                let mut deletes = Vec::new();
                if kept.is_empty() {
                    deletes.push(MODELS);
                } else {
                    sets.push(KV::new(MODELS, Value::Array(kept)));
                }
                if let Some(Value::Array(mut ids)) = value_at(&current, AVAILABLE) {
                    ids.retain(|id| {
                        !id.as_str()
                            .is_some_and(|name| ours.iter().any(|model| model == name))
                    });
                    if ids.is_empty() {
                        deletes.push(AVAILABLE);
                    } else {
                        sets.push(KV::new(AVAILABLE, Value::Array(ids)));
                    }
                }

                let mut text = json::set(&current, &sets)?;
                text = json::del(&text, &deletes)?;
                text
            };
            written.push(write_config(&models_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let text = match old.get(SETTINGS_MODEL) {
                Some(value) if !value.is_empty() => {
                    json::set(&current, &[KV::new(SETTINGS_MODEL, json!(value))])?
                }
                _ => json::del(&current, &[SETTINGS_MODEL])?,
            };
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let entries = models(&current);
        let key = our_key(&entries);
        let ours: Vec<Value> = entries
            .into_iter()
            .filter(|entry| is_ours(entry, key.as_deref()))
            .collect();
        let selected = read_config(&Self::settings_path(ctx))
            .ok()
            .flatten()
            .and_then(|settings| json::get(&settings, SETTINGS_MODEL))
            .is_some_and(|value| !value.is_empty());
        if ours.is_empty() && !selected {
            return WiredState::Disconnected;
        }
        let base_ok = ours.iter().all(|entry| {
            entry
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|value| gw_base_match(value, gateway_url))
        });
        let key_ok = ours.iter().all(|entry| {
            entry
                .get("apiKey")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        });
        if base_ok && key_ok && selected && !ours.is_empty() {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::models_path(ctx)) {
            let entries = models(&current);
            let key = our_key(&entries);
            for entry in entries
                .into_iter()
                .filter(|entry| is_ours(entry, key.as_deref()))
            {
                if let Some(id) = id_of(&entry) {
                    if let Some(url) = entry.get("url").and_then(Value::as_str) {
                        values.insert(format!("{MODELS}.{id}.url"), url.to_owned());
                    }
                }
            }
        }
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            if let Some(value) = json::get(&settings, SETTINGS_MODEL) {
                values.insert(format!("settings.{SETTINGS_MODEL}"), value);
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// One model entry as CodeBuddy reads it.
fn entry(wiring: &WiringCtx<'_>, model: &crate::sync::catalog::CatalogModel) -> Value {
    let mut entry = json!({
        "id": model.id,
        "name": model.name,
        "vendor": PROVIDER,
        "apiKey": wiring.agent_key,
        "url": gateway_path(wiring.gateway_url, "v1/chat/completions"),
        "maxInputTokens": model.context_tokens.unwrap_or(DEFAULT_CONTEXT),
        "supportsToolCall": true,
        "supportsImages": false,
        "supportsReasoning": model.reasoning,
    });
    if model.reasoning {
        entry["reasoning"] = json!({
            "supportedEfforts": EFFORTS,
            "canDisableThinking": true,
            "defaultEffort": wiring
                .effort
                .filter(|level| !level.is_empty())
                .unwrap_or("medium"),
        });
    }
    entry
}

/// The root JSON value, parsed through the format-preserving reader.
fn root_value(text: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, "")?).ok()
}

/// Writes entries back as the bare array the file uses.
fn write_array(entries: &[Value]) -> Result<String, WiringError> {
    let text = serde_json::to_string_pretty(&Value::Array(entries.to_vec()))
        .map_err(|error| WiringError::Refused(format!("cannot serialize models: {error}")))?;
    Ok(format!("{text}\n"))
}

/// All entries in the `models` array, accepting a bare-array root.
fn models(text: &str) -> Vec<Value> {
    match root_value(text) {
        Some(Value::Array(items)) => items,
        _ => match value_at(text, MODELS) {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
    }
}

/// Entries that this spec did not write.
fn user_models(text: &str, agent_key: Option<&str>) -> Vec<Value> {
    models(text)
        .into_iter()
        .filter(|entry| !is_ours(entry, agent_key))
        .collect()
}

/// Whether `entry` belongs to us, by vendor or by our own API key.
fn is_ours(entry: &Value, agent_key: Option<&str>) -> bool {
    if entry.get("vendor").and_then(Value::as_str) == Some(PROVIDER) {
        return true;
    }
    agent_key.is_some_and(|key| {
        !key.is_empty() && entry.get("apiKey").and_then(Value::as_str) == Some(key)
    })
}

/// Our API key as recorded in the file, used on disconnect where no wiring
/// context is available.
fn our_key(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry.get("vendor").and_then(Value::as_str) == Some(PROVIDER))
        .and_then(|entry| entry.get("apiKey").and_then(Value::as_str))
        .map(str::to_owned)
}

fn id_of(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}
