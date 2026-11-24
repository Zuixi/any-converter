//! LiveAgent specification (`LIVEAGENT_HOME`, `K_BRAIN_HOME`,
//! `~/.liveagent/data-dir.json`, or `~/.liveagent`).
//!
//! LiveAgent stores providers in a JSONC `config.json`. Connect registers three
//! entries so the user can pick the protocol per model: a Codex provider on
//! Chat Completions, a Codex provider on the Responses API, and a Claude Code
//! provider on Anthropic Messages. Disconnect removes all three and restores
//! the previous default model and provider.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::support;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gw_base_match,
    join_relative, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json as json_edit};
use crate::error::WiringError;

/// Configuration keys owned by the LiveAgent spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.any-converter",
    "providers.any-converter-responses",
    "providers.any-converter-anthropic",
    "defaultModel",
    "defaultProvider",
];

const DEFAULT_MODEL: &str = "defaultModel";
const DEFAULT_PROVIDER: &str = "defaultProvider";

/// The demo provider/model LiveAgent's CLI writes into a fresh config.
const DEMO_PROVIDER: &str = "demo";
const DEMO_MODEL: &str = "model1";

/// LiveAgent wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveagentSpec;

impl LiveagentSpec {
    /// LiveAgent's configuration directory.
    ///
    /// Environment overrides win; otherwise `~/.liveagent/data-dir.json` may
    /// relocate the data directory, and `~/.liveagent` is the fallback.
    fn dir(ctx: &HomeCtx) -> PathBuf {
        for variable in ["LIVEAGENT_HOME", "K_BRAIN_HOME"] {
            if let Some(value) = (ctx.env)(variable) {
                let value = value.trim();
                if !value.is_empty() {
                    return expand_tilde(ctx, value);
                }
            }
        }
        let pointer = join_relative(&ctx.home, ".liveagent/data-dir.json");
        if let Ok(Some(text)) = read_config(&pointer) {
            if let Some(path) = json_edit::get(&text, "path") {
                let path = path.trim();
                if !path.is_empty() {
                    return expand_tilde(ctx, path);
                }
            }
        }
        join_relative(&ctx.home, ".liveagent")
    }

    /// The JSONC configuration file.
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.json")
    }
}

impl AgentSpec for LiveagentSpec {
    fn id(&self) -> &'static str {
        "liveagent"
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

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            support::record_keys(&config, &[DEFAULT_MODEL, DEFAULT_PROVIDER, "providers"]),
        );

        let mut kvs = vec![
            KV::new(
                format!("providers.{}", support::PROVIDER),
                codex_provider(wiring, "openai-completions", None),
            ),
            KV::new(
                format!("providers.{}-responses", support::PROVIDER),
                codex_provider(wiring, "openai-responses", Some("openai-responses")),
            ),
            KV::new(
                format!("providers.{}-anthropic", support::PROVIDER),
                anthropic_provider(wiring),
            ),
        ];
        if defaults_unset(&config) {
            // The pair is written together or not at all: writing one beside
            // the user's other pick leaves an inconsistent selection (magpie
            // liveagent.go:431-436).
            kvs.push(KV::new(DEFAULT_MODEL, json!(wiring.default_model)));
            kvs.push(KV::new(DEFAULT_PROVIDER, json!(support::PROVIDER)));
        }

        let text = json_edit::set(&config, &kvs)?;
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
            let names = [
                format!("providers.{}", support::PROVIDER),
                format!("providers.{}-responses", support::PROVIDER),
                format!("providers.{}-anthropic", support::PROVIDER),
            ];
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut text = json_edit::del(&current, &refs)?;
            text = prune_container(&text, "providers", old.contains_key("providers"))?;
            text = support::restore_keys(&text, &old, &[DEFAULT_MODEL, DEFAULT_PROVIDER])?;
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
        let provider = format!("providers.{}", support::PROVIDER);
        let base = json_edit::get(&config, &format!("{provider}.baseUrl"));
        let key = json_edit::get(&config, &format!("{provider}.apiKey"));
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
            for key in [DEFAULT_MODEL, DEFAULT_PROVIDER] {
                if let Some(value) = json_edit::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
            let provider = format!("providers.{}", support::PROVIDER);
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json_edit::get(&config, &format!("{provider}.{key}")) {
                    values.insert(format!("{provider}.{key}"), value);
                }
            }
        }
        values
    }
}

/// Reports whether BOTH default picks are absent or still the demo values.
///
/// Magpie additionally checks that the demo provider has no endpoint and that
/// no other provider lists `model1` (magpie `liveagent.go:793-822`); that scan
/// is approximated here, but the provider must at least be empty or `demo`.
fn defaults_unset(text: &str) -> bool {
    let model = json_edit::get(text, DEFAULT_MODEL);
    let provider = json_edit::get(text, DEFAULT_PROVIDER);
    let model_unset = model
        .as_deref()
        .is_none_or(|value| value.is_empty() || value == DEMO_MODEL);
    let provider_unset = provider
        .as_deref()
        .is_none_or(|value| value.is_empty() || value == DEMO_PROVIDER);
    model_unset && provider_unset
}

/// Removes an emptied container unless it existed before connect.
fn prune_container(text: &str, path: &str, existed: bool) -> Result<String, WiringError> {
    if existed {
        return Ok(text.to_owned());
    }
    if json_edit::get(text, path)
        .as_deref()
        .is_some_and(|value| value.trim() == "{}")
    {
        return Ok(json_edit::del(text, &[path])?);
    }
    Ok(text.to_owned())
}

/// A Codex-protocol provider entry.
fn codex_provider(wiring: &WiringCtx<'_>, api: &str, request_format: Option<&str>) -> Value {
    let ids: Vec<&str> = wiring
        .catalog
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut entry = Map::new();
    entry.insert("name".to_owned(), json!(support::PROVIDER));
    entry.insert("type".to_owned(), json!("codex"));
    entry.insert("baseUrl".to_owned(), json!(support::base_url(wiring)));
    entry.insert("api".to_owned(), json!(api));
    if let Some(format) = request_format {
        entry.insert("requestFormat".to_owned(), json!(format));
    }
    entry.insert("apiKey".to_owned(), json!(wiring.agent_key));
    entry.insert("modelOrder".to_owned(), json!(ids));
    entry.insert("models".to_owned(), Value::Array(models(wiring)));
    Value::Object(entry)
}

/// A Claude Code-protocol provider entry at the gateway root.
fn anthropic_provider(wiring: &WiringCtx<'_>) -> Value {
    json!({
        "type": "claude_code",
        "api": "anthropic-messages",
        "baseUrl": wiring.gateway_url.trim_end_matches('/'),
        "apiKey": wiring.agent_key,
        "models": models(wiring),
    })
}

/// The provider's model array, one entry per catalog model.
///
/// Catalog approximation: [`CatalogModel`](crate::sync::catalog::CatalogModel)
/// carries no image data, so `input` is always `["text"]` (magpie
/// `liveagent.go:300-306` adds `"image"` only for `m.Images`). Output caps and
/// other per-model data fall back to LiveAgent's defaults.
fn models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    support::catalog_models(wiring, |model| {
        let mut entry = Map::new();
        entry.insert("id".to_owned(), json!(model.id));
        entry.insert("displayName".to_owned(), json!(model.name));
        entry.insert("input".to_owned(), json!(["text"]));
        if let Some(context) = model.context_tokens {
            entry.insert("contextWindow".to_owned(), json!(context));
        }
        Value::Object(entry)
    })
}
