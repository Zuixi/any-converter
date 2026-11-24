//! WorkBuddy and WorkBuddy AI specifications.
//!
//! Both products read `models.json` from their configuration directory. The
//! file is usually wrapped as `{"models": [...], "availableModels": [...]}`,
//! but a bare JSON array root is also valid. Each model names its vendor,
//! endpoint, key, limits, and reasoning capabilities. Wiring appends one entry
//! per catalog model with `vendor: "any-converter"`, which is also what tells
//! our entries apart on disconnect; an entry whose `apiKey` is ours is treated
//! the same way.
//!
//! WorkBuddy honours `$WORKBUDDY_CONFIG_DIR`; WorkBuddy AI has no override and
//! always uses `~/.workbuddy-ai`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    join_relative, read_config, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the WorkBuddy specs.
pub const OUR_KEYS: &[&str] = &["models", "availableModels"];

const PROVIDER: &str = "any-converter";
const MODELS: &str = "models";
const AVAILABLE: &str = "availableModels";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 200_000;
/// Reasoning levels WorkBuddy accepts per model.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// One WorkBuddy flavour's layout.
#[derive(Debug, Clone, Copy)]
pub struct WorkbuddySpec {
    id: &'static str,
    env_var: Option<&'static str>,
    default_dir: &'static str,
}

impl WorkbuddySpec {
    fn dir(&self, ctx: &HomeCtx) -> PathBuf {
        let default_dir = self.default_dir;
        match self.env_var {
            Some(variable) => env_dir(ctx, variable, |ctx| join_relative(&ctx.home, default_dir)),
            None => join_relative(&ctx.home, default_dir),
        }
    }

    fn models_path(&self, ctx: &HomeCtx) -> PathBuf {
        self.dir(ctx).join("models.json")
    }
}

impl AgentSpec for WorkbuddySpec {
    fn id(&self) -> &'static str {
        self.id
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = self.dir(ctx);
        if config_dir.is_dir() || self.models_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![self.models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.models_path(ctx);
        let current = read_config(&path)?.unwrap_or_default();

        if let Some(Value::Array(items)) = root_value(&current) {
            let mut entries: Vec<Value> = items
                .into_iter()
                .filter(|entry| !is_ours(entry, Some(wiring.agent_key)))
                .collect();
            entries.extend(
                wiring
                    .catalog
                    .models
                    .iter()
                    .map(|model| entry(wiring, model)),
            );
            let text = write_array(&entries)?;
            let written = write_config(&path, &text)?;
            return Ok(vec![written]);
        }

        let mut entries = user_models(&current, Some(wiring.agent_key));
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
        entries.extend(our_entries);

        let mut sets = vec![KV::new(MODELS, Value::Array(entries))];
        if let Some(Value::Array(mut ids)) = value_at(&current, AVAILABLE) {
            // A non-empty list is a whitelist; without our ids the new models
            // are hidden even though they exist.
            if !ids.is_empty() {
                for id in &our_ids {
                    if !ids.iter().any(|seen| seen.as_str() == Some(id)) {
                        ids.push(json!(id));
                    }
                }
                sets.push(KV::new(AVAILABLE, Value::Array(ids)));
            }
        }

        let text = json::set(&current, &sets)?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.models_path(ctx);
        let mut written = Vec::new();
        let Some(current) = read_config(&path)? else {
            return Ok(written);
        };

        if let Some(Value::Array(items)) = root_value(&current) {
            let key = our_key(&items);
            let kept: Vec<Value> = items
                .into_iter()
                .filter(|entry| !is_ours(entry, key.as_deref()))
                .collect();
            let text = write_array(&kept)?;
            written.push(write_config(&path, &text)?);
            return Ok(written);
        }

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
        written.push(write_config(&path, &text)?);
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&self.models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let entries = models(&current);
        let key = our_key(&entries);
        let ours: Vec<Value> = entries
            .into_iter()
            .filter(|entry| is_ours(entry, key.as_deref()))
            .collect();
        if ours.is_empty() {
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
        if base_ok && key_ok {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&self.models_path(ctx)) {
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
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// WorkBuddy (`$WORKBUDDY_CONFIG_DIR` or `~/.workbuddy`).
pub fn workbuddy() -> WorkbuddySpec {
    WorkbuddySpec {
        id: "workbuddy",
        env_var: Some("WORKBUDDY_CONFIG_DIR"),
        default_dir: ".workbuddy",
    }
}

/// WorkBuddy AI (`~/.workbuddy-ai`), which has no environment override.
pub fn workbuddy_ai() -> WorkbuddySpec {
    WorkbuddySpec {
        id: "workbuddy-ai",
        env_var: None,
        default_dir: ".workbuddy-ai",
    }
}

/// One model entry as WorkBuddy reads it.
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
    // Only reasoning models carry an effort block.
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
