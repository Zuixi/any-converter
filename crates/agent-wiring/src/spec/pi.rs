//! Pi specification (`PI_CODING_AGENT_DIR` or `~/.pi/agent`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path,
    is_gateway_base, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Pi spec.
pub const OUR_KEYS: &[&str] = &[
    "models.providers.any-converter",
    "settings.defaultProvider",
    "settings.defaultModel",
    "settings.enabledModels",
];

const PROVIDER: &str = "any-converter";

/// Pi wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct PiSpec;

impl PiSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "PI_CODING_AGENT_DIR", |home| {
            home.home.join(".pi").join("agent")
        })
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }

    fn models_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("models.json")
    }
}

impl AgentSpec for PiSpec {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::settings_path(ctx).exists()
            || Self::models_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx), Self::models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = Self::settings_path(ctx);
        let models_path = Self::models_path(ctx);
        let settings = read_config(&settings_path)?.unwrap_or_default();
        let models = read_config(&models_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in ["defaultProvider", "defaultModel", "enabledModels"] {
            if let Some(value) = json::get(&settings, key) {
                old.insert(format!("settings.{key}"), value);
            }
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let provider = json!({
            "name": PROVIDER,
            "baseUrl": gateway_path(wiring.gateway_url, "v1"),
            "apiKey": wiring.agent_key,
            "models": provider_models(wiring),
        });
        let models_text = json::set(&models, &[KV::new("providers.any-converter", provider)])?;

        let mut settings_text = json::set(
            &settings,
            &[
                KV::new("defaultProvider", json!(PROVIDER)),
                KV::new("defaultModel", json!(wiring.default_model)),
            ],
        )?;
        settings_text = append_enabled_model(&settings_text, wiring.default_model)?;

        let written_models = write_config(&models_path, &models_text)?;
        let written_settings = write_config(&settings_path, &settings_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_models, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = Self::settings_path(ctx);
        let models_path = Self::models_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&models_path)? {
            let text = json::del(&current, &["providers.any-converter"])?;
            written.push(write_config(&models_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let mut sets = Vec::new();
            let mut deletes = Vec::new();
            for key in ["defaultProvider", "defaultModel"] {
                match old.get(&format!("settings.{key}")) {
                    Some(value) if !value.is_empty() => {
                        sets.push(KV::new(key, json!(value)));
                    }
                    _ => deletes.push(key),
                }
            }
            let mut text = json::set(&current, &sets)?;
            text = json::del(&text, &deletes)?;
            text = strip_enabled_models(&text)?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx) -> WiredState {
        let Ok(Some(models)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&models, "providers.any-converter.baseUrl");
        let key = json::get(&models, "providers.any-converter.apiKey");
        let gateway = base.as_deref().is_some_and(is_gateway_base);
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn base_url_key(&self) -> Option<&'static str> {
        Some("providers.any-converter.baseUrl")
    }

    fn read_key(&self, ctx: &HomeCtx, key: &str) -> Option<String> {
        let models = read_config(&Self::models_path(ctx)).ok().flatten()?;
        json::get(&models, key)
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in ["defaultProvider", "defaultModel", "enabledModels"] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(format!("settings.{key}"), value);
                }
            }
        }
        if let Ok(Some(models)) = read_config(&Self::models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json::get(&models, &format!("providers.any-converter.{key}")) {
                    values.insert(format!("models.providers.any-converter.{key}"), value);
                }
            }
        }
        values
    }
}

fn provider_models(wiring: &WiringCtx<'_>) -> Map<String, Value> {
    let mut models = Map::new();
    for model in &wiring.catalog.models {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(model.name));
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if let Some(context) = model.context_tokens {
            entry.insert("context".to_owned(), json!(context));
        }
        models.insert(model.id.clone(), Value::Object(entry));
    }
    models
}

fn append_enabled_model(settings: &str, default_model: &str) -> Result<String, WiringError> {
    let Some(raw) = json::get(settings, "enabledModels") else {
        return Ok(settings.to_owned());
    };
    let Ok(Value::Array(mut items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(settings.to_owned());
    };
    let reference = format!("{PROVIDER}/{default_model}");
    if items
        .iter()
        .any(|item| item.as_str() == Some(reference.as_str()))
    {
        return Ok(settings.to_owned());
    }
    items.push(Value::String(reference));
    Ok(json::set(
        settings,
        &[KV::new("enabledModels", Value::Array(items))],
    )?)
}

fn strip_enabled_models(settings: &str) -> Result<String, WiringError> {
    let Some(raw) = json::get(settings, "enabledModels") else {
        return Ok(settings.to_owned());
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(settings.to_owned());
    };
    let mut kept = Vec::new();
    for item in items {
        let ours = item
            .as_str()
            .is_some_and(|value| value.starts_with(&format!("{PROVIDER}/")));
        if !ours {
            kept.push(item);
        }
    }
    if kept.is_empty() {
        Ok(json::del(settings, &["enabledModels"])?)
    } else {
        Ok(json::set(
            settings,
            &[KV::new("enabledModels", Value::Array(kept))],
        )?)
    }
}
