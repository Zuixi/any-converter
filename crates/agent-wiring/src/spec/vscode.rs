//! VS Code / Insiders / VSCodium specification.
//!
//! Chat custom endpoints live in `chatLanguageModels.json` next to the user's
//! `settings.json` (`%APPDATA%/<product>/User` on Windows, `~/Library/
//! Application Support/<product>/User` on macOS, `$XDG_CONFIG_HOME/<product>/
//! User` elsewhere). The editor reads one provider group with a list of
//! OpenAI chat-completions models; `chat.defaultModel` selects the one a new
//! chat starts on, so both files are wired.
//!
//! VSCodium's fork uses the `customoai` vendor with `x-api-key` headers,
//! while VS Code and Insiders use `customendpoint`; each installed product
//! gets the shape it understands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, app_data_dir, bin_in_path,
    gateway_path, gw_base_match, load_stash, read_config, record_old_values, save_stash,
    write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the VS Code spec.
pub const OUR_KEYS: &[&str] = &[
    "settings.chat\\.defaultModel",
    "chatLanguageModels.any-converter",
];

const GROUP_NAME: &str = "Any Converter";
const DEFAULT_MODEL: &str = "chat\\.defaultModel";
/// Products in the order VS Code's own "stable" install prefers them.
const VARIANTS: &[&str] = &["Code", "Code - Insiders", "VSCodium"];

/// VS Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct VSCodeSpec;

impl VSCodeSpec {
    /// Every installed product's `User` directory.
    fn dirs(ctx: &HomeCtx) -> Vec<PathBuf> {
        let root = app_data_dir(ctx);
        let installed: Vec<PathBuf> = VARIANTS
            .iter()
            .map(|name| root.join(name).join("User"))
            .filter(|dir| dir.is_dir())
            .collect();
        if !installed.is_empty() {
            return installed;
        }
        // A CLI on PATH without a user directory yet: pick the matching
        // product so the first connect creates the file the editor reads.
        let name = if bin_in_path(ctx, "codium") {
            "VSCodium"
        } else if bin_in_path(ctx, "code-insiders") {
            "Code - Insiders"
        } else {
            "Code"
        };
        vec![root.join(name).join("User")]
    }

    fn configured_dirs(ctx: &HomeCtx) -> Vec<PathBuf> {
        Self::dirs(ctx)
            .into_iter()
            .filter(|dir| dir.is_dir())
            .collect()
    }
}

impl AgentSpec for VSCodeSpec {
    fn id(&self) -> &'static str {
        "vscode"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let installed = Self::configured_dirs(ctx);
        if let Some(config_dir) = installed.first() {
            return DetectResult::Detected {
                config_dir: config_dir.clone(),
            };
        }
        let path_present = ["code", "code-insiders", "codium"]
            .iter()
            .any(|name| bin_in_path(ctx, name));
        if path_present {
            DetectResult::Detected {
                config_dir: Self::dirs(ctx).remove(0),
            }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        Self::dirs(ctx)
            .into_iter()
            .flat_map(|dir| {
                [
                    dir.join("settings.json"),
                    dir.join("chatLanguageModels.json"),
                ]
            })
            .collect()
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let mut written = Vec::new();
        let mut old = BTreeMap::new();
        let mut stash = load_stash(ctx);

        for dir in Self::dirs(ctx) {
            let settings_path = dir.join("settings.json");
            let models_path = dir.join("chatLanguageModels.json");
            let settings = read_config(&settings_path)?.unwrap_or_default();
            let models = read_config(&models_path)?.unwrap_or_default();

            if let Some(value) = json::get(&settings, DEFAULT_MODEL) {
                old.insert("settings.chat\\.defaultModel".to_owned(), value);
            }

            let next_settings = json::set(
                &settings,
                &[KV::new(DEFAULT_MODEL, json!(wiring.default_model))],
            )?;
            let next_models = json::set(
                &models,
                &[KV::new(
                    "models",
                    Value::Array(
                        user_groups(&models)
                            .into_iter()
                            .chain([group(&dir, wiring)])
                            .collect(),
                    ),
                )],
            )?;

            written.push(write_config(&settings_path, &next_settings)?);
            written.push(write_config(&models_path, &next_models)?);
        }

        record_old_values(&mut stash, self.id(), old);
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let mut written = Vec::new();
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());

