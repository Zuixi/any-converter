//! Paimon specification (`$PAIMON_CONFIG_HOME/config.json`).
//!
//! Paimon routes OpenAI-compatible models through a LiteLLM provider block.
//! The selected model is `litellm:<id>`, the provider names the gateway as its
//! `api_base` with the agent key, and the compaction context window mirrors the
//! model's catalog context when one is known. `$PAIMON_CONFIG_HOME` only
//! applies when absolute; otherwise Paimon always reads `~/.config/paimon`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, remove_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Paimon spec.
pub const OUR_KEYS: &[&str] = &["model", "providers.litellm", "compaction.context_window"];

const PROVIDER: &str = "litellm";
const CONFIG_FILE: &str = "config.json";
const MODEL: &str = "model";
const LITELLM: &str = "providers.litellm";
const CONTEXT_WINDOW: &str = "compaction.context_window";
/// Stash key marking a config file this spec created from nothing.
const CREATED_KEY: &str = "__created";

/// Paimon wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct PaimonSpec;

impl PaimonSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        // A relative override is ignored; XDG_CONFIG_HOME is never consulted.
        if let Some(value) = (ctx.env)("PAIMON_CONFIG_HOME") {
            let trimmed = value.trim();
            if !trimmed.is_empty() && Path::new(trimmed).is_absolute() {
                return PathBuf::from(trimmed);
            }
        }
        ctx.home.join(".config").join("paimon")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(CONFIG_FILE)
    }
}

impl AgentSpec for PaimonSpec {
    fn id(&self) -> &'static str {
        "paimon"
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
            super::support::record_keys(&config, &[MODEL, LITELLM, CONTEXT_WINDOW]),
        );
        if created {
            let mut marker = BTreeMap::new();
            marker.insert(CREATED_KEY.to_owned(), "true".to_owned());
            stash.record(self.id(), marker);
        }

        let mut kvs = vec![
            KV::new(MODEL, json!(format!("{PROVIDER}:{}", wiring.default_model))),
            KV::new(
                LITELLM,
                json!({
                    "api_base": gateway_path(wiring.gateway_url, "v1"),
                    "api_key": wiring.agent_key,
                }),
            ),
        ];
        // Without a catalog window keep the user's own value; if they had none
        // either, an older window we wrote must not linger.
        let fallback = stash
            .old_values(self.id())
            .get(CONTEXT_WINDOW)
            .filter(|value| !value.is_empty())
            .map(|raw| parse_value(raw));
        let window = context_window(wiring)
            .map(|context| json!(context))
            .or(fallback);
        if let Some(window) = &window {
            kvs.push(KV::new(CONTEXT_WINDOW, window.clone()));
        }
        let mut text = json::set(&config, &kvs)?;
        if window.is_none() {
            text = json::del_pruning(&text, &[CONTEXT_WINDOW])?;
        }
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
            let mut text = current;
            for key in [MODEL, LITELLM, CONTEXT_WINDOW] {
                text = restore_key(&text, key, &old)?;
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
        let base = json::get(&config, &format!("{LITELLM}.api_base"));
        let key = json::get(&config, &format!("{LITELLM}.api_key"));
        let model = json::get(&config, MODEL);

        let prefix = format!("{PROVIDER}:");
        let selected = model
            .as_deref()
            .is_some_and(|value| value.starts_with(&prefix));
        if base.is_none() && key.is_none() && !selected {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        if gateway && key && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for path in [MODEL, CONTEXT_WINDOW] {
                if let Some(value) = json::get(&config, path) {
                    values.insert(path.to_owned(), value);
                }
            }
            for key in ["api_base", "api_key"] {
                let path = format!("{LITELLM}.{key}");
                if let Some(value) = json::get(&config, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }
}

/// The selected model's context window, when the catalog knows it.
fn context_window(wiring: &WiringCtx<'_>) -> Option<u64> {
    wiring
        .catalog
        .find(wiring.default_model)
        .and_then(|model| model.context_tokens)
}

/// Restores a stashed value, or deletes the key when it was absent.
fn restore_key(
    text: &str,
    key: &str,
    old: &BTreeMap<String, String>,
) -> Result<String, WiringError> {
    match old.get(key) {
        Some(raw) if !raw.is_empty() => Ok(json::set(text, &[KV::new(key, parse_value(raw))])?),
        _ => {
            let text = json::del(text, &[key])?;
            match key.rsplit_once('.') {
                Some((parent, _)) => prune_if_empty(&text, parent),
                None => Ok(text),
            }
        }
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

/// Parses a stashed compact JSON value, keeping plain strings intact.
fn parse_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn is_empty_object(text: &str) -> bool {
    json::get(text, "").as_deref() == Some("{}")
}
