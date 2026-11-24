//! Nanobot specification (`~/.nanobot/config.json`).
//!
//! Nanobot keeps OpenAI-compatible providers under `providers` and selects the
//! agent default through `agents.defaults.provider` / `agents.defaults.model`.
//! A model preset (`agents.defaults.modelPreset` or the snake_case spelling)
//! overrides that pair when present, so connect clears both spellings and
//! records them first for disconnect to restore.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, remove_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Nanobot spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.any-converter",
    "agents.defaults.provider",
    "agents.defaults.model",
    "agents.defaults.modelPreset",
    "agents.defaults.model_preset",
    "agents.defaults.contextWindowTokens",
    "agents.defaults.context_window_tokens",
];

const PROVIDER: &str = super::support::PROVIDER;
const CONFIG_FILE: &str = "config.json";
const PROVIDER_PATH: &str = "providers.any-converter";
const DEFAULT_PROVIDER: &str = "agents.defaults.provider";
const DEFAULT_MODEL: &str = "agents.defaults.model";
/// Preset keys that override the provider/model pair when Nanobot resolves one.
const MODEL_PRESET: &str = "agents.defaults.modelPreset";
const MODEL_PRESET_SNAKE: &str = "agents.defaults.model_preset";
/// Context window Nanobot reads for the selected default model, in both
/// spellings Nanobot accepts.
const CONTEXT_WINDOW: &str = "agents.defaults.contextWindowTokens";
const CONTEXT_WINDOW_SNAKE: &str = "agents.defaults.context_window_tokens";
/// Stash key marking a config file this spec created from nothing.
const CREATED_KEY: &str = "__created";

/// Keys stashed before connect and restored on disconnect.
///
/// The preset pair is cleared on connect because `Config.resolve_preset`
/// overrides the provider/model we select; both spellings must be recorded
/// first so the user's preset comes back on disconnect.
const STASH_KEYS: &[&str] = &[
    DEFAULT_PROVIDER,
    DEFAULT_MODEL,
    MODEL_PRESET,
    MODEL_PRESET_SNAKE,
    CONTEXT_WINDOW,
    CONTEXT_WINDOW_SNAKE,
];

/// Nanobot wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct NanobotSpec;

impl NanobotSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".nanobot")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(CONFIG_FILE)
    }
}

impl AgentSpec for NanobotSpec {
    fn id(&self) -> &'static str {
        "nanobot"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::config_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let current = read_config(&path)?;
        let created = current.is_none();
        let config = current.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&config, STASH_KEYS),
        );
        if created {
            let mut marker = BTreeMap::new();
            marker.insert(CREATED_KEY.to_owned(), "true".to_owned());
            stash.record(self.id(), marker);
        }

        let mut kvs = vec![
            KV::new(
                PROVIDER_PATH,
                json!({
                    "apiKey": wiring.agent_key,
                    "apiBase": gateway_path(wiring.gateway_url, "v1"),
                    "extraHeaders": { "User-Agent": "nanobot" },
                }),
            ),
            KV::new(DEFAULT_PROVIDER, json!(PROVIDER)),
            KV::new(
                DEFAULT_MODEL,
                json!(format!("{PROVIDER}/{}", wiring.default_model)),
            ),
        ];
        if let Some(context) = context_window(wiring) {
            kvs.push(KV::new(CONTEXT_WINDOW, json!(context)));
        }
        // A preset takes precedence over the pair above, and a snake_case
        // window value would shadow ours; both spellings of each are stashed,
        // so disconnect puts the user's values back.
        let text = json::del(
            &config,
            &[
                MODEL_PRESET,
                MODEL_PRESET_SNAKE,
                CONTEXT_WINDOW,
                CONTEXT_WINDOW_SNAKE,
            ],
        )?;
        let text = json::set(&text, &kvs)?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let created = old.contains_key(CREATED_KEY);
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let text = json::del(&current, &[PROVIDER_PATH])?;
            let text = prune_if_empty(&text, "providers")?;
            let text = super::support::restore_keys(
                &text,
                &old,
                &[
                    DEFAULT_PROVIDER,
                    DEFAULT_MODEL,
                    MODEL_PRESET,
                    MODEL_PRESET_SNAKE,
                ],
            )?;
            // The context window is numeric; the generic restorer only writes
            // strings, so restore both spellings through parsed stash values.
            let mut text = text;
            for key in [CONTEXT_WINDOW, CONTEXT_WINDOW_SNAKE] {
                text = match old.get(key) {
                    Some(raw) if !raw.is_empty() => {
                        json::set(&text, &[KV::new(key, parse_value(raw))])?
                    }
                    _ => json::del_pruning(&text, &[key])?,
                };
            }
            if created && is_empty_object(&text) {
                remove_config(&path)?;
                written.push(path);
            } else {
                written.push(write_config(&path, &text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(config)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&config, &format!("{PROVIDER_PATH}.apiBase"));
        let key = json::get(&config, &format!("{PROVIDER_PATH}.apiKey"));
        let provider = json::get(&config, DEFAULT_PROVIDER);
        let model = json::get(&config, DEFAULT_MODEL);

        let prefix = format!("{PROVIDER}/");
        let named = provider.as_deref() == Some(PROVIDER);
        let selected = model
            .as_deref()
            .is_some_and(|value| value.starts_with(&prefix));
        if base.is_none() && key.is_none() && !named && !selected {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        if gateway && key && named && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            let paths = [
                DEFAULT_PROVIDER.to_owned(),
                DEFAULT_MODEL.to_owned(),
                format!("{PROVIDER_PATH}.apiBase"),
            ];
            for path in paths {
                if let Some(value) = json::get(&config, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }
}

/// Parses a stashed compact JSON value, keeping plain strings intact.
fn parse_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()))
}

/// The selected model's context window, when the catalog knows it.
fn context_window(wiring: &WiringCtx<'_>) -> Option<u64> {
    wiring
        .catalog
        .find(wiring.default_model)
        .and_then(|model| model.context_tokens)
}

/// Deletes `path` when our removal left it literally empty.
fn prune_if_empty(text: &str, path: &str) -> Result<String, WiringError> {
    if json::get(text, path).as_deref() == Some("{}") {
        Ok(json::del(text, &[path])?)
    } else {
        Ok(text.to_owned())
    }
}

fn is_empty_object(text: &str) -> bool {
    json::get(text, "").as_deref() == Some("{}")
}
