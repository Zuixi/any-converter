//! GitHub Copilot for JetBrains BYOK specification.
//!
//! The plugin reads a small key/value document, `byok.json`, from the
//! `github-copilot` directory. An absolute `XDG_CONFIG_HOME` wins on every
//! platform; otherwise Windows uses `%LOCALAPPDATA%` and other platforms use
//! the XDG config home. Three top-level keys describe our provider, its key,
//! and one deployment entry per catalog model; disconnect removes only those
//! keys and leaves the file in place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

#[cfg(windows)]
use super::local_app_data_dir;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    join_relative, load_stash, read_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Copilot for JetBrains spec.
pub const OUR_KEYS: &[&str] = &[
    "any-converter-provider-config",
    "any-converter-api-key",
    "any-converter-models-config",
];

const PROVIDER: &str = super::support::PROVIDER;
const FILE_NAME: &str = "byok.json";
const PROVIDER_CONFIG: &str = "any-converter-provider-config";
const API_KEY: &str = "any-converter-api-key";
const MODELS_CONFIG: &str = "any-converter-models-config";
/// Input budget used when the catalog knows no context window.
const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;
/// Output budget, capped at a quarter of a known context window.
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 16_000;

/// GitHub Copilot for JetBrains wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CopilotJetBrainsSpec;

impl CopilotJetBrainsSpec {
    fn byok_path(ctx: &HomeCtx) -> PathBuf {
        join_relative(&Self::root(ctx), &format!("github-copilot/{FILE_NAME}"))
    }

    /// The platform config root, preferring an absolute `XDG_CONFIG_HOME`.
    fn root(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("XDG_CONFIG_HOME") {
            let trimmed = value.trim();
            if !trimmed.is_empty() && Path::new(trimmed).is_absolute() {
                return Path::new(trimmed).to_path_buf();
            }
        }
        #[cfg(windows)]
        {
            local_app_data_dir(ctx)
        }
        #[cfg(not(windows))]
        {
            // A relative XDG_CONFIG_HOME is ignored, like the language server.
            ctx.home.join(".config")
        }
    }
}

impl AgentSpec for CopilotJetBrainsSpec {
    fn id(&self) -> &'static str {
        "copilot-jetbrains"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let path = Self::byok_path(ctx);
        match path.parent() {
            Some(dir) if path.exists() || dir.is_dir() => DetectResult::Detected {
                config_dir: dir.to_path_buf(),
            },
            _ => DetectResult::Absent,
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::byok_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::byok_path(ctx);
        let config = read_config(&path)?.unwrap_or_default();
        // A model entry that already exists keeps its registration state.
        let existing = read_models_config(&config);

        let text = json::set(
            &config,
            &[
                KV::new(
                    PROVIDER_CONFIG,
                    json!({ "groupName": PROVIDER, "apiType": "chatCompletions" }),
                ),
                KV::new(API_KEY, json!(wiring.agent_key)),
                KV::new(
                    MODELS_CONFIG,
                    Value::Object(model_configs(wiring, &existing)),
                ),
            ],
        )?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::byok_path(ctx);
        let mut stash = load_stash(ctx);
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            // Only our three keys go; an emptied file is left in place.
            let text = json::del_pruning(&current, &[PROVIDER_CONFIG, API_KEY, MODELS_CONFIG])?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(config)) = read_config(&Self::byok_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let provider = json::get(&config, PROVIDER_CONFIG);
        let key = json::get(&config, API_KEY);
        let models = json::get(&config, MODELS_CONFIG);

        if provider.is_none() && key.is_none() && models.is_none() {
            return WiredState::Disconnected;
        }

        let chat = provider
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .is_some_and(|value| {
                value.get("apiType").and_then(Value::as_str) == Some("chatCompletions")
            });
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        let deployment = models
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .is_some_and(|value| {
                value.as_object().is_some_and(|entries| {
                    entries.values().any(|entry| {
                        entry
                            .get("deploymentUrl")
                            .and_then(Value::as_str)
                            .is_some_and(|url| gw_base_match(url, gateway_url))
                    })
                })
            });
        if chat && key && deployment {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::byok_path(ctx)) {
            for key in [PROVIDER_CONFIG, API_KEY, MODELS_CONFIG] {
                if let Some(value) = json::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        values
    }
}

/// Reads the models-config object already present in `byok.json`.
fn read_models_config(text: &str) -> Map<String, Value> {
    json::get(text, MODELS_CONFIG)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// One deployment entry per catalog model.
///
/// The catalog knows no context window for every model and no output limit at
/// all, so the input budget defaults to 128000 and the output budget is 16000
/// capped at a quarter of a known context. Vision is false because the catalog
/// carries no modality flags. A pre-existing entry keeps its `isRegistered`.
fn model_configs(wiring: &WiringCtx<'_>, existing: &Map<String, Value>) -> Map<String, Value> {
    let deployment = gateway_path(wiring.gateway_url, "v1/chat/completions");
    let mut models = Map::new();
    for model in &wiring.catalog.models {
        let input_tokens = model.context_tokens.unwrap_or(DEFAULT_MAX_INPUT_TOKENS);
        let output_tokens = model
            .context_tokens
            .map_or(DEFAULT_MAX_OUTPUT_TOKENS, |context| {
                DEFAULT_MAX_OUTPUT_TOKENS.min(context / 4)
            });

        let mut capabilities = Map::new();
        capabilities.insert("name".to_owned(), json!(model.name));
        capabilities.insert("maxInputTokens".to_owned(), json!(input_tokens));
        capabilities.insert("maxOutputTokens".to_owned(), json!(output_tokens));
        capabilities.insert("toolCalling".to_owned(), json!(true));
        capabilities.insert("vision".to_owned(), json!(false));

        let is_registered = existing
            .get(&model.id)
            .and_then(|entry| entry.get("isRegistered"))
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut entry = Map::new();
        entry.insert("deploymentUrl".to_owned(), json!(deployment));
        entry.insert("isRegistered".to_owned(), json!(is_registered));
        entry.insert("isCustomModel".to_owned(), json!(true));
        entry.insert("modelCapabilities".to_owned(), Value::Object(capabilities));
        models.insert(model.id.clone(), Value::Object(entry));
    }
    models
}
