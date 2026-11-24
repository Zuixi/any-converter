//! Crush specification (a provider config plus a model selection config).
//!
//! Crush keeps providers in `crush.json` and the selected large/small models in
//! a second `crush.json`. On Windows both live at
//! `%LOCALAPPDATA%/crush/crush.json`; elsewhere the provider file is
//! `<xdg config>/crush/crush.json` and the model file is
//! `<xdg data>/crush/crush.json`.
//!
//! The provider entry is OpenAI-compatible at the gateway's `/v1`. Both model
//! slots point at the selected catalog model, and the large slot carries the
//! reasoning effort. Pre-existing large/small objects are stashed so a user's
//! own selections come back on disconnect.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

#[cfg(windows)]
use super::local_app_data_dir;
#[cfg(not(windows))]
use super::{env_dir, join_relative, xdg_config_home};

/// Configuration keys owned by the Crush spec.
pub const OUR_KEYS: &[&str] = &["providers.any-converter", "models.large", "models.small"];

/// Provider id written into the config file.
pub const PROVIDER_ID: &str = "any-converter";

const PROVIDER_PATH: &str = "providers.any-converter";
const LARGE: &str = "models.large";
const SMALL: &str = "models.small";

/// Reasoning levels Crush accepts per model.
const EFFORTS: &[&str] = &["low", "medium", "high"];

/// Crush wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrushSpec;

impl CrushSpec {
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        #[cfg(windows)]
        {
            local_app_data_dir(ctx).join("crush").join("crush.json")
        }
        #[cfg(not(windows))]
        {
            xdg_config_home(ctx).join("crush").join("crush.json")
        }
    }

    fn data_path(ctx: &HomeCtx) -> PathBuf {
        #[cfg(windows)]
        {
            Self::config_path(ctx)
        }
        #[cfg(not(windows))]
        {
            let data_home = env_dir(ctx, "XDG_DATA_HOME", |ctx| {
                join_relative(&ctx.home, ".local/share")
            });
            data_home.join("crush").join("crush.json")
        }
    }
}

impl AgentSpec for CrushSpec {
    fn id(&self) -> &'static str {
        "crush"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::config_path(ctx)
            .parent()
            .map_or_else(|| Self::config_path(ctx), PathBuf::from);
        if Self::config_path(ctx).exists() || Self::data_path(ctx).exists() || config_dir.is_dir() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        let config_path = Self::config_path(ctx);
        let data_path = Self::data_path(ctx);
        if config_path == data_path {
            vec![config_path]
        } else {
            vec![config_path, data_path]
        }
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let data_path = Self::data_path(ctx);
        let same = config_path == data_path;
        let config_source = read_config(&config_path)?.unwrap_or_default();
        let data_source = if same {
            config_source.clone()
        } else {
            read_config(&data_path)?.unwrap_or_default()
        };

