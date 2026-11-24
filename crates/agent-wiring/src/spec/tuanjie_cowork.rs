//! Tuanjie Cowork specification (`~/.codely/settings.json`).
//!
//! Tuanjie Cowork names custom endpoints in `customModelProviders` and their
//! models in `customModels`. Builds disagree on whether those containers are
//! arrays or maps, so this spec is tolerant of both: it filters or inserts the
//! `any-converter` entry in whichever shape the file already uses, and settles
//! on an array when neither key exists yet.
//!
//! Disconnect removes both entries and deletes `settings.json` when nothing
//! else remains in it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    join_relative, read_config, remove_config, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Tuanjie Cowork spec.
pub const OUR_KEYS: &[&str] = &["customModelProviders", "customModels"];

const PROVIDER: &str = "any-converter";
const PROVIDERS: &str = "customModelProviders";
const MODELS: &str = "customModels";

/// Tuanjie Cowork wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct TuanjieCoworkSpec;

impl TuanjieCoworkSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".codely")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for TuanjieCoworkSpec {
    fn id(&self) -> &'static str {
        "tuanjie-cowork"
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

        let provider = with_entry(
            read_bucket(&current, PROVIDERS)?,
            json!({
                "id": PROVIDER,
                "name": PROVIDER,
                "baseUrl": gateway_path(wiring.gateway_url, "v1"),
                "apiKey": wiring.agent_key,
                "authType": "openai",
            }),
        );
        let models = with_entry(
            read_bucket(&current, MODELS)?,
            json!({
                "id": PROVIDER,
                "providerId": PROVIDER,
                "model": wiring.default_model,
                "displayName": wiring.default_model,
                "roles": ["model"],
                "wireApi": "chat",
                "supportsMultimodal": false,
            }),
        );

        let text = json::set(
            &current,
            &[KV::new(PROVIDERS, provider), KV::new(MODELS, models)],
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

        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        for key in [PROVIDERS, MODELS] {
            match without_entry(read_bucket(&current, key)?) {
                Some(value) => sets.push(KV::new(key, value)),
                None => deletes.push(key),
            }
        }
        let mut text = json::set(&current, &sets)?;
        text = json::del(&text, &deletes)?;

        if is_empty_document(&text) {
            remove_config(&path)?;
            written.push(path);
        } else {
            written.push(write_config(&path, &text)?);
        }
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let Ok(bucket) = read_bucket(&current, PROVIDERS) else {
            return WiredState::Disconnected;
        };
        let Some(entry) = our_entry(&bucket) else {
            return WiredState::Disconnected;
        };
        let base = entry.get("baseUrl").and_then(Value::as_str);
        let key = entry.get("apiKey").and_then(Value::as_str);
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
            if let Ok(bucket) = read_bucket(&current, PROVIDERS) {
                if let Some(entry) = our_entry(&bucket) {
                    for key in ["baseUrl", "apiKey"] {
                        if let Some(value) = entry.get(key).and_then(Value::as_str) {
                            values
                                .insert(format!("{PROVIDERS}.{PROVIDER}.{key}"), value.to_owned());
                        }
                    }
                }
            }
        }
        values
    }
}

/// A container the settings file uses for custom entries.
enum Bucket {
    Array(Vec<Value>),
    Map(Map<String, Value>),
}

/// Reads `path` as an array or a map, defaulting to an empty array.
fn read_bucket(text: &str, path: &str) -> Result<Bucket, WiringError> {
    match value_at(text, path) {
        None => Ok(Bucket::Array(Vec::new())),
        Some(Value::Array(items)) => Ok(Bucket::Array(items)),
        Some(Value::Object(map)) => Ok(Bucket::Map(map)),
        Some(_) => Err(WiringError::Refused(format!(
            "{path} is not an array or object"
        ))),
    }
}

/// Inserts `entry`, merging it into any previous `any-converter` entry.
///
/// The user's own `name` and `roles` survive a reconnect; every other field is
/// refreshed from the gateway.
fn with_entry(bucket: Bucket, entry: Value) -> Value {
    match bucket {
        Bucket::Array(mut items) => {
            let existing = items
                .iter()
                .find(|item| id_of(item) == Some(PROVIDER))
                .cloned();
            items.retain(|item| id_of(item) != Some(PROVIDER));
            items.push(merge_entry(existing.as_ref(), entry));
            Value::Array(items)
        }
        Bucket::Map(mut map) => {
            let existing = map.get(PROVIDER).cloned();
            map.insert(PROVIDER.to_owned(), merge_entry(existing.as_ref(), entry));
            Value::Object(map)
        }
    }
}

/// Overlays `entry` on `existing`, keeping user-owned presentation fields.
fn merge_entry(existing: Option<&Value>, entry: Value) -> Value {
    let Some(existing) = existing.and_then(Value::as_object) else {
        return entry;
    };
    let Value::Object(mut merged) = entry else {
        return entry;
    };
    for key in ["name", "roles"] {
        if let Some(value) = existing.get(key) {
            merged.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(merged)
}

/// Removes our entry, returning `None` when the container would be empty.
fn without_entry(bucket: Bucket) -> Option<Value> {
    match bucket {
        Bucket::Array(mut items) => {
            items.retain(|item| id_of(item) != Some(PROVIDER));
            (!items.is_empty()).then_some(Value::Array(items))
        }
        Bucket::Map(mut map) => {
            map.remove(PROVIDER);
            (!map.is_empty()).then_some(Value::Object(map))
        }
    }
}

/// Our entry in whichever shape the container uses.
fn our_entry(bucket: &Bucket) -> Option<Value> {
    match bucket {
        Bucket::Array(items) => items
            .iter()
            .find(|item| id_of(item) == Some(PROVIDER))
            .cloned(),
        Bucket::Map(map) => map.get(PROVIDER).cloned(),
    }
}

fn id_of(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}

fn is_empty_document(text: &str) -> bool {
    matches!(value_at(text, ""), Some(Value::Object(map)) if map.is_empty())
}
