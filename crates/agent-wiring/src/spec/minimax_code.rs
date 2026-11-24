//! MiniMax Code specification (`MINIMAX_DATA_DIR`, `MAVIS_DATA_DIR`, or
//! `~/.minimax`).
//!
//! MiniMax Code nests custom providers under `custom_provider` and selects one
//! with `custom_provider:<name>/<model>`. Connect writes an
//! Anthropic-messages provider at the gateway root (the agent appends its own
//! `/v1`) and points the default and memory selectors at it; disconnect
//! restores the previous selectors and removes the provider.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::support;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gw_base_match,
    join_relative, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, yaml};
use crate::error::WiringError;

/// Configuration keys owned by the MiniMax Code spec.
pub const OUR_KEYS: &[&str] = &[
    "custom_provider.any-converter",
    "defaultModel",
    "defaultModelVariant",
    "defaultLightModel",
    "memory.captureModel",
    "memory.consolidationModel",
];

/// Reasoning levels MiniMax Code accepts.
const EFFORTS: &[&str] = &["low", "medium", "high"];

const DEFAULT_MODEL: &str = "defaultModel";
const DEFAULT_MODEL_VARIANT: &str = "defaultModelVariant";
const DEFAULT_LIGHT_MODEL: &str = "defaultLightModel";
const CAPTURE_MODEL: &str = "memory.captureModel";
const CONSOLIDATION_MODEL: &str = "memory.consolidationModel";

/// Selectors rewritten when they are absent or already point at us.
const SELECTORS: &[&str] = &[DEFAULT_LIGHT_MODEL, CAPTURE_MODEL, CONSOLIDATION_MODEL];

/// MiniMax Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MinimaxCodeSpec;

impl MinimaxCodeSpec {
    /// MiniMax Code's data directory.
    fn data_dir(ctx: &HomeCtx) -> PathBuf {
        for variable in ["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"] {
            if let Some(value) = (ctx.env)(variable) {
                let value = value.trim();
                if !value.is_empty() {
                    return expand_tilde(ctx, value);
                }
            }
        }
        join_relative(&ctx.home, ".minimax")
    }

    /// The YAML configuration file.
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::data_dir(ctx).join("config.yaml")
    }

    /// The dotted path of the provider block.
    fn provider_path() -> String {
        format!("custom_provider.{}", support::PROVIDER)
    }

    /// The prefix every selector value written by this spec carries.
    fn selection_prefix() -> String {
        format!("custom_provider:{}/", support::PROVIDER)
    }
}

/// MiniMax Code names a custom model `"<provider>/<model>"` with at most one
/// slash, so the models-map key and the selector use the flat spelling: each
/// `/` becomes `~` (magpie `provider/flat.go`, `minimax.go:56-68`). The
/// gateway maps the flat id back to the catalog id.
fn flat_id(id: &str) -> String {
    id.replace('/', "~")
}

impl AgentSpec for MinimaxCodeSpec {
    fn id(&self) -> &'static str {
        "minimax-code"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::data_dir(ctx);
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

        let selected = format!(
            "{}{}",
            Self::selection_prefix(),
            flat_id(wiring.default_model)
        );
        let mut kvs = vec![
            KV::new(Self::provider_path(), provider(wiring)),
            KV::new(DEFAULT_MODEL, json!(selected)),
        ];
        for key in SELECTORS {
            if writes_selector(&config, key) {
                kvs.push(KV::new(*key, json!(selected)));
            }
        }

