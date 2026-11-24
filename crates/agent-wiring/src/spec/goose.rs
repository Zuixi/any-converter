//! Goose specification (`config.yaml` plus a custom provider file).
//!
//! Goose reads its provider in one of two shapes. Newer installs keep a
//! `providers` mapping and an `active_provider` selector; older installs keep
//! the flat `GOOSE_PROVIDER` / `GOOSE_MODEL` keys. The spec preserves whichever
//! shape the file already uses, so a user's existing provider layout is never
//! converted behind their back.
//!
//! A custom provider descriptor lives beside the YAML at
//! `custom_providers/any-converter.json`; its `display_name` and any user
//! headers survive a connect, while `Authorization` always carries the agent
//! key.
//!
//! The config root is `%APPDATA%/Block/goose/config` on Windows and
//! `<xdg config>/goose` elsewhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, gateway_path,
    gw_base_match, load_stash, read_config, record_old_values, remove_config, save_stash,
    write_config,
};
use crate::edit::{KV, yaml};
use crate::error::WiringError;

#[cfg(windows)]
use super::app_data_dir;
#[cfg(not(windows))]
use super::xdg_config_home;

/// Configuration keys owned by the Goose spec.
pub const OUR_KEYS: &[&str] = &[
    "active_provider",
    "providers.any-converter",
    "GOOSE_PROVIDER",
    "GOOSE_MODEL",
    "GOOSE_THINKING_EFFORT",
];

/// Provider id written into both Goose shapes.
pub const PROVIDER_ID: &str = "any-converter";

const ACTIVE_PROVIDER: &str = "active_provider";
const PROVIDER_PATH: &str = "providers.any-converter";
const LEGACY_PROVIDER: &str = "GOOSE_PROVIDER";
const LEGACY_MODEL: &str = "GOOSE_MODEL";
const EFFORT_KEY: &str = "GOOSE_THINKING_EFFORT";
const THINKING_SENTINEL: &str = "__thinking_effort_written";

/// Reasoning levels Goose accepts for `GOOSE_THINKING_EFFORT`.
const EFFORTS: &[&str] = &["off", "low", "medium", "high", "max"];

/// Goose wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct GooseSpec;

impl GooseSpec {
    /// The directory holding `config.yaml` and `custom_providers`.
    fn base_dir(ctx: &HomeCtx) -> PathBuf {
        #[cfg(windows)]
        {
            app_data_dir(ctx).join("Block").join("goose").join("config")
        }
        #[cfg(not(windows))]
        {
            xdg_config_home(ctx).join("goose")
        }
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::base_dir(ctx).join("config.yaml")
    }

    fn provider_path(ctx: &HomeCtx) -> PathBuf {
        Self::base_dir(ctx)
            .join("custom_providers")
            .join("any-converter.json")
    }
}

