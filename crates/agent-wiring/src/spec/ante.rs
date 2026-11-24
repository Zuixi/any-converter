//! Ante specification (`$ANTE_HOME` or `~/.ante`).
//!
//! `catalog.json` describes OpenAI-compatible providers and the models each
//! one prefers; `settings.json` selects the provider and the bare model id a
//! session starts on. Ante declares supported reasoning efforts per model but
//! has no writable effort setting, so the spec only advertises them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Ante spec.
pub const OUR_KEYS: &[&str] = &[
    "catalog.providers.any-converter",
    "settings.provider",
    "settings.model",
];

const PROVIDER: &str = super::support::PROVIDER;
const CATALOG_FILE: &str = "catalog.json";
const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_PATH: &str = "providers.any-converter";
const SETTING_PROVIDER: &str = "provider";
const SETTING_MODEL: &str = "model";

/// Reasoning levels Ante accepts, in display order.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Ante wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnteSpec;

impl AnteSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "ANTE_HOME", |ctx| ctx.home.join(".ante"))
    }

    fn catalog_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(CATALOG_FILE)
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }
}

impl AgentSpec for AnteSpec {
    fn id(&self) -> &'static str {
        "ante"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::catalog_path(ctx).exists()
            || Self::settings_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::catalog_path(ctx), Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let catalog_path = Self::catalog_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let catalog = read_config(&catalog_path)?.unwrap_or_default();
        let settings = read_config(&settings_path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[SETTING_PROVIDER, SETTING_MODEL]),
        );

        let provider = json!({
            "display_name": PROVIDER,
            "base_url": gateway_path(wiring.gateway_url, "v1"),
            "wire_style": "OpenAiCompatible",
            "preferred_models": preferred_models(wiring),
        });
        let catalog_text = json::set(&catalog, &[KV::new(PROVIDER_PATH, provider)])?;
        let settings_text = json::set(
            &settings,
            &[
                KV::new(SETTING_PROVIDER, json!(PROVIDER)),
                KV::new(SETTING_MODEL, json!(wiring.default_model)),
            ],
        )?;

        let written_catalog = write_config(&catalog_path, &catalog_text)?;
        let written_settings = write_config(&settings_path, &settings_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_catalog, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let catalog_path = Self::catalog_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&catalog_path)? {
            let text = json::del(&current, &[PROVIDER_PATH])?;
            let text = prune_if_empty(&text, "providers")?;
            written.push(write_config(&catalog_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            // A user who pointed Ante elsewhere after we connected keeps their
            // choice: only restore over a selection that is still ours.
            let ours = json::get(&current, SETTING_PROVIDER).as_deref() == Some(PROVIDER);
            if ours {
                let text = super::support::restore_keys(
                    &current,
                    &old,
                    &[SETTING_PROVIDER, SETTING_MODEL],
                )?;
                written.push(write_config(&settings_path, &text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let catalog = read_config(&Self::catalog_path(ctx)).ok().flatten();
        let settings = read_config(&Self::settings_path(ctx)).ok().flatten();
        let base = catalog
            .as_deref()
            .and_then(|text| json::get(text, &format!("{PROVIDER_PATH}.base_url")));
        let provider = settings
            .as_deref()
            .and_then(|text| json::get(text, SETTING_PROVIDER));
        let model = settings
            .as_deref()
            .and_then(|text| json::get(text, SETTING_MODEL));

        let named = provider.as_deref() == Some(PROVIDER);
        if base.is_none() && !named {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let selected = model.as_deref().is_some_and(|value| !value.is_empty());
        if gateway && named && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(catalog)) = read_config(&Self::catalog_path(ctx)) {
            for key in ["display_name", "base_url"] {
                let path = format!("{PROVIDER_PATH}.{key}");
                if let Some(value) = json::get(&catalog, &path) {
                    values.insert(path, value);
                }
            }
        }
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in [SETTING_PROVIDER, SETTING_MODEL] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
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

/// The model list Ante shows for our provider.
fn preferred_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            // Ante omits the display name when it adds nothing over the id.
            if !model.name.is_empty() && model.name != model.id {
                entry.insert("display_name".to_owned(), json!(model.name));
            }
            if let Some(context) = model.context_tokens {
                entry.insert("context_limit".to_owned(), json!(context));
                entry.insert(
                    "max_tokens".to_owned(),
                    json!(context.saturating_sub(1).min(16384)),
                );
            }
            // The catalog carries no per-model effort list, so reasoning
            // models advertise the full generic ladder mapped onto Ante's
            // names; models without reasoning support advertise none.
            if model.reasoning {
                let efforts: Vec<&str> = EFFORTS.iter().map(|effort| ante_effort(effort)).collect();
                entry.insert("supported_efforts".to_owned(), json!(efforts));
            }
            Value::Object(entry)
        })
        .collect()
}

/// Maps one generic effort name onto the name Ante accepts.
fn ante_effort(effort: &str) -> &str {
    match effort {
        "minimal" => "min",
        other => other,
    }
}
