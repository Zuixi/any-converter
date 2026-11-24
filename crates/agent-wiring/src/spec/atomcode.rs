//! AtomCode specification (`$ATOMCODE_HOME/config.toml`).
//!
//! AtomCode separates the endpoint (`[provider_accounts."<id>"]`) from the
//! models that use it (`[models."<account>/<model>"]`). Wiring registers one
//! `any-converter` account pointing at the gateway, one model table per
//! catalog model, and selects it through the top-level `default_model` and
//! `default_provider`.
//!
//! Disconnect scans the raw text for `[models."any-converter/` headers (the
//! catalog may have changed since connect) and deletes exactly those tables,
//! then restores the two stashed selectors.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gateway_path,
    gw_base_match, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, toml};
use crate::error::WiringError;

/// Configuration keys owned by the AtomCode spec.
pub const OUR_KEYS: &[&str] = &[
    "provider_accounts.any-converter",
    "default_model",
    "default_provider",
    "models.any-converter",
];

/// Reasoning levels AtomCode accepts per model.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const PROVIDER: &str = "any-converter";
const ACCOUNT_TABLE: &str = "provider_accounts.any-converter";
const DEFAULT_MODEL: &str = "default_model";
const DEFAULT_PROVIDER: &str = "default_provider";
const MODEL_PREFIX: &str = "any-converter/";
/// Raw header prefixes our per-model tables render with.
const MODEL_HEADER_PREFIX_DOUBLE: &str = "[models.\"any-converter/";
const MODEL_HEADER_PREFIX_SINGLE: &str = "[models.'any-converter/";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;

/// AtomCode wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct AtomCodeSpec;

impl AtomCodeSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        // A relative `ATOMCODE_HOME` is not a valid location; AtomCode itself
        // requires an absolute path, so ignore anything else.
        if let Some(value) = (ctx.env)("ATOMCODE_HOME") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                let expanded = expand_tilde(ctx, trimmed);
                if expanded.is_absolute() {
                    return expanded;
                }
            }
        }
        ctx.home.join(".atomcode")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.toml")
    }
}

impl AgentSpec for AtomCodeSpec {
    fn id(&self) -> &'static str {
        "atomcode"
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
        for key in [DEFAULT_MODEL, DEFAULT_PROVIDER] {
            if let Some(value) = toml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let selected = format!("{MODEL_PREFIX}{}", wiring.default_model);
        let mut text = toml::set(
            &config,
            &[
                KV::new(DEFAULT_MODEL, json!(selected)),
                KV::new(DEFAULT_PROVIDER, json!(PROVIDER)),
            ],
        )?;
        text = toml::set_table(
            &text,
            ACCOUNT_TABLE,
            &[
                KV::new("provider", json!("openai-compatible")),
                KV::new("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
                KV::new("api_key", json!(wiring.agent_key)),
            ],
        )?;

        let effort = wiring.effort.filter(|level| !level.is_empty());
        let mut model_tables = Vec::new();
        for model in &wiring.catalog.models {
            let mut kvs = vec![
                KV::new("account", json!(PROVIDER)),
                KV::new("model", json!(model.id)),
                KV::new(
                    "context_window",
                    json!(model.context_tokens.unwrap_or(DEFAULT_CONTEXT)),
                ),
                KV::new("supports_vision", json!(false)),
            ];
            // Effort is only meaningful for reasoning models.
            if model.reasoning {
                kvs.push(KV::new("reasoning_effort_levels", json!(EFFORTS)));
                if let Some(level) = effort {
                    kvs.push(KV::new("reasoning_effort", json!(level)));
                }
            }
            let model_key = format!("{MODEL_PREFIX}{}", model.id);
            text = toml::set_table_keys(&text, &["models", &model_key], &kvs)?;
            model_tables.push(model_key);
        }

        // `Some("")` means the model default: drop a level a previous connect
        // wrote. Re-created tables keep old keys, so clear them explicitly.
        if wiring.effort == Some("") {
            let paths: Vec<Vec<String>> = model_tables
                .iter()
                .map(|key| {
                    vec![
                        "models".to_owned(),
                        key.clone(),
                        "reasoning_effort".to_owned(),
                    ]
                })
                .collect();
            text = toml::del_key_paths(&text, &paths)?;
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
            text = toml::del(&text, &[ACCOUNT_TABLE])?;
            text = restore_scalar(&text, DEFAULT_MODEL, old.get(DEFAULT_MODEL))?;
            text = restore_scalar(&text, DEFAULT_PROVIDER, old.get(DEFAULT_PROVIDER))?;
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
        let base = toml::get(&config, "provider_accounts.any-converter.base_url");
        let key = toml::get(&config, "provider_accounts.any-converter.api_key");
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
            for key in [DEFAULT_MODEL, DEFAULT_PROVIDER] {
                if let Some(value) = toml::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
            for key in ["base_url", "api_key"] {
                if let Some(value) = toml::get(&config, &format!("{ACCOUNT_TABLE}.{key}")) {
                    values.insert(format!("{ACCOUNT_TABLE}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Deletes every `[models."any-converter/..."]` table found in `text`.
///
/// `toml::del` accepts dotted paths only, so the raw table headers are scanned
/// first. Reading the names from the file (instead of the catalog) keeps a
/// disconnect correct after the catalog changed.
fn delete_model_tables(text: &str) -> Result<String, WiringError> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let Some(id) = model_header_id(line) else {
            continue;
        };
        paths.push(vec!["models".to_owned(), format!("{MODEL_PREFIX}{id}")]);
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

/// Restores one stashed value, or removes the key when there was none.
fn restore_scalar(text: &str, key: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => toml::set(text, &[KV::new(key, json!(value))])?,
        _ => toml::del(text, &[key])?,
    })
}
