//! Qwen Code specification (`QWEN_HOME` or `~/.qwen/settings.json`).
//!
//! Qwen Code reads OpenAI-compatible providers from the `modelProviders.openai`
//! array; each entry names a model, the base URL it is served from, and a
//! variable in the same file's `env` object holding the key. Sessions start on
//! `model.name` (with `model.baseUrl` telling duplicate ids apart), and
//! `security.auth.selectedType = "openai"` skips the `/auth` prompt.
//!
//! Entries written here carry a dedicated `envKey`, which is what tells them
//! from the user's own. The array is replaced wholesale, so the user's entries
//! stay untouched and in front of ours.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Qwen Code spec.
pub const OUR_KEYS: &[&str] = &[
    "modelProviders.openai",
    "model.name",
    "model.baseUrl",
    "security.auth.selectedType",
    "env.ANY_CONVERTER_QWEN_API_KEY",
];

const MODELS: &str = "modelProviders.openai";
const MODEL_NAME: &str = "model.name";
const MODEL_BASE: &str = "model.baseUrl";
const AUTH_TYPE: &str = "security.auth.selectedType";
const ENV_KEY: &str = "env.ANY_CONVERTER_QWEN_API_KEY";
const ENV_NAME: &str = "ANY_CONVERTER_QWEN_API_KEY";

/// Qwen Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct QwenSpec;

impl QwenSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "QWEN_HOME", |home| home.home.join(".qwen"))
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for QwenSpec {
    fn id(&self) -> &'static str {
        "qwen"
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
        let settings = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for (key, path) in [
            ("model.name", MODEL_NAME),
            ("model.baseUrl", MODEL_BASE),
            ("security.auth.selectedType", AUTH_TYPE),
            (ENV_KEY, ENV_KEY),
        ] {
            if let Some(value) = json::get(&settings, path) {
                old.insert(key.to_owned(), value);
            }
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let base = gateway_path(wiring.gateway_url, "v1");
        let mut entries = user_entries(&settings, MODELS)?;
        entries.extend(wiring.catalog.models.iter().map(|model| {
            let mut entry = serde_json::Map::new();
            entry.insert("baseUrl".to_owned(), json!(base));
            entry.insert("envKey".to_owned(), json!(ENV_NAME));
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("name".to_owned(), json!(model.name));
            if let Some(context) = model.context_tokens {
                entry.insert(
                    "generationConfig".to_owned(),
                    json!({ "contextWindowSize": context }),
                );
            }
            Value::Object(entry)
        }));

        let text = json::set(
            &settings,
            &[
                KV::new(MODELS, Value::Array(entries)),
                KV::new(MODEL_NAME, json!(wiring.default_model)),
                KV::new(MODEL_BASE, json!(base)),
                KV::new(AUTH_TYPE, json!("openai")),
                KV::new(ENV_KEY, json!(wiring.agent_key)),
            ],
        )?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = current;

            let kept = user_entries(&text, MODELS)?;
            text = if kept.is_empty() {
                json::del(&text, &[MODELS])?
            } else {
                json::set(&text, &[KV::new(MODELS, Value::Array(kept))])?
            };
            text = prune_empty(&text, "modelProviders")?;

            text = restore(&text, MODEL_NAME, old.get("model.name"))?;
            text = restore(&text, MODEL_BASE, old.get("model.baseUrl"))?;
            text = prune_empty(&text, "model")?;
            text = restore(&text, AUTH_TYPE, old.get("security.auth.selectedType"))?;
            text = prune_empty(&text, "security.auth")?;
            text = prune_empty(&text, "security")?;
            text = restore(&text, ENV_KEY, old.get(ENV_KEY))?;
            text = prune_empty(&text, "env")?;

            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let ours = our_entries(&settings, MODELS);
        let base = ours
            .iter()
            .all(|entry| entry_base(entry).is_some_and(|value| gw_base_match(value, gateway_url)));
        let key = json::get(&settings, ENV_KEY).is_some_and(|value| !value.is_empty());
        let selected = json::get(&settings, MODEL_NAME).is_some_and(|name| {
            ours.iter()
                .any(|entry| entry_id(entry).is_some_and(|id| id == name))
        });

        if ours.is_empty() && !key {
            return WiredState::Disconnected;
        }
        if base && key && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for path in [MODELS, MODEL_NAME, MODEL_BASE, AUTH_TYPE, ENV_KEY] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
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

/// Entries in `path` that this spec did not write.
fn user_entries(text: &str, path: &str) -> Result<Vec<Value>, WiringError> {
    Ok(read_array(text, path)?
        .into_iter()
        .filter(|entry| entry_env_key(entry) != Some(ENV_NAME))
        .collect())
}

/// Entries in `path` that this spec wrote.
fn our_entries(text: &str, path: &str) -> Vec<Value> {
    read_array(text, path)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry_env_key(entry) == Some(ENV_NAME))
        .collect()
}

fn entry_env_key(entry: &Value) -> Option<&str> {
    entry.get("envKey")?.as_str()
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id")?.as_str()
}

fn entry_base(entry: &Value) -> Option<&str> {
    entry.get("baseUrl")?.as_str()
}

/// Restores one stashed value, or removes the key when there was none.
fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

/// Removes `path` when it is now an empty object.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
