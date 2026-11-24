//! StepCode specification (`STEP_CODING_AGENT_DIR`, `STEPCODE_CONFIG_DIR`, or
//! `~/.stepcode`).
//!
//! StepCode keeps its defaults in `config.toml` (`defaultProvider`,
//! `defaultModel`, `enabledModels`) and its providers in a Pi-shaped
//! `models.json` whose provider block carries the gateway key and a model
//! array. Connect rewires both files; disconnect restores the user's previous
//! selections and removes the provider block.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::support;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gw_base_match,
    join_relative, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json as json_edit, toml};
use crate::error::WiringError;

/// Configuration keys owned by the StepCode spec.
pub const OUR_KEYS: &[&str] = &[
    "config.defaultProvider",
    "config.defaultModel",
    "config.enabledModels",
    "models.providers.any-converter",
];

/// The TOML keys connect writes and disconnect restores.
const TOML_KEYS: &[&str] = &["defaultProvider", "defaultModel", "enabledModels"];

/// StepCode wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct StepcodeSpec;

impl StepcodeSpec {
    /// StepCode's configuration directory.
    ///
    /// `STEP_CODING_AGENT_DIR` names an `agent` directory when absolute, so its
    /// parent is the config root. `STEPCODE_CONFIG_DIR` is a directory name
    /// under the home. The default is `~/.stepcode`.
    fn dir(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("STEP_CODING_AGENT_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                let path = expand_tilde(ctx, value);
                if path.is_absolute() {
                    if let Some(parent) = path.parent() {
                        return parent.to_path_buf();
                    }
                }
            }
        }
        if let Some(value) = (ctx.env)("STEPCODE_CONFIG_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                let path = expand_tilde(ctx, value);
                return if path.is_absolute() {
                    path
                } else {
                    join_relative(&ctx.home, value)
                };
            }
        }
        join_relative(&ctx.home, ".stepcode")
    }

    /// The TOML file holding the default provider and model.
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.toml")
    }

    /// The JSON file holding provider definitions.
    fn models_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("models.json")
    }

    /// The dotted path of the provider block in `models.json`.
    fn provider_path() -> String {
        format!("providers.{}", support::PROVIDER)
    }
}

impl AgentSpec for StepcodeSpec {
    fn id(&self) -> &'static str {
        "stepcode"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::config_path(ctx).exists() || Self::models_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx), Self::models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let models_path = Self::models_path(ctx);
        let config = read_config(&config_path)?.unwrap_or_default();
        let models = read_config(&models_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in TOML_KEYS {
            if let Some(value) = toml::get(&config, key) {
                old.insert((*key).to_owned(), value);
            }
        }
        // Lets disconnect tell an explicitly empty `providers` from an absent
        // one.
        if let Some(value) = json_edit::get(&models, "providers") {
            old.insert("providers".to_owned(), value);
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let provider = json!({
            "name": support::PROVIDER,
            "baseUrl": support::base_url(wiring),
            "api": "openai-completions",
            "apiKey": wiring.agent_key,
            "models": provider_models(wiring),
        });
        let models_text = json_edit::set(&models, &[KV::new(Self::provider_path(), provider)])?;

        let config_text = toml::set(
            &config,
            &[
                KV::new("defaultProvider", json!(support::PROVIDER)),
                KV::new("defaultModel", json!(wiring.default_model)),
            ],
        )?;
        let config_text = append_enabled_model(&config_text, wiring.default_model)?;

        let written_models = write_config(&models_path, &models_text)?;
        let written_config = write_config(&config_path, &config_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_config, written_models])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let models_path = Self::models_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&models_path)? {
            let provider = Self::provider_path();
            let mut text = json_edit::del(&current, &[provider.as_str()])?;
            text = prune_container(&text, "providers", old.contains_key("providers"))?;
            written.push(write_config(&models_path, &text)?);
        }

        if let Some(current) = read_config(&config_path)? {
            let mut text = current;
            for key in TOML_KEYS {
                text = restore_toml(&text, key, old.get(*key))?;
            }
            written.push(write_config(&config_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(models)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let provider = Self::provider_path();
        let base = json_edit::get(&models, &format!("{provider}.baseUrl"));
        let key = json_edit::get(&models, &format!("{provider}.apiKey"));
        let base = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (base, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in TOML_KEYS {
                if let Some(value) = toml::get(&config, key) {
                    values.insert(format!("config.{key}"), value);
                }
            }
        }
        let provider = Self::provider_path();
        if let Ok(Some(models)) = read_config(&Self::models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json_edit::get(&models, &format!("{provider}.{key}")) {
                    values.insert(format!("{provider}.{key}"), value);
                }
            }
        }
        values
    }
}

/// The provider's model array, one entry per catalog model.
///
/// The window key is `contextWindow` (magpie `agents.go:906-908`,
/// `stepcode_test.go:607`). Catalog approximation: no image/effort/output
/// data is available, so entries carry only id, name, reasoning, and window;
/// StepCode falls back to its own limits for the rest.
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    support::catalog_models(wiring, |model| {
        let mut entry = Map::new();
        entry.insert("id".to_owned(), json!(model.id));
        entry.insert("name".to_owned(), json!(model.name));
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if let Some(context) = model.context_tokens {
            entry.insert("contextWindow".to_owned(), json!(context));
        }
        Value::Object(entry)
    })
}

/// Appends `provider/model` to an existing `enabledModels` list.
///
/// StepCode names models as `"<provider>/<model>"` (magpie
/// `stepcode.go:585-590`, fixture `"magpie/deepseek/flash"`), so the entry
/// and the dedupe key both use that form. A missing or non-array list is left
/// untouched: an absent allow-list means "everything enabled".
fn append_enabled_model(text: &str, model: &str) -> Result<String, WiringError> {
    let Some(raw) = toml::get(text, "enabledModels") else {
        return Ok(text.to_owned());
    };
    let Ok(Value::Array(mut items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(text.to_owned());
    };
    let reference = format!("{}/{}", support::PROVIDER, model);
    if items
        .iter()
        .filter_map(Value::as_str)
        .any(|pattern| super::pi::enabled_model_takes(pattern, &reference))
    {
        return Ok(text.to_owned());
    }
    items.push(Value::String(reference));
    Ok(toml::set(
        text,
        &[KV::new("enabledModels", Value::Array(items))],
    )?)
}

/// Restores one stashed TOML value, deleting the key when it was absent.
fn restore_toml(text: &str, key: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let value = serde_json::from_str::<Value>(value)
                .unwrap_or_else(|_| Value::String(value.clone()));
            Ok(toml::set(text, &[KV::new(key, value)])?)
        }
        _ => Ok(toml::del(text, &[key])?),
    }
}

/// Removes an emptied container unless it existed before connect.
fn prune_container(text: &str, path: &str, existed: bool) -> Result<String, WiringError> {
    if existed {
        return Ok(text.to_owned());
    }
    if json_edit::get(text, path)
        .as_deref()
        .is_some_and(|value| value.trim() == "{}")
    {
        return Ok(json_edit::del(text, &[path])?);
    }
    Ok(text.to_owned())
}
