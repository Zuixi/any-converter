//! Command Code specification (`~/.commandcode`).
//!
//! `providers.json` holds one OpenAI-completions provider block keyed by our
//! provider id. `settings.json` selects that provider through `modelProvider`
//! and `model`, and keeps a per-model `reasoningEffort` map keyed by the
//! `<provider>/<model>` identifier. Keying efforts by the qualified model id
//! keeps the user's own entries separate from ours.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Command Code spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.provider.any-converter",
    "settings.model",
    "settings.modelProvider",
    "settings.reasoningEffort",
];

const PROVIDER: &str = super::support::PROVIDER;
const PROVIDERS_FILE: &str = "providers.json";
const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_PATH: &str = "provider.any-converter";
const MODEL: &str = "model";
const MODEL_PROVIDER: &str = "modelProvider";
const REASONING_EFFORT: &str = "reasoningEffort";

/// Reasoning levels Command Code forwards to reasoning models.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Command Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommandCodeSpec;

impl CommandCodeSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".commandcode")
    }

    fn providers_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(PROVIDERS_FILE)
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }
}

impl AgentSpec for CommandCodeSpec {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::providers_path(ctx).exists()
            || Self::settings_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::providers_path(ctx), Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let providers_path = Self::providers_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let providers = read_config(&providers_path)?.unwrap_or_default();
        let settings = read_config(&settings_path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[MODEL, MODEL_PROVIDER]),
        );

        let entry = json!({
            "name": PROVIDER,
            "api": "openai-completions",
            "baseURL": gateway_path(wiring.gateway_url, "v1"),
            "apiKey": false,
            "models": provider_models(wiring),
        });
        let providers_text = json::set(&providers, &[KV::new(PROVIDER_PATH, entry)])?;

        let model = model_key(wiring);
        let mut settings_text = json::set(
            &settings,
            &[
                KV::new(MODEL, json!(model)),
                KV::new(MODEL_PROVIDER, json!(PROVIDER)),
            ],
        )?;
        settings_text = apply_effort(&settings_text, wiring.effort, &model)?;

        let written_providers = write_config(&providers_path, &providers_text)?;
        let written_settings = write_config(&settings_path, &settings_text)?;
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
            let text = json::del(&current, &[PROVIDER_PATH])?;
            let text = prune_if_empty(&text, "provider")?;
            written.push(write_config(&providers_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let mut text = super::support::restore_keys(&current, &old, &[MODEL, MODEL_PROVIDER])?;
            text = remove_our_efforts(&text)?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let providers = read_config(&Self::providers_path(ctx)).ok().flatten();
        let settings = read_config(&Self::settings_path(ctx)).ok().flatten();
        let base = providers
            .as_deref()
            .and_then(|text| json::get(text, &format!("{PROVIDER_PATH}.baseURL")));
        let model = settings.as_deref().and_then(|text| json::get(text, MODEL));
        let provider = settings
            .as_deref()
            .and_then(|text| json::get(text, MODEL_PROVIDER));

        let prefix = format!("{PROVIDER}/");
        let selected = model
            .as_deref()
            .is_some_and(|value| value.starts_with(&prefix));
        let named = provider.as_deref() == Some(PROVIDER);
        if base.is_none() && !selected && !named {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && selected && named {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(providers)) = read_config(&Self::providers_path(ctx)) {
            for key in ["name", "baseURL"] {
                let path = format!("{PROVIDER_PATH}.{key}");
                if let Some(value) = json::get(&providers, &path) {
                    values.insert(path, value);
                }
            }
        }
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in [MODEL, MODEL_PROVIDER, REASONING_EFFORT] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(format!("settings.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The qualified model identifier Command Code selects on.
fn model_key(wiring: &WiringCtx<'_>) -> String {
    format!("{PROVIDER}/{}", wiring.default_model)
}

/// The provider's model map, one entry per catalog model.
///
/// The catalog carries no per-model effort ladder, so a reasoning model
/// advertises the full ladder this spec accepts; non-reasoning models carry
/// neither `reasoning` nor `reasoningEfforts`. `maxOutputTokens` is omitted
/// because we cannot know it (Command Code also omits it when zero).
fn provider_models(wiring: &WiringCtx<'_>) -> Map<String, Value> {
    let mut models = Map::new();
    for model in &wiring.catalog.models {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(model.name));
        if model.reasoning {
            entry.insert("reasoning".to_owned(), json!(true));
            entry.insert("reasoningEfforts".to_owned(), json!(EFFORTS));
        }
        if let Some(context) = model.context_tokens {
            entry.insert("contextWindow".to_owned(), json!(context));
        }
        models.insert(model.id.clone(), Value::Object(entry));
    }
    models
}

/// Writes or clears the reasoning level for our model.
///
/// `None` leaves the file untouched, `Some("")` removes our entry, and any
/// other value merges into the existing effort map.
fn apply_effort(text: &str, effort: Option<&str>, model: &str) -> Result<String, WiringError> {
    let Some(effort) = effort else {
        return Ok(text.to_owned());
    };
    let mut efforts = effort_map(text);
    efforts.remove(model);
    if !effort.is_empty() {
        efforts.insert(model.to_owned(), json!(effort));
    }
    write_efforts(text, efforts)
}

/// Removes every effort entry that belongs to our provider namespace.
fn remove_our_efforts(text: &str) -> Result<String, WiringError> {
    let mut efforts = effort_map(text);
    let prefix = format!("{PROVIDER}/");
    efforts.retain(|key, _| !key.starts_with(&prefix));
    write_efforts(text, efforts)
}

fn effort_map(text: &str) -> Map<String, Value> {
    json::get(text, REASONING_EFFORT)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_efforts(text: &str, efforts: Map<String, Value>) -> Result<String, WiringError> {
    if efforts.is_empty() {
        Ok(json::del_pruning(text, &[REASONING_EFFORT])?)
    } else {
        Ok(json::set(
            text,
            &[KV::new(REASONING_EFFORT, Value::Object(efforts))],
        )?)
    }
}

/// Deletes `path` when our removal left it literally empty.
fn prune_if_empty(text: &str, path: &str) -> Result<String, WiringError> {
    if json::get(text, path).as_deref() == Some("{}") {
        Ok(json::del(text, &[path])?)
    } else {
        Ok(text.to_owned())
    }
}