        let mut old = BTreeMap::new();
        for key in [LARGE, SMALL] {
            if let Some(value) = json::get(&data_source, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let provider = json!({
            "type": "openai",
            "name": PROVIDER_ID,
            "base_url": gateway_path(wiring.gateway_url, "v1"),
            "api_key": wiring.agent_key,
            "models": provider_models(wiring),
        });
        let config_text = json::set(&config_source, &[KV::new(PROVIDER_PATH, provider)])?;

        // Reconnecting with no effort selected keeps the effort this entry
        // already carries; an explicit empty effort means the user cleared it.
        let existing_effort = if json::get(&data_source, &format!("{LARGE}.provider")).as_deref()
            == Some(PROVIDER_ID)
        {
            json::get(&data_source, &format!("{LARGE}.reasoning_effort"))
        } else {
            None
        };

        let data_source = if same { &config_text } else { &data_source };
        let data_text = json::set(
            data_source,
            &[
                KV::new(LARGE, large_model(wiring, existing_effort)),
                KV::new(SMALL, small_model(wiring)),
            ],
        )?;

        let written = if same {
            vec![write_config(&config_path, &data_text)?]
        } else {
            vec![
                write_config(&config_path, &config_text)?,
                write_config(&data_path, &data_text)?,
            ]
        };
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let data_path = Self::data_path(ctx);
        let same = config_path == data_path;
        let config_current = read_config(&config_path)?;
        let data_current = if same {
            config_current.clone()
        } else {
            read_config(&data_path)?
        };
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = data_current {
            let mut data_text = restore_json(&current, LARGE, old.get(LARGE))?;
            data_text = restore_json(&data_text, SMALL, old.get(SMALL))?;
            data_text = prune_empty(&data_text, "models")?;
            let referenced =
                model_references(&data_text, LARGE) || model_references(&data_text, SMALL);

            if same {
                if !referenced {
                    data_text = remove_provider(&data_text)?;
                }
                written.push(write_config(&data_path, &data_text)?);
            } else {
                if let Some(text) = config_current {
                    let text = if referenced {
                        text
                    } else {
                        remove_provider(&text)?
                    };
                    written.push(write_config(&config_path, &text)?);
                }
                written.push(write_config(&data_path, &data_text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let data = read_config(&Self::data_path(ctx)).ok().flatten();
        let config = read_config(&Self::config_path(ctx)).ok().flatten();
        if data.is_none() && config.is_none() {
            return WiredState::Disconnected;
        }
        let data = data.unwrap_or_default();
        let provider = json::get(&data, &format!("{LARGE}.provider"));
        let model = json::get(&data, &format!("{LARGE}.model"));
        let base = config.and_then(|text| json::get(&text, &format!("{PROVIDER_PATH}.base_url")));
        let selected = provider.as_deref() == Some(PROVIDER_ID)
            && model.is_some_and(|value| !value.is_empty());
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        if !selected && base.is_none() {
            return WiredState::Disconnected;
        }
        if selected && gateway {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            if let Some(value) = json::get(&config, &format!("{PROVIDER_PATH}.base_url")) {
                values.insert("providers.any-converter.base_url".to_owned(), value);
            }
        }
        if let Ok(Some(data)) = read_config(&Self::data_path(ctx)) {
            for path in [format!("{LARGE}.provider"), format!("{LARGE}.model")] {
                if let Some(value) = json::get(&data, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Provider model entries as Crush reads them.
///
/// A model's window defaults to 200000 and its output to 16384 (or the
/// window, when smaller) when the catalog does not know them; without
/// `default_max_tokens` Crush caps every reply at 16384 tokens anyway.
/// Pricing is not part of this catalog, so Crush's `cost_per_1m_*` fields
/// are omitted (the session then shows no cost).
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let window = model.context_tokens.unwrap_or(200_000);
            let mut entry = Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("name".to_owned(), json!(model.name));
            entry.insert("context_window".to_owned(), json!(window));
            entry.insert("default_max_tokens".to_owned(), json!(16_384.min(window)));
            entry.insert("can_reason".to_owned(), json!(model.reasoning));
            Value::Object(entry)
        })
        .collect()
}

/// The large model slot, carrying the effort when one was selected.
///
/// With no effort supplied, a previous entry of ours keeps its
/// `reasoning_effort`; an empty effort clears it.
fn large_model(wiring: &WiringCtx<'_>, existing_effort: Option<String>) -> Value {
    let mut entry = Map::new();
    entry.insert("provider".to_owned(), json!(PROVIDER_ID));
    entry.insert("model".to_owned(), json!(wiring.default_model));
    match wiring.effort {
        Some("") => {}
        Some(effort) => {
            entry.insert("reasoning_effort".to_owned(), json!(effort));
        }
        None => {
            if let Some(effort) = existing_effort.filter(|effort| !effort.is_empty()) {
                entry.insert("reasoning_effort".to_owned(), json!(effort));
            }
        }
    }
    Value::Object(entry)
}

/// The small model slot.
fn small_model(wiring: &WiringCtx<'_>) -> Value {
    json!({ "provider": PROVIDER_ID, "model": wiring.default_model })
}

/// Whether a model slot points at our provider.
fn model_references(text: &str, slot: &str) -> bool {
    json::get(text, &format!("{slot}.provider")).as_deref() == Some(PROVIDER_ID)
}

/// Restores a stashed model object, or removes the slot when there was none.
fn restore_json(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let parsed: Value = serde_json::from_str(value)
                .map_err(|error| WiringError::Refused(format!("invalid stashed value: {error}")))?;
            Ok(json::set(text, &[KV::new(path, parsed)])?)
        }
        _ => Ok(json::del(text, &[path])?),
    }
}

/// Removes the provider entry and its emptied container.
fn remove_provider(text: &str) -> Result<String, WiringError> {
    let text = json::del(text, &[PROVIDER_PATH])?;
    prune_empty(&text, "providers")
}

/// Removes `path` when it is now an empty object.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
