//! Mothx specification (`MOTHX_DIR`, `%APPDATA%\mothx`, or `~/.mothx`).
//!
//! Mothx keeps everything in one `settings.json`: the default provider and
//! model, an optional thinking level, and a `providers` mapping. Connect writes
//! an OpenAI-chat provider pointing at the gateway's `/v1`; disconnect restores
//! the previous selectors and removes the provider block.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::support;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, app_data_dir, env_dir, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json as json_edit};
use crate::error::WiringError;

/// Configuration keys owned by the Mothx spec.
pub const OUR_KEYS: &[&str] = &[
    "defaultProvider",
    "defaultModel",
    "defaultThinkingLevel",
    "providers.any-converter",
];

/// Reasoning levels Mothx accepts.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// The selector keys connect writes and disconnect restores.
const SELECTORS: &[&str] = &["defaultProvider", "defaultModel", "defaultThinkingLevel"];

/// Mothx wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MothxSpec;

impl MothxSpec {
    /// Mothx's configuration directory.
    fn dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "MOTHX_DIR", |ctx| {
            if cfg!(windows) {
                app_data_dir(ctx).join("mothx")
            } else {
                ctx.home.join(".mothx")
            }
        })
    }

    /// The settings file.
    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }

    /// The dotted path of the provider block.
    fn provider_path() -> String {
        format!("providers.{}", support::PROVIDER)
    }
}

impl AgentSpec for MothxSpec {
    fn id(&self) -> &'static str {
        "mothx"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::settings_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let settings = read_config(&path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            support::record_keys(
                &settings,
                &[
                    "defaultProvider",
                    "defaultModel",
                    "defaultThinkingLevel",
                    "providers",
                ],
            ),
        );

        let provider = json!({
            "baseUrl": support::base_url(wiring),
            "api": "openai-chat",
            "apiKey": wiring.agent_key,
            "thinkingFormat": "openai",
            "models": provider_models(wiring),
        });

        let mut text = json_edit::set(
            &settings,
            &[
                KV::new("defaultProvider", json!(support::PROVIDER)),
                KV::new("defaultModel", json!(wiring.default_model)),
                KV::new(Self::provider_path(), provider),
            ],
        )?;
        text = apply_effort(&text, wiring.effort)?;

        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = support::restore_keys(&current, &old, SELECTORS)?;
            let provider = Self::provider_path();
            text = json_edit::del(&text, &[provider.as_str()])?;
            text = prune_container(&text, "providers", old.contains_key("providers"))?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let provider = Self::provider_path();
        let base = json_edit::get(&settings, &format!("{provider}.baseUrl"));
        let key = json_edit::get(&settings, &format!("{provider}.apiKey"));
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
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in SELECTORS {
                if let Some(value) = json_edit::get(&settings, key) {
                    values.insert((*key).to_owned(), value);
                }
            }
            let provider = Self::provider_path();
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json_edit::get(&settings, &format!("{provider}.{key}")) {
                    values.insert(format!("{provider}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The provider's model array, one entry per catalog model.
///
/// `reasoning` is what has Mothx send a thinking level at all, so it mirrors
/// the catalog flag rather than defaulting to `true` (magpie `mothx.go:202`).
///
/// Catalog approximation: [`CatalogModel`](crate::sync::catalog::CatalogModel)
/// carries no image data, so `input` is always `["text"]`, matching magpie's
/// `!Images` default. A model that accepts images would need `"image"` here,
/// which we cannot know.
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    support::catalog_models(wiring, |model| {
        let mut entry = Map::new();
        entry.insert("id".to_owned(), json!(model.id));
        entry.insert("name".to_owned(), json!(model.name));
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if let Some(context) = model.context_tokens {
            entry.insert("contextWindow".to_owned(), json!(context));
        }
        entry.insert("input".to_owned(), json!(["text"]));
        Value::Object(entry)
    })
}

/// Writes `defaultThinkingLevel`; `None` leaves the file untouched and
/// `Some("")` removes our setting.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(json_edit::del(text, &["defaultThinkingLevel"])?),
        Some(level) => Ok(json_edit::set(
            text,
            &[KV::new("defaultThinkingLevel", json!(level))],
        )?),
    }
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