impl AgentSpec for GooseSpec {
    fn id(&self) -> &'static str {
        "goose"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::base_dir(ctx);
        if config_dir.is_dir()
            || Self::config_path(ctx).exists()
            || Self::provider_path(ctx).exists()
            || bin_in_path(ctx, "goose")
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx), Self::provider_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let provider_path = Self::provider_path(ctx);
        let config = read_config(&config_path)?.unwrap_or_default();
        let existing_provider = read_config(&provider_path)?;

        let mut old = BTreeMap::new();
        for key in [
            ACTIVE_PROVIDER,
            PROVIDER_PATH,
            LEGACY_PROVIDER,
            LEGACY_MODEL,
            EFFORT_KEY,
        ] {
            if let Some(value) = yaml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let new_shape = yaml::get(&config, "providers").is_some()
            || yaml::get(&config, ACTIVE_PROVIDER).is_some();
        let mut kvs = if new_shape {
            vec![
                KV::new(ACTIVE_PROVIDER, json!(PROVIDER_ID)),
                KV::new(
                    PROVIDER_PATH,
                    json!({
                        "enabled": true,
                        "model": wiring.default_model,
                        "configured": true,
                    }),
                ),
            ]
        } else {
            vec![
                KV::new(LEGACY_PROVIDER, json!(PROVIDER_ID)),
                KV::new(LEGACY_MODEL, json!(wiring.default_model)),
            ]
        };
        match wiring.effort {
            None => {}
            Some("") => {
                record_thinking(&mut stash, self.id());
            }
            Some(effort) => {
                kvs.push(KV::new(EFFORT_KEY, json!(effort)));
                record_thinking(&mut stash, self.id());
            }
        }

        let mut text = yaml::set(&config, &kvs)?;
        if wiring.effort == Some("") {
            text = yaml::del(&text, &[EFFORT_KEY])?;
        }

        let provider = provider_json(wiring, existing_provider.as_deref())?;
        let provider_text = pretty_json(&provider)?;

        let written_config = write_config(&config_path, &text)?;
        let written_provider = write_config(&provider_path, &provider_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_config, written_provider])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let provider_path = Self::provider_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&config_path)? {
            let mut text = yaml::del(
                &current,
                &[
                    ACTIVE_PROVIDER,
                    PROVIDER_PATH,
                    LEGACY_PROVIDER,
                    LEGACY_MODEL,
                ],
            )?;
            for key in [
                ACTIVE_PROVIDER,
                PROVIDER_PATH,
                LEGACY_PROVIDER,
                LEGACY_MODEL,
            ] {
                if let Some(value) = old.get(key) {
                    if !value.is_empty() {
                        text = yaml::set(&text, &[KV::new(key, stashed_value(value))])?;
                    }
                }
            }
            if old.contains_key(THINKING_SENTINEL) {
                text = match old.get(EFFORT_KEY) {
                    Some(value) if !value.is_empty() => {
                        yaml::set(&text, &[KV::new(EFFORT_KEY, json!(value))])?
                    }
                    _ => yaml::del(&text, &[EFFORT_KEY])?,
                };
            }
            text = prune_empty(&text, "providers")?;
            written.push(write_config(&config_path, &text)?);
        }

        if provider_path.exists() {
            remove_config(&provider_path)?;
            written.push(provider_path);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let config = read_config(&Self::config_path(ctx))
            .ok()
            .flatten()
            .unwrap_or_default();
        let new_shape = yaml::get(&config, "providers").is_some()
            || yaml::get(&config, ACTIVE_PROVIDER).is_some();
        let (provider, model) = if new_shape {
            (
                yaml::get(&config, ACTIVE_PROVIDER),
                yaml::get(&config, &format!("{PROVIDER_PATH}.model")),
            )
        } else {
            (
                yaml::get(&config, LEGACY_PROVIDER),
                yaml::get(&config, LEGACY_MODEL),
            )
        };
        let base_url = read_config(&Self::provider_path(ctx))
            .ok()
            .flatten()
            .and_then(|text| provider_base(&text));

        if provider.is_none() && model.is_none() && base_url.is_none() {
            return WiredState::Disconnected;
        }
        let wired = provider.as_deref() == Some(PROVIDER_ID)
            && model.is_some_and(|value| !value.is_empty());
        let base_ok = base_url
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        match (wired, base_ok) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in [
                ACTIVE_PROVIDER,
                PROVIDER_PATH,
                LEGACY_PROVIDER,
                LEGACY_MODEL,
                EFFORT_KEY,
            ] {
                if let Some(value) = yaml::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        if let Ok(Some(text)) = read_config(&Self::provider_path(ctx)) {
            if let Some(value) = provider_base(&text) {
                values.insert("custom_providers.base_url".to_owned(), value);
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Marks in the stash that this spec touched `GOOSE_THINKING_EFFORT`.
///
/// The marker distinguishes "we never wrote the key" (leave it alone) from
/// "we wrote it and the user had nothing" (remove it on disconnect).
fn record_thinking(stash: &mut crate::backup::Stash, agent: &str) {
    let mut marker = BTreeMap::new();
    marker.insert(THINKING_SENTINEL.to_owned(), "1".to_owned());
    record_old_values(stash, agent, marker);
}

/// The custom provider descriptor, laid over a descriptor the user or Goose
/// already wrote.
///
/// Goose's own editor carries the keys it does not show into the file, and
/// users set `timeout_seconds`, `toolshim`, `supports_streaming`,
/// `preserves_thinking`, a custom name or their own headers there. Those
/// survive; only the gateway's fields and the model list are ours.
///
/// `skip_canonical_filtering` and `dynamic_models: false` keep Goose to this
/// static list instead of fetching `/v1/models` and filtering our models away
/// against its own registry. `reasoning` follows the catalog's per-model
/// flag; magpie infers the same field from a model-name regex.
fn provider_json(wiring: &WiringCtx<'_>, existing: Option<&str>) -> Result<Value, WiringError> {
    let mut provider = match existing {
        Some(text) if !text.trim().is_empty() => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                return Err(WiringError::Refused(
                    "custom provider root is not an object".to_owned(),
                ));
            }
            Err(error) => {
                return Err(WiringError::Refused(format!(
                    "custom provider is not valid JSON: {error}"
                )));
            }
        },
        _ => Map::new(),
    };

    let mut headers = provider
        .get("headers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    headers.insert(
        "Authorization".to_owned(),
        json!(format!("Bearer {}", wiring.agent_key)),
    );

    let mut models = Vec::new();
    for model in &wiring.catalog.models {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(model.id));
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if let Some(context) = model.context_tokens {
            entry.insert("context_limit".to_owned(), json!(context));
        }
        models.push(Value::Object(entry));
    }

    // A custom `name` is kept, like the other fields Goose shows in its own
    // editor; the rest of the gateway wiring is ours.
    provider
        .entry("name".to_owned())
        .or_insert_with(|| json!(PROVIDER_ID));
    if provider
        .get("display_name")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        provider.insert("display_name".to_owned(), json!(PROVIDER_ID));
    }
    for (key, value) in [
        ("engine", json!("openai")),
        ("api_key_env", json!("")),
        ("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
        ("headers", Value::Object(headers)),
        ("requires_auth", json!(false)),
        ("skip_canonical_filtering", json!(true)),
        ("dynamic_models", json!(false)),
        ("models", Value::Array(models)),
    ] {
        provider.insert(key.to_owned(), value);
    }
    Ok(Value::Object(provider))
}

/// Reads `base_url` from a custom provider descriptor.
fn provider_base(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Serializes JSON with two-space indentation and a trailing newline.
fn pretty_json(value: &Value) -> Result<String, WiringError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| WiringError::Refused(format!("cannot serialize provider: {error}")))?;
    text.push('\n');
    Ok(text)
}

/// Parses a stashed YAML value back into JSON, treating bare text as a string.
fn stashed_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// Removes `path` when it is now an empty mapping.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match yaml::get(text, path) {
        Some(value) if matches!(value.trim(), "{}" | "null" | "~" | "") => {
            Ok(yaml::del(text, &[path])?)
        }
        _ => Ok(text.to_owned()),
    }
}
