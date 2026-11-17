//! Antigravity (desktop) specification.
//!
//! Antigravity 2.30+ keeps its settings in `~/.gemini/config/config.json`;
//! earlier builds use `~/.antigravity/settings.json`. Both read the same
//! `gateway` block (OpenAI wire protocol plus the agent key) and a
//! `models.listSource` pointing at the gateway's model list, with
//! `models.defaultModelId` selecting the starting model. A legacy inline
//! `models.list` would outrank the list source, so it is removed on connect.
//!
//! The same block is written to both locations; a file that did not exist is
//! created and later removed by the wiring layer's absent marker.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, gateway_path,
    gw_base_match, join_relative, load_stash, read_config, record_old_values, save_stash,
    write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Antigravity spec.
pub const OUR_KEYS: &[&str] = &[
    "gateway",
    "models.listSource",
    "models.defaultModelId",
    "models.list",
];

const GATEWAY: &str = "gateway";
const LIST_SOURCE: &str = "models.listSource";
const DEFAULT_MODEL: &str = "models.defaultModelId";
const LEGACY_LIST: &str = "models.list";

/// Antigravity desktop wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct AntigravitySpec;

impl AntigravitySpec {
    fn new_path(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".gemini/config/config.json")
    }

    fn legacy_path(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".antigravity/settings.json")
    }

    fn paths(ctx: &HomeCtx) -> [PathBuf; 2] {
        [Self::new_path(ctx), Self::legacy_path(ctx)]
    }
}

impl AgentSpec for AntigravitySpec {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = join_relative(&ctx.home, ".gemini/config");
        let installed = config_dir.is_dir()
            || ctx.home.join(".antigravity").is_dir()
            || bin_in_path(ctx, "antigravity");
        if installed {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        Self::paths(ctx).to_vec()
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let list_source = gateway_path(wiring.gateway_url, "v1/models?format=antigravity");
        let gateway = json!({
            "enabled": true,
            "url": wiring.gateway_url,
            "apiKey": wiring.agent_key,
            "wireProtocol": "openai",
        });

        let mut old = BTreeMap::new();
        let mut stash = load_stash(ctx);
        let mut written = Vec::new();

        for path in Self::paths(ctx) {
            let text = read_config(&path)?.unwrap_or_default();
            for key in [GATEWAY, LIST_SOURCE, DEFAULT_MODEL, LEGACY_LIST] {
                if let Some(value) = json::get(&text, key) {
                    old.insert(key.to_owned(), value);
                }
            }
            let mut next = json::set(
                &text,
                &[
                    KV::new(GATEWAY, gateway.clone()),
                    KV::new(LIST_SOURCE, json!(list_source)),
                    KV::new(DEFAULT_MODEL, json!(wiring.default_model)),
                ],
            )?;
            next = json::del(&next, &[LEGACY_LIST])?;
            written.push(write_config(&path, &next)?);
        }

        record_old_values(&mut stash, self.id(), old);
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let old = load_stash(ctx).old_values(self.id());
        let mut stash = load_stash(ctx);
        let mut written = Vec::new();

        for path in Self::paths(ctx) {
            let Some(current) = read_config(&path)? else {
                continue;
            };
            let mut text = current;
            text = restore(&text, GATEWAY, old.get(GATEWAY))?;
            text = restore(&text, LIST_SOURCE, old.get(LIST_SOURCE))?;
            text = restore(&text, DEFAULT_MODEL, old.get(DEFAULT_MODEL))?;
            text = restore(&text, LEGACY_LIST, old.get(LEGACY_LIST))?;
            text = prune_empty(&text, "models")?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let mut found = false;
        let mut connected = false;
        for path in Self::paths(ctx) {
            let Ok(Some(text)) = read_config(&path) else {
                continue;
            };
            let Some(gateway) = json::get(&text, GATEWAY) else {
                continue;
            };
            let Ok(gateway) = serde_json::from_str::<Value>(&gateway) else {
                continue;
            };
            found = true;
            let enabled = gateway
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let url = gateway.get("url").and_then(Value::as_str);
            let key = gateway.get("apiKey").and_then(Value::as_str);
            let selected = json::get(&text, DEFAULT_MODEL).is_some_and(|value| !value.is_empty());
            if enabled
                && url.is_some_and(|value| gw_base_match(value, gateway_url))
                && key.is_some_and(|value| !value.is_empty())
                && selected
                && json::get(&text, LEGACY_LIST).is_none()
            {
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
        for path in Self::paths(ctx) {
            if let Ok(Some(text)) = read_config(&path) {
                for key in [GATEWAY, DEFAULT_MODEL] {
                    if let Some(value) = json::get(&text, key) {
                        values.insert(key.to_owned(), value);
                    }
                }
            }
        }
        values
    }
}

fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => {
            let value: Value =
                serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()));
            json::set(text, &[KV::new(path, value)])?
        }
        _ => json::del(text, &[path])?,
    })
}

fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