        for dir in Self::dirs(ctx) {
            let settings_path = dir.join("settings.json");
            let models_path = dir.join("chatLanguageModels.json");

            if let Some(current) = read_config(&settings_path)? {
                let text = match old.get("settings.chat\\.defaultModel") {
                    Some(value) if !value.is_empty() => {
                        json::set(&current, &[KV::new(DEFAULT_MODEL, json!(value))])?
                    }
                    _ => json::del(&current, &[DEFAULT_MODEL])?,
                };
                written.push(write_config(&settings_path, &text)?);
            }

            if let Some(current) = read_config(&models_path)? {
                let kept = user_groups(&current);
                let text = if kept.is_empty() {
                    json::del(&current, &["models"])?
                } else {
                    json::set(&current, &[KV::new("models", Value::Array(kept))])?
                };
                written.push(write_config(&models_path, &text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let mut found = false;
        let mut connected = false;
        for dir in Self::configured_dirs(ctx) {
            let settings = read_config(&dir.join("settings.json")).ok().flatten();
            let models = read_config(&dir.join("chatLanguageModels.json"))
                .ok()
                .flatten();
            let Some(models) = models else { continue };
            let Some(group) = group_value(&models) else {
                continue;
            };
            found = true;
            let base_ok = group_base(&group).is_some_and(|url| gw_base_match(&url, gateway_url));
            let key_ok = group_key_present(&group);
            let selected = settings
                .as_deref()
                .and_then(|text| json::get(text, DEFAULT_MODEL))
                .is_some_and(|value| !value.is_empty());
            if base_ok && key_ok && selected {
                connected = true;
            }
        }
        if connected {
            WiredState::Connected
        } else if found {
            WiredState::Drifted
        } else {
            WiredState::Disconnected
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        for dir in Self::configured_dirs(ctx) {
            if let Ok(Some(settings)) = read_config(&dir.join("settings.json")) {
                if let Some(value) = json::get(&settings, DEFAULT_MODEL) {
                    values.insert(DEFAULT_MODEL.to_owned(), value);
                }
            }
            if let Ok(Some(models)) = read_config(&dir.join("chatLanguageModels.json")) {
                if let Some(group) = group_value(&models) {
                    values.insert("chatLanguageModels.any-converter".to_owned(), group);
                }
            }
        }
        values
    }
}

/// Groups in `models` that this spec did not write.
fn user_groups(text: &str) -> Vec<Value> {
    let Some(raw) = json::get(text, "models") else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) != Some(GROUP_NAME))
        .collect()
}

/// This spec's group as compact JSON, when present.
fn group_value(text: &str) -> Option<String> {
    let raw = json::get(text, "models")?;
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return None;
    };
    items
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(GROUP_NAME))
        .and_then(|entry| serde_json::to_string(&entry).ok())
}

fn group_base(group: &str) -> Option<String> {
    let value: Value = serde_json::from_str(group).ok()?;
    value
        .get("models")?
        .as_array()?
        .first()?
        .get("url")?
        .as_str()
        .map(str::to_owned)
}

fn group_key_present(group: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(group) else {
        return false;
    };
    value
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| models.first())
        .and_then(|model| model.get("requestHeaders"))
        .and_then(Value::as_object)
        .is_some_and(|headers| {
            headers
                .values()
                .any(|value| value.as_str().is_some_and(|text| !text.is_empty()))
        })
}

fn group(dir: &Path, wiring: &WiringCtx<'_>) -> Value {
    let vscodium = dir
        .parent()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        == Some("VSCodium");
    let url = gateway_path(wiring.gateway_url, "v1/chat/completions");
    let headers = if vscodium {
        json!({ "x-api-key": wiring.agent_key })
    } else {
        json!({ "Authorization": format!("Bearer {}", wiring.agent_key) })
    };
    let models: Vec<Value> = wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("name".to_owned(), json!(model.name));
            entry.insert("url".to_owned(), json!(url));
            entry.insert("toolCalling".to_owned(), json!(true));
            entry.insert("vision".to_owned(), json!(false));
            if let Some(context) = model.context_tokens {
                entry.insert("maxInputTokens".to_owned(), json!(context));
            }
            entry.insert("maxOutputTokens".to_owned(), json!(8192));
            entry.insert("requestHeaders".to_owned(), headers.clone());
            Value::Object(entry)
        })
        .collect();
    if vscodium {
        json!({ "name": GROUP_NAME, "vendor": "customoai", "models": models })
    } else {
        json!({
            "name": GROUP_NAME,
            "vendor": "customendpoint",
            "apiType": "chat-completions",
            "models": models,
        })
    }
}
