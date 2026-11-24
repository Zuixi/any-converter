//! Craft Agents specification (`$CRAFT_CONFIG_DIR/config.json`).
//!
//! Craft Agents keeps LLM endpoints in an `llmConnections` array and names the
//! active one with the top-level `defaultLlmConnection`. Wiring appends a
//! keyless Pi-compatible connection named `any-converter`, selects it, and
//! restores the previous selection on disconnect.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Craft Agents spec.
pub const OUR_KEYS: &[&str] = &["llmConnections", "defaultLlmConnection"];

const SLUG: &str = "any-converter";
const CONNECTIONS: &str = "llmConnections";
const DEFAULT_CONNECTION: &str = "defaultLlmConnection";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;

/// Craft Agents wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CraftAgentsSpec;

impl CraftAgentsSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "CRAFT_CONFIG_DIR", |ctx| ctx.home.join(".craft-agent"))
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.json")
    }
}

impl AgentSpec for CraftAgentsSpec {
    fn id(&self) -> &'static str {
        "craft-agents"
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
        let current = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        if let Some(value) = json::get(&current, DEFAULT_CONNECTION) {
            old.insert(DEFAULT_CONNECTION.to_owned(), value);
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let existing = our_connection(&current);
        let mut entries = user_connections(&current);
        entries.push(merge_connection(connection(wiring), existing.as_ref()));
        let text = json::set(
            &current,
            &[
                KV::new(CONNECTIONS, Value::Array(entries)),
                KV::new(DEFAULT_CONNECTION, json!(SLUG)),
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
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let kept = user_connections(&current);
            let mut text = if kept.is_empty() {
                json::del(&current, &[CONNECTIONS])?
            } else {
                json::set(&current, &[KV::new(CONNECTIONS, Value::Array(kept))])?
            };
            text = match old.get(DEFAULT_CONNECTION) {
                Some(value) if !value.is_empty() => {
                    json::set(&text, &[KV::new(DEFAULT_CONNECTION, json!(value))])?
                }
                _ => json::del(&text, &[DEFAULT_CONNECTION])?,
            };
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let Some(entry) = our_connection(&current) else {
            return WiredState::Disconnected;
        };
        let base = entry.get("baseUrl").and_then(Value::as_str);
        let selected = json::get(&current, DEFAULT_CONNECTION).is_some_and(|value| value == SLUG);
        if base.is_some_and(|value| gw_base_match(value, gateway_url)) && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::config_path(ctx)) {
            if let Some(entry) = our_connection(&current) {
                if let Some(base) = entry.get("baseUrl").and_then(Value::as_str) {
                    values.insert(format!("{CONNECTIONS}.{SLUG}.baseUrl"), base.to_owned());
                }
            }
            if let Some(value) = json::get(&current, DEFAULT_CONNECTION) {
                values.insert(DEFAULT_CONNECTION.to_owned(), value);
            }
        }
        values
    }
}

/// The connection entry Craft Agents reads.
fn connection(wiring: &WiringCtx<'_>) -> Value {
    let mut models: Vec<Value> = wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            json!({
                "id": model.id,
                "name": model.name,
                "shortName": model.name,
                // Craft reads both fields even when empty; omitting them
                // breaks its model picker.
                "description": model.name,
                "provider": "pi",
                "contextWindow": model.context_tokens.unwrap_or(DEFAULT_CONTEXT),
                "supportsImages": false,
            })
        })
        .collect();

    // `defaultModel` must resolve against this list.
    if !wiring
        .catalog
        .models
        .iter()
        .any(|model| model.id == wiring.default_model)
    {
        models.push(json!({
            "id": wiring.default_model,
            "name": wiring.default_model,
            "shortName": wiring.default_model,
            "description": wiring.default_model,
            "provider": "pi",
            "contextWindow": DEFAULT_CONTEXT,
            "supportsImages": false,
        }));
    }

    json!({
        "slug": SLUG,
        "name": SLUG,
        "providerType": "pi_compat",
        "authType": "none",
        "baseUrl": gateway_path(wiring.gateway_url, "agent/craft-agents/v1"),
        "customEndpoint": { "api": "openai-completions" },
        "models": models,
        "defaultModel": wiring.default_model,
        "modelSelectionMode": "userDefined3Tier",
        "createdAt": now_millis(),
    })
}

/// Keeps a pre-existing connection's display name and creation time.
fn merge_connection(mut entry: Value, existing: Option<&Value>) -> Value {
    let Some(existing) = existing.and_then(Value::as_object) else {
        return entry;
    };
    let Some(target) = entry.as_object_mut() else {
        return entry;
    };
    for key in ["name", "createdAt"] {
        if let Some(value) = existing.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
    entry
}

/// Entries in `llmConnections` that this spec did not write.
fn user_connections(text: &str) -> Vec<Value> {
    entries(text)
        .into_iter()
        .filter(|entry| entry.get("slug").and_then(Value::as_str) != Some(SLUG))
        .collect()
}

fn our_connection(text: &str) -> Option<Value> {
    entries(text)
        .into_iter()
        .find(|entry| entry.get("slug").and_then(Value::as_str) == Some(SLUG))
}

fn entries(text: &str) -> Vec<Value> {
    match value_at(text, CONNECTIONS) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}

/// Wall-clock time in Unix milliseconds.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
