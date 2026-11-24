//! GitHub Copilot specification (`$COPILOT_HOME` or `~/.copilot`).
//!
//! Copilot reads OpenAI-compatible providers and models from arrays in
//! `providers.json` (overridable through `$COPILOT_PROVIDERS_CONFIG`) and
//! selects the active model plus an optional effort level from `settings.json`.
//! Both arrays are edited key-by-key so unknown top-level members survive.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, expand_tilde, gateway_path,
    gw_base_match, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Copilot spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.providers",
    "providers.models",
    "settings.model",
    "settings.effortLevel",
];

const PROVIDER: &str = super::support::PROVIDER;
const SETTINGS_FILE: &str = "settings.json";
const PROVIDERS_FILE: &str = "providers.json";
const PROVIDERS: &str = "providers";
const MODELS: &str = "models";
const SETTING_MODEL: &str = "model";
const EFFORT_LEVEL: &str = "effortLevel";

/// Reasoning levels Copilot accepts, in display order.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// GitHub Copilot wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CopilotSpec;

impl CopilotSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "COPILOT_HOME", |ctx| ctx.home.join(".copilot"))
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }

    fn providers_path(ctx: &HomeCtx) -> PathBuf {
        match (ctx.env)("COPILOT_PROVIDERS_CONFIG") {
            Some(value) if !value.trim().is_empty() => expand_tilde(ctx, value.trim()),
            _ => Self::dir(ctx).join(PROVIDERS_FILE),
        }
    }
}

impl AgentSpec for CopilotSpec {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::settings_path(ctx).exists()
            || Self::providers_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx), Self::providers_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let providers_path = Self::providers_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let providers_text = read_config(&providers_path)?.unwrap_or_default();
        let settings = read_config(&settings_path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[SETTING_MODEL]),
        );

        let mut providers = user_providers(&providers_text)?;
        providers.push(json!({
            "name": PROVIDER,
            "type": "openai",
            "baseUrl": gateway_path(wiring.gateway_url, "v1"),
            "apiKey": wiring.agent_key,
        }));
        let mut models = user_models(&providers_text)?;
        models.extend(model_entries(wiring));

        let next_providers = json::set(
            &providers_text,
            &[
                KV::new(PROVIDERS, Value::Array(providers)),
                KV::new(MODELS, Value::Array(models)),
            ],
        )?;

        let mut next_settings = json::set(
            &settings,
            &[KV::new(
                SETTING_MODEL,
                json!(format!("{PROVIDER}/{}", wiring.default_model)),
            )],
        )?;
        next_settings = apply_effort(&next_settings, wiring.effort)?;

        let written_providers = write_config(&providers_path, &next_providers)?;
        let written_settings = write_config(&settings_path, &next_settings)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_providers, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let providers_path = Self::providers_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&providers_path)? {
            let mut text = write_array(&current, PROVIDERS, user_providers(&current)?)?;
            text = write_array(&text, MODELS, user_models(&text)?)?;
            written.push(write_config(&providers_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let text = super::support::restore_keys(&current, &old, &[SETTING_MODEL])?;
            let text = json::del_pruning(&text, &[EFFORT_LEVEL])?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let providers = read_config(&Self::providers_path(ctx)).ok().flatten();
        let settings = read_config(&Self::settings_path(ctx)).ok().flatten();
        let selected = settings
            .as_deref()
            .and_then(|text| json::get(text, SETTING_MODEL));

        let prefix = format!("{PROVIDER}/");
        let Some(providers_text) = providers.as_deref() else {
            let ours = selected
                .as_deref()
                .is_some_and(|value| value.starts_with(&prefix));
            return if ours {
                WiredState::Drifted
            } else {
                WiredState::Disconnected
            };
        };

        let provider_entry = read_array(providers_text, PROVIDERS)
            .unwrap_or_default()
            .into_iter()
            .find(|entry| name(entry) == Some(PROVIDER));
        let ours_models = read_array(providers_text, MODELS)
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| provider_name(entry) == Some(PROVIDER))
            .count();

        let selected = selected
            .as_deref()
            .is_some_and(|value| value.starts_with(&prefix));
        if provider_entry.is_none() && ours_models == 0 && !selected {
            return WiredState::Disconnected;
        }

        let gateway = provider_entry
            .as_ref()
            .and_then(entry_base)
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = provider_entry
            .as_ref()
            .and_then(entry_key)
            .is_some_and(|value| !value.is_empty());
        if gateway && key && ours_models > 0 && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in [SETTING_MODEL, EFFORT_LEVEL] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(format!("settings.{key}"), value);
                }
            }
        }
        if let Ok(Some(providers)) = read_config(&Self::providers_path(ctx)) {
            for key in [PROVIDERS, MODELS] {
                if let Some(value) = json::get(&providers, key) {
                    values.insert(format!("providers.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// One model entry per catalog model.
///
/// Copilot computes the prompt budget as context minus output. The catalog has
/// no output limit, so the output side is zero and the prompt limit reduces to
/// the context window itself. The display name is written only when it adds
/// information over the id.
fn model_entries(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("provider".to_owned(), json!(PROVIDER));
            if !model.name.is_empty() && model.name != model.id {
                entry.insert("name".to_owned(), json!(model.name));
            }
            entry.insert("maxOutputTokens".to_owned(), json!(16384));
            if let Some(context) = model.context_tokens {
                entry.insert("maxContextWindowTokens".to_owned(), json!(context));
                entry.insert("maxPromptTokens".to_owned(), json!(context));
            }
            Value::Object(entry)
        })
        .collect()
}

/// Writes or clears the effort level.
///
/// `None` leaves the file untouched; `Some("")` removes our setting.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(json::del_pruning(text, &[EFFORT_LEVEL])?),
        Some(level) => Ok(json::set(text, &[KV::new(EFFORT_LEVEL, json!(level))])?),
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

/// Replaces an array with `items`, or removes an emptied array.
fn write_array(text: &str, path: &str, items: Vec<Value>) -> Result<String, WiringError> {
    if items.is_empty() {
        Ok(json::del_pruning(text, &[path])?)
    } else {
        Ok(json::set(text, &[KV::new(path, Value::Array(items))])?)
    }
}

/// Providers that are not ours.
fn user_providers(text: &str) -> Result<Vec<Value>, WiringError> {
    Ok(read_array(text, PROVIDERS)?
        .into_iter()
        .filter(|entry| name(entry) != Some(PROVIDER))
        .collect())
}

/// Models that are not ours.
fn user_models(text: &str) -> Result<Vec<Value>, WiringError> {
    Ok(read_array(text, MODELS)?
        .into_iter()
        .filter(|entry| provider_name(entry) != Some(PROVIDER))
        .collect())
}

fn name(entry: &Value) -> Option<&str> {
    entry.get("name")?.as_str()
}

fn provider_name(entry: &Value) -> Option<&str> {
    entry.get("provider")?.as_str()
}

fn entry_base(entry: &Value) -> Option<&str> {
    entry.get("baseUrl")?.as_str()
}

fn entry_key(entry: &Value) -> Option<&str> {
    entry.get("apiKey")?.as_str()
}
