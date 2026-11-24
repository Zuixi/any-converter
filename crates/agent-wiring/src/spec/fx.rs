//! `fx` specification (`~/.fx/settings.json`).
//!
//! `fx` reads OpenAI-chat-completions providers from its top-level `providers`
//! object, selects the active provider with `provider`, and resolves the model
//! through the `models` map. The provider entry declares its protocol, its
//! keyless auth mode, how tool choices are forwarded, and per-model metadata.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the `fx` spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.any-converter",
    "provider",
    "models.any-converter",
];

const PROVIDER: &str = super::support::PROVIDER;
const SETTINGS_FILE: &str = "settings.json";
const PROVIDERS_PATH: &str = "providers.any-converter";
const PROVIDER_KEY: &str = "provider";
const MODEL_PATH: &str = "models.any-converter";
/// `fx` refuses the whole settings file past this many models.
const MAX_MODELS: usize = 256;

/// `fx` wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct FxSpec;

impl FxSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".fx")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }
}

impl AgentSpec for FxSpec {
    fn id(&self) -> &'static str {
        "fx"
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

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[PROVIDER_KEY]),
        );

        let provider = json!({
            "protocol": "openai-chat-completions",
            "base_url": gateway_path(wiring.gateway_url, "v1"),
            "auth": { "type": "none" },
            "tool_choice_mode": "send",
            "model_metadata": model_metadata(wiring),
        });
        // `fx` composes `<provider>/<model ref>` itself, so the ref is bare.
        let model = wiring.default_model;
        let text = json::set(
            &settings,
            &[
                KV::new(PROVIDERS_PATH, provider),
                KV::new(PROVIDER_KEY, json!(PROVIDER)),
                KV::new(MODEL_PATH, json!(model)),
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
            let text = json::del(&current, &[PROVIDERS_PATH, MODEL_PATH])?;
            let text = prune_if_empty(&text, "providers")?;
            let text = prune_if_empty(&text, "models")?;
            let text = super::support::restore_keys(&text, &old, &[PROVIDER_KEY])?;
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
        let base = json::get(&settings, &format!("{PROVIDERS_PATH}.base_url"));
        let provider = json::get(&settings, PROVIDER_KEY);
        let model = json::get(&settings, MODEL_PATH);

        let named = provider.as_deref() == Some(PROVIDER);
        let selected = model.as_deref().is_some_and(|value| !value.is_empty());
        if base.is_none() && !named && !selected {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && named && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for path in [PROVIDER_KEY, MODEL_PATH] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
            }
            for key in ["base_url", "protocol"] {
                let path = format!("{PROVIDERS_PATH}.{key}");
                if let Some(value) = json::get(&settings, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
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

/// Per-model metadata keyed by catalog id.
///
/// Capped at [`MAX_MODELS`] because `fx` rejects the entire file beyond it.
fn model_metadata(wiring: &WiringCtx<'_>) -> Map<String, Value> {
    let mut models = Map::new();
    for model in wiring.catalog.models.iter().take(MAX_MODELS) {
        let mut entry = Map::new();
        entry.insert("supports_tool_use".to_owned(), json!(true));
        // The catalog carries no modality flags, so treat every model as
        // text-only rather than advertising vision we cannot confirm.
        entry.insert("supports_vision".to_owned(), json!(false));
        if let Some(context) = model.context_tokens {
            entry.insert("context_window".to_owned(), json!(context));
        }
        // `max_output_tokens` is omitted: the catalog has no output limit and
        // `fx` treats an absent value as unknown.
        models.insert(model.id.clone(), Value::Object(entry));
    }
    models
}
