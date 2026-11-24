//! Grok CLI specification (`$GROK_HOME/config.toml`).
//!
//! Grok keeps connection profiles as named TOML tables under `[model."<id>"]`
//! and selects one through `[models] default`. Wiring writes one table per
//! catalog model, named `any-converter/<catalog id>`, and points `default` at
//! the selected model.
//!
//! `[features] campaigns` disables Grok's campaign prompts, which would
//! otherwise override our default model. Wiring creates the key (and the
//! `[features]` table when absent) whenever it picks a default, and restores
//! the user's previous value on disconnect.
//!
//! Disconnect discovers our table names from the raw text (the catalog may
//! have changed since connect) and deletes them with literal key segments, so
//! model ids containing dots stay a single table. It then removes
//! `models.default` and `models.default_reasoning_effort` without restoring
//! them, and restores `features.campaigns`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, toml};
use crate::error::WiringError;

/// Configuration keys owned by the Grok spec.
pub const OUR_KEYS: &[&str] = &[
    "models.default",
    "models.default_reasoning_effort",
    "features.campaigns",
    "model.any-converter",
];

/// Reasoning levels Grok accepts, in its own order.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

const DEFAULT: &str = "models.default";
const DEFAULT_EFFORT: &str = "models.default_reasoning_effort";
const CAMPAIGNS: &str = "features.campaigns";
const MODEL_PREFIX: &str = "any-converter/";
/// Raw header prefixes our per-model tables render with.
const MODEL_HEADER_PREFIX_DOUBLE: &str = "[model.\"any-converter/";
const MODEL_HEADER_PREFIX_SINGLE: &str = "[model.'any-converter/";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;

/// Grok CLI wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct GrokSpec;

impl GrokSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "GROK_HOME", |ctx| ctx.home.join(".grok"))
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.toml")
    }
}

impl AgentSpec for GrokSpec {
    fn id(&self) -> &'static str {
        "grok"
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
        let config = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [DEFAULT, CAMPAIGNS] {
            if let Some(value) = toml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut kvs = vec![KV::new(
            DEFAULT,
            json!(format!("{MODEL_PREFIX}{}", wiring.default_model)),
        )];
        if let Some(level) = wiring.effort.filter(|level| !level.is_empty()) {
            kvs.push(KV::new(DEFAULT_EFFORT, json!(level)));
        }
        let mut text = toml::set(&config, &kvs)?;
        if wiring.effort == Some("") {
            text = toml::del(&text, &[DEFAULT_EFFORT])?;
        }

        // Campaign prompts would take over the default we just selected.
        // `features` may not exist yet; creating the one table is what makes
        // the setting effective.
        text = toml::set(&text, &[KV::new(CAMPAIGNS, json!(false))])?;

        for model in &wiring.catalog.models {
            let mut model_kvs = vec![
                KV::new("model", json!(model.id)),
                KV::new("name", json!(model.name)),
                KV::new("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
                KV::new("api_key", json!(wiring.agent_key)),
                KV::new("api_backend", json!("chat_completions")),
                KV::new(
                    "context_window",
                    json!(model.context_tokens.unwrap_or(DEFAULT_CONTEXT)),
                ),
            ];
            if model.reasoning {
                model_kvs.push(KV::new("reasoning_efforts", json!(EFFORTS)));
            }
            let model_key = format!("{MODEL_PREFIX}{}", model.id);
            text = toml::set_table_keys(&text, &["model", &model_key], &model_kvs)?;
        }

        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = delete_model_tables(&current)?;
            text = toml::del(&text, &[DEFAULT, DEFAULT_EFFORT])?;
            text = restore_campaigns(&text, old.get(CAMPAIGNS))?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(config)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let selected = toml::get(&config, DEFAULT);
        let lookup = |field: &str| {
            selected
                .as_deref()
                .filter(|value| value.starts_with(MODEL_PREFIX))
                .and_then(|name| toml::get_keys(&config, &["model", name, field]))
        };
        let base = lookup("base_url");
        let key = lookup("api_key");
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in [DEFAULT, DEFAULT_EFFORT, CAMPAIGNS] {
                if let Some(value) = toml::get(&config, key) {
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

/// Deletes every `[model."any-converter/..."]` table found in `text`.
///
/// `toml::del` splits paths on dots, so the raw headers are scanned first and
/// literal key paths are deleted. Looking at the file rather than the catalog
/// keeps a disconnect correct after the catalog changed.
fn delete_model_tables(text: &str) -> Result<String, WiringError> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let Some(id) = model_header_id(line) else {
            continue;
        };
        paths.push(vec!["model".to_owned(), format!("{MODEL_PREFIX}{id}")]);
    }
    if paths.is_empty() {
        return Ok(text.to_owned());
    }
    Ok(toml::del_key_paths(text, &paths)?)
}

/// Extracts the model id from one of our table headers.
fn model_header_id(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let (rest, quote) = if let Some(rest) = trimmed.strip_prefix(MODEL_HEADER_PREFIX_DOUBLE) {
        (rest, '"')
    } else {
        (trimmed.strip_prefix(MODEL_HEADER_PREFIX_SINGLE)?, '\'')
    };
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

/// Restores `features.campaigns`, or removes it when it did not exist.
fn restore_campaigns(text: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if value == "true" => toml::set(text, &[KV::new(CAMPAIGNS, json!(true))])?,
        Some(value) if value == "false" => toml::set(text, &[KV::new(CAMPAIGNS, json!(false))])?,
        Some(value) if !value.is_empty() => toml::set(text, &[KV::new(CAMPAIGNS, json!(value))])?,
        _ => toml::del(text, &[CAMPAIGNS])?,
    })
}
