//! OpenBot specification (`<user data>/OpenBot/openbot-custom-providers-v1.json`).
//!
//! OpenBot reads custom OpenAI-compatible providers from a versioned JSON file
//! under its user-data directory. Wiring appends one keyless provider named
//! `any-converter`; OpenBot adds no authentication header of its own, so the
//! provider carries no secret (`secret: null`). Disconnect removes the entry
//! and deletes the file when nothing but the version marker remains.
//!
//! OpenBot rejects the whole model list when it contains one entry it cannot
//! accept, so ids are filtered, deduplicated, length-checked, and capped before
//! they are written. Files with an unsupported version or a non-array
//! `providers` value are refused untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, app_data_dir, gateway_path,
    gw_base_match, read_config, remove_config, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenBot spec.
pub const OUR_KEYS: &[&str] = &["version", "providers"];

const PROVIDER: &str = "any-converter";
const PROVIDERS: &str = "providers";
const VERSION: &str = "version";
const FILE_NAME: &str = "openbot-custom-providers-v1.json";
/// The only provider-file version this spec understands.
const SUPPORTED_VERSION: &str = "1";
/// Upper bound on the model list written into our provider.
const MAX_MODELS: usize = 64;
/// Longest model id OpenBot accepts.
const MAX_ID_LEN: usize = 128;
/// Longest prefixed model key OpenBot accepts.
const MAX_PREFIXED_LEN: usize = 160;
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;
/// Context bounds OpenBot accepts.
const MIN_CONTEXT: u64 = 1_024;
const MAX_CONTEXT: u64 = 100_000_000;

/// OpenBot wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenBotSpec;

impl OpenBotSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        app_data_dir(ctx).join("OpenBot")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(FILE_NAME)
    }
}

impl AgentSpec for OpenBotSpec {
    fn id(&self) -> &'static str {
        "openbot"
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
        validate(&current)?;

        let existing = our_provider(&current);
        let mut entries = user_providers(&current);
        entries.push(merge_provider(provider(wiring), existing.as_ref()));
        let text = json::set(
            &current,
            &[
                KV::new(VERSION, json!(1)),
                KV::new(PROVIDERS, Value::Array(entries)),
            ],
        )?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut written = Vec::new();
        let Some(current) = read_config(&path)? else {
            return Ok(written);
        };
        validate(&current)?;

        let kept = user_providers(&current);
        let mut text = if kept.is_empty() {
            json::del(&current, &[PROVIDERS])?
        } else {
            json::set(&current, &[KV::new(PROVIDERS, Value::Array(kept))])?
        };
        if is_only_version(&text) {
            remove_config(&path)?;
            written.push(path);
            return Ok(written);
        }
        text = json::set(&text, &[KV::new(VERSION, json!(1))])?;
        written.push(write_config(&path, &text)?);
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let Some(entry) = our_provider(&current) else {
            return WiredState::Disconnected;
        };
        let base = entry.get("baseUrl").and_then(Value::as_str);
        if base.is_some_and(|value| gw_base_match(value, gateway_url)) {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::config_path(ctx)) {
            if let Some(entry) = our_provider(&current) {
                if let Some(base) = entry.get("baseUrl").and_then(Value::as_str) {
                    values.insert(format!("{PROVIDERS}.{PROVIDER}.baseUrl"), base.to_owned());
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
                "unsupported provider-file version: {version}"
            )));
        }
    }
    if let Some(raw) = json::get(text, PROVIDERS) {
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Array(_)) => {}
            Ok(_) => {
                return Err(WiringError::Refused(format!("{PROVIDERS} is not an array")));
            }
            Err(error) => {
                return Err(WiringError::Refused(format!(
                    "{PROVIDERS} is not valid JSON: {error}"
                )));
            }
        }
    }
    Ok(())
}

/// The provider entry OpenBot reads.
fn provider(wiring: &WiringCtx<'_>) -> Value {
    let mut models = Vec::new();
    for model in &wiring.catalog.models {
        if models.len() == MAX_MODELS {
            break;
        }
        if model.id.is_empty() || model.name.is_empty() {
            continue;
        }
        if !valid_model_id(&model.id) || model.id.len() > MAX_ID_LEN {
            continue;
        }
        if format!("{PROVIDER}/{}", model.id).len() > MAX_PREFIXED_LEN {
            continue;
        }
        if models
            .iter()
            .any(|seen: &Value| seen.get("id").and_then(Value::as_str) == Some(model.id.as_str()))
        {
            continue;
        }
        let context = model
            .context_tokens
            .unwrap_or(DEFAULT_CONTEXT)
            .clamp(MIN_CONTEXT, MAX_CONTEXT);
        models.push(json!({
            "id": model.id,
            "name": model.name,
            "contextWindow": context,
        }));
    }
    json!({
        "id": PROVIDER,
        "name": PROVIDER,
        "baseUrl": gateway_path(wiring.gateway_url, "agent/openbot/v1"),
        "models": models,
        "secret": null,
    })
}

/// Keeps a pre-existing our-provider's identity fields.
fn merge_provider(mut entry: Value, existing: Option<&Value>) -> Value {
    let Some(existing) = existing.and_then(Value::as_object) else {
        return entry;
    };
    let Some(target) = entry.as_object_mut() else {
        return entry;
    };
    for key in ["name", "harness", "secret"] {
        if let Some(value) = existing.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
    entry
}

/// Whether an id matches OpenBot's accepted character set and length.
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '.' | '_' | ':' | '/' | '[' | ']' | ',' | '=' | '-')
        })
}

/// Entries in `providers` that this spec did not write.
fn user_providers(text: &str) -> Vec<Value> {
    entries(text)
        .into_iter()
        .filter(|entry| entry.get("id").and_then(Value::as_str) != Some(PROVIDER))
        .collect()
}

fn our_provider(text: &str) -> Option<Value> {
    entries(text)
        .into_iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(PROVIDER))
}

fn entries(text: &str) -> Vec<Value> {
    match value_at(text, PROVIDERS) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

/// Whether the document carries nothing but the version marker.
fn is_only_version(text: &str) -> bool {
    let Some(Value::Object(document)) = value_at(text, "") else {
        return false;
    };
    document.keys().all(|key| key == VERSION)
}

fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}
