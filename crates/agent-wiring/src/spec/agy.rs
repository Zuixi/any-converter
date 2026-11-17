//! Antigravity CLI (`agy`) specification.
//!
//! `agy` reads its endpoint and key only from the environment
//! (`GOOGLE_GEMINI_BASE_URL`, `GEMINI_API_KEY`); nothing in `settings.json`
//! can hold them. The launcher therefore keeps the user's `settings.json` untouched
//! and writes a separate settings file that a launcher passes with
//! `--settings`: `modelProvider: "gemini"`, `model` naming one of our custom
//! models, and `customModelsConfig.customModels` carrying the user's own
//! entries plus one per catalog model (`any-converter/<id>`).
//!
//! The launcher (see [`AgentSpec::launch`]) exports the two variables and
//! selects the model with `--model`, so a plain `agy` still runs on the
//! user's Google sign-in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, join_relative,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Antigravity CLI spec.
pub const OUR_KEYS: &[&str] = &[
    "override.modelProvider",
    "override.model",
    "override.customModelsConfig.customModels",
];

/// Provider id used as the model prefix.
pub const PREFIX: &str = "any-converter";
const OVERRIDE_NAME: &str = "any-converter-settings.json";
const MODEL_PROVIDER: &str = "modelProvider";
const MODEL: &str = "model";
const MODELS: &str = "customModelsConfig.customModels";

/// Antigravity CLI wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgySpec;

impl AgySpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".gemini/antigravity-cli")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }

    fn override_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(OVERRIDE_NAME)
    }

    /// Custom models the user already had in `settings.json`.
    fn own_models(settings: &str) -> Map<String, Value> {
        let Some(raw) = json::get(settings, MODELS) else {
            return Map::new();
        };
        let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(&raw) else {
            return Map::new();
        };
        entries
            .into_iter()
            .filter(|(key, _)| !key.starts_with(&format!("{PREFIX}/")))
            .collect()
    }
}

impl AgentSpec for AgySpec {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::settings_path(ctx).exists() || bin_in_path(ctx, "agy") {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::override_path(ctx), Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let settings = read_config(&Self::settings_path(ctx))?.unwrap_or_default();
        let override_path = Self::override_path(ctx);
        let existing = read_config(&override_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [MODEL_PROVIDER, MODEL] {
            if let Some(value) = json::get(&existing, key) {
                old.insert(format!("override.{key}"), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut models = Self::own_models(&settings);
        for model in &wiring.catalog.models {
            models.insert(
                format!("{PREFIX}/{}", model.id),
                json!({
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "modelName": model.id,
                }),
            );
        }

        let text = json::set(
            &existing,
            &[
                KV::new(MODEL_PROVIDER, json!("gemini")),
                KV::new(MODEL, json!(format!("{PREFIX}/{}", wiring.default_model))),
                KV::new(MODELS, Value::Object(models)),
            ],
        )?;
        let written = write_config(&override_path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let override_path = Self::override_path(ctx);
        let mut written = Vec::new();
        if override_path.exists() {
            super::remove_config(&override_path)?;
            written.push(override_path);
        }

        // Older builds wrote into `settings.json`; put the user's values
        // back and drop only our custom models.
        if let Some(current) = read_config(&Self::settings_path(ctx))? {
            let model = json::get(&current, MODEL).unwrap_or_default();
            if model.starts_with(&format!("{PREFIX}/")) {
                let old = load_stash(ctx).old_values(self.id());
                let mut text =
                    restore(&current, MODEL_PROVIDER, old.get("override.modelProvider"))?;
                text = restore(&text, MODEL, old.get("override.model"))?;
                let own = Self::own_models(&current);
                text = if own.is_empty() {
                    json::del(&text, &[MODELS])?
                } else {
                    json::set(&text, &[KV::new(MODELS, Value::Object(own))])?
                };
                text = prune_empty(&text, "customModelsConfig")?;
                written.push(write_config(&Self::settings_path(ctx), &text)?);
            }
        }

        let mut stash = load_stash(ctx);
        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, _gateway_url: &str) -> WiredState {
        let Ok(Some(text)) = read_config(&Self::override_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let provider = json::get(&text, MODEL_PROVIDER);
        let model = json::get(&text, MODEL).filter(|value| !value.is_empty());
        if provider.is_none() && model.is_none() {
            return WiredState::Disconnected;
        }
        if provider.as_deref() == Some("gemini")
            && model.is_some_and(|value| value.starts_with(&format!("{PREFIX}/")))
        {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(text)) = read_config(&Self::override_path(ctx)) {
            for key in [MODEL_PROVIDER, MODEL] {
                if let Some(value) = json::get(&text, key) {
                    values.insert(format!("override.{key}"), value);
                }
            }
        }
        values
    }

    fn launch(
        &self,
        ctx: &HomeCtx,
        gateway_url: &str,
        agent_key: &str,
        model: &str,
    ) -> Option<String> {
        let selected = if model.starts_with(&format!("{PREFIX}/")) {
            model.to_owned()
        } else {
            format!("{PREFIX}/{model}")
        };
        let settings = Self::override_path(ctx);
        let quote = |value: &str| value.replace('\'', "''");
        Some(if cfg!(windows) {
            format!(
                "$env:GEMINI_API_KEY=\"{}\"; $env:GOOGLE_GEMINI_BASE_URL=\"{}\"; agy --settings '{}' --model '{}'",
                agent_key,
                gateway_url,
                quote(&settings.display().to_string()),
                quote(&selected)
            )
        } else {
            format!(
                "GEMINI_API_KEY={} GOOGLE_GEMINI_BASE_URL={} agy --settings '{}' --model '{}'",
                agent_key,
                gateway_url,
                quote(&settings.display().to_string()),
                quote(&selected)
            )
        })
    }
}

fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
