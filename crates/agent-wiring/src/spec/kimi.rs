//! Kimi Code specification (`$KIMI_CODE_HOME/config.toml`).
//!
//! Kimi Code stores providers and models as named TOML tables:
//! `[providers."<id>"]` names the endpoint and key, and each
//! `[models."<provider>/<model>"]` table describes one model. The session
//! starts on the top-level `default_model`, written as
//! `any-converter/<catalog id>`.
//!
//! The configuration directory is `$KIMI_CODE_HOME` when set, otherwise
//! `~/.kimi-code`. The legacy `~/.kimi` directory is used only when
//! `~/.kimi-code` does not exist, so an upgraded install that still has the old
//! folder keeps working. The legacy directory honours `$KIMI_SHARE_DIR`.
//!
//! Legacy `kimi-cli` refuses the `tool_use` capability and `max_output_size`,
//! so those keys are written only for the new `~/.kimi-code` layout.
//!
//! Reasoning effort is per model in Kimi Code (`default_effort` inside each
//! reasoning model table); there is no top-level effort key.
//!
//! Disconnect cannot recompute our model-table names: the catalog may have
//! changed since connect. It therefore scans the raw TOML for table headers
//! beginning with `[models."any-converter/` or `[models.'any-converter/` and
//! deletes exactly those keys. Table keys are treated literally, so model ids
//! that contain dots (for example `glm-4.6`) remain a single key.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gateway_path,
    gw_base_match, join_relative, load_stash, read_config, record_old_values, save_stash,
    write_config,
};
use crate::edit::{KV, toml};
use crate::error::WiringError;

/// Configuration keys owned by the Kimi Code spec.
pub const OUR_KEYS: &[&str] = &[
    "default_model",
    "providers.any-converter",
    "models.any-converter",
];

/// Reasoning levels Kimi Code accepts per reasoning model.
const EFFORTS: &[&str] = &["low", "medium", "high"];

const PROVIDER: &str = "any-converter";
const PROVIDER_TABLE: &str = "providers.any-converter";
const DEFAULT_MODEL: &str = "default_model";
/// Per-model effort key written inside a reasoning model table.
const MODEL_EFFORT: &str = "default_effort";
const MODEL_PREFIX: &str = "any-converter/";
/// Raw header prefixes our per-model tables render with.
const MODEL_HEADER_PREFIX_DOUBLE: &str = "[models.\"any-converter/";
const MODEL_HEADER_PREFIX_SINGLE: &str = "[models.'any-converter/";
/// Default context window when the catalog does not report one.
const DEFAULT_CONTEXT: u64 = 128_000;
/// Output cap Kimi Code expects on every model.
const MAX_OUTPUT: u64 = 16_384;

/// Kimi Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct KimiSpec;

impl KimiSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("KIMI_CODE_HOME") {
            if !value.trim().is_empty() {
                return expand_tilde(ctx, value.trim());
            }
        }
        let current = join_relative(&ctx.home, ".kimi-code");
        if current.is_dir() {
            return current;
        }
        let legacy = Self::legacy_dir(ctx);
        if legacy.is_dir() {
            return legacy;
        }
        current
    }

    /// The legacy `kimi-cli` directory, which honours `$KIMI_SHARE_DIR`.
    fn legacy_dir(ctx: &HomeCtx) -> PathBuf {
        match (ctx.env)("KIMI_SHARE_DIR") {
            Some(value) if !value.trim().is_empty() => expand_tilde(ctx, value.trim()),
            _ => join_relative(&ctx.home, ".kimi"),
        }
    }

    /// Whether wiring resolves to the legacy `kimi-cli` layout.
    fn is_legacy(ctx: &HomeCtx) -> bool {
        if let Some(value) = (ctx.env)("KIMI_CODE_HOME") {
            if !value.trim().is_empty() {
                return false;
            }
        }
        if join_relative(&ctx.home, ".kimi-code").is_dir() {
            return false;
        }
        Self::legacy_dir(ctx).is_dir()
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.toml")
    }
}

impl AgentSpec for KimiSpec {
    fn id(&self) -> &'static str {
        "kimi"
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
        let legacy = Self::is_legacy(ctx);
        let config = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        if let Some(value) = toml::get(&config, DEFAULT_MODEL) {
            old.insert(DEFAULT_MODEL.to_owned(), value);
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut text = toml::set(
            &config,
            &[KV::new(
                DEFAULT_MODEL,
                json!(format!("{MODEL_PREFIX}{}", wiring.default_model)),
            )],
        )?;
        text = toml::set_table(
            &text,
            PROVIDER_TABLE,
            &[
                KV::new("type", json!("kimi")),
                KV::new("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
                KV::new("api_key", json!(wiring.agent_key)),
            ],
        )?;

        let effort = wiring.effort.filter(|level| !level.is_empty());
        let mut model_keys = Vec::new();
        for model in &wiring.catalog.models {
            let mut kvs = vec![
                KV::new("provider", json!(PROVIDER)),
                KV::new("model", json!(model.id)),
                KV::new(
                    "max_context_size",
                    json!(model.context_tokens.unwrap_or(DEFAULT_CONTEXT)),
                ),
            ];
            if model.name != model.id {
                kvs.push(KV::new("display_name", json!(model.name)));
            }
            if legacy {
                // Legacy kimi-cli rejects `tool_use`; non-reasoning models get
                // no capabilities key at all.
                if model.reasoning {
                    kvs.push(KV::new("capabilities", json!(["thinking"])));
                }
            } else {
                let capabilities = if model.reasoning {
                    json!(["thinking", "tool_use"])
                } else {
                    json!(["tool_use"])
                };
                kvs.push(KV::new("capabilities", capabilities));
            }
            if model.reasoning {
                kvs.push(KV::new("support_efforts", json!(EFFORTS)));
                if let Some(level) = effort {
                    kvs.push(KV::new(MODEL_EFFORT, json!(level)));
                }
            }
            if !legacy {
                kvs.push(KV::new("max_output_size", json!(MAX_OUTPUT)));
            }
            let model_key = format!("{MODEL_PREFIX}{}", model.id);
            text = toml::set_table_keys(&text, &["models", &model_key], &kvs)?;
            model_keys.push(model_key);
        }

        // `Some("")` means the model default: drop a level a previous connect
        // wrote. Re-created tables keep old keys, so clear them explicitly.
        if wiring.effort == Some("") {
            let paths: Vec<Vec<String>> = model_keys
                .iter()
                .map(|key| vec!["models".to_owned(), key.clone(), MODEL_EFFORT.to_owned()])
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
            text = toml::del(&text, &[PROVIDER_TABLE])?;
            text = restore_scalar(&text, DEFAULT_MODEL, old.get(DEFAULT_MODEL))?;
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
        let base = toml::get(&config, "providers.any-converter.base_url");
        let key = toml::get(&config, "providers.any-converter.api_key");
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
            if let Some(value) = toml::get(&config, DEFAULT_MODEL) {
                values.insert(DEFAULT_MODEL.to_owned(), value);
            }
            for key in ["base_url", "api_key"] {
                if let Some(value) = toml::get(&config, &format!("{PROVIDER_TABLE}.{key}")) {
                    values.insert(format!("{PROVIDER_TABLE}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Deletes every `[models."any-converter/..."]` or single-quoted variant found
/// in `text`.
///
/// The catalog may differ between connect and disconnect, so the table names
/// are discovered from the file's own headers. Paths are deleted with literal
/// key segments, which keeps dotted model ids in one table.
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