        // Record every selector: a user pick we leave untouched is written back
        // verbatim, and an absent one is deleted again.
        let mut old = BTreeMap::new();
        for key in [
            DEFAULT_MODEL,
            DEFAULT_MODEL_VARIANT,
            DEFAULT_LIGHT_MODEL,
            CAPTURE_MODEL,
            CONSOLIDATION_MODEL,
        ] {
            if let Some(value) = yaml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        // Whole mappings are recorded so disconnect can distinguish an
        // explicitly empty mapping from an absent one.
        for key in ["custom_provider", "memory"] {
            if let Some(value) = yaml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let text = yaml::set(&config, &kvs)?;
        // A variant belongs to the model it was picked for, and none of ours
        // has one (magpie minimax.go:96-110).
        let text = yaml::del(&text, &[DEFAULT_MODEL_VARIANT])?;
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
            // Top-level scalars are safe to delete directly; nested mappings
            // are rebuilt whole because deleting their last entry corrupts the
            // line that follows it.
            let mut text = current;
            for key in [DEFAULT_MODEL, DEFAULT_MODEL_VARIANT, DEFAULT_LIGHT_MODEL] {
                text = restore_top(&text, key, old.get(key))?;
            }

            let mut providers = parent_mapping(&text, "custom_provider");
            providers.remove(support::PROVIDER);
            text = replace_mapping(
                &text,
                "custom_provider",
                providers,
                old.contains_key("custom_provider"),
            )?;

            let mut memory = parent_mapping(&text, "memory");
            for (relative, full) in [
                ("captureModel", CAPTURE_MODEL),
                ("consolidationModel", CONSOLIDATION_MODEL),
            ] {
                memory.remove(relative);
                if let Some(value) = old.get(full).filter(|value| !value.is_empty()) {
                    memory.insert(relative.to_owned(), Value::String(value.clone()));
                }
            }
            text = replace_mapping(&text, "memory", memory, old.contains_key("memory"))?;
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
        let provider = Self::provider_path();
        let base = yaml::get(&config, &format!("{provider}.options.baseURL"));
        let key = yaml::get(&config, &format!("{provider}.options.apiKey"));
        let base = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (base, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in [
                DEFAULT_MODEL,
                DEFAULT_MODEL_VARIANT,
                DEFAULT_LIGHT_MODEL,
                CAPTURE_MODEL,
                CONSOLIDATION_MODEL,
            ] {
                if let Some(value) = yaml::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
            let provider = Self::provider_path();
            for key in ["baseURL", "apiKey"] {
                if let Some(value) = yaml::get(&config, &format!("{provider}.options.{key}")) {
                    values.insert(format!("{provider}.options.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The provider block written under `custom_provider`.
///
/// Models are keyed by their flat id (see [`flat_id`]). `reasoning` and the
/// `thinking` block are gated on the catalog's reasoning flag; only a
/// reasoning model gets the effort ladder (catalog approximation: per-model
/// efforts are unknown, so a reasoning model gets the full supported set).
fn provider(wiring: &WiringCtx<'_>) -> Value {
    let default_effort = wiring.effort.filter(|effort| !effort.is_empty());

    let mut models = Map::new();
    for model in &wiring.catalog.models {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(model.name));
        if let Some(context) = model.context_tokens {
            entry.insert("limit".to_owned(), json!({ "context": context }));
        }
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if model.reasoning {
            let mut thinking = Map::new();
            thinking.insert("effortOptions".to_owned(), json!(EFFORTS));
            if let Some(effort) = default_effort {
                thinking.insert("defaultEffort".to_owned(), json!(effort));
            }
            entry.insert("thinking".to_owned(), Value::Object(thinking));
        }
        models.insert(flat_id(&model.id), Value::Object(entry));
    }

    let options = json!({
        "apiKey": wiring.agent_key,
        "baseURL": wiring.gateway_url.trim_end_matches('/'),
        "authMode": "api-key",
        "headers": { "User-Agent": "minimax-code" },
    });
    json!({
        "name": support::PROVIDER,
        "kind": "custom",
        "enabled": true,
        "api": "anthropic-messages",
        "options": options,
        "models": Value::Object(models),
    })
}

/// Reports whether a secondary selector already points at one of our models.
///
/// Only a value that already starts with `custom_provider:any-converter/` is
/// rewritten (for example from the old slash id to the flat one). An absent
/// selector or one naming another provider is the user's and left alone;
/// magpie never creates these selectors (magpie `minimax.go:326-331`).
fn writes_selector(text: &str, key: &str) -> bool {
    let reference = format!("custom_provider:{}/", support::PROVIDER);
    yaml::get(text, key).is_some_and(|value| value.trim().starts_with(&reference))
}

/// Restores a top-level scalar, deleting the key when it was absent.
fn restore_top(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => Ok(yaml::set(text, &[KV::new(path, json!(value))])?),
        _ => Ok(yaml::del(text, &[path])?),
    }
}

/// Reads a mapping-valued key as a JSON object.
fn parent_mapping(text: &str, path: &str) -> Map<String, Value> {
    yaml::get(text, path)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Replaces (or removes) a top-level mapping in one safe edit.
///
/// The top-level key has no indentation to leak into the following line, so
/// this avoids the nested-entry deletion bug in the YAML editor. An explicitly
/// empty mapping is kept when it existed before connect.
fn replace_mapping(
    text: &str,
    path: &str,
    mapping: Map<String, Value>,
    existed: bool,
) -> Result<String, WiringError> {
    let text = yaml::del(text, &[path])?;
    if mapping.is_empty() {
        return if existed {
            Ok(yaml::set(&text, &[KV::new(path, json!({}))])?)
        } else {
            Ok(text)
        };
    }
    Ok(yaml::set(&text, &[KV::new(path, Value::Object(mapping))])?)
}
