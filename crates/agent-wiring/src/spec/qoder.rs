//! Qoder and Qoder CN (`QODER_CONFIG_DIR` / `QODERCN_CONFIG_DIR`).
//!
//! Both variants share one settings layout: a `providers` map keyed by our
//! provider id, a top-level `model.name` selecting `<provider>/<model>`, and an
//! optional `model.reasoningEffort` plus a matching entry in
//! `model.preferences`. One parameterized spec covers both; only the agent id,
//! environment override, and default directory differ.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Qoder specs.
pub const OUR_KEYS: &[&str] = &[
    "providers.any-converter",
    "model.name",
    "model.reasoningEffort",
    "model.preferences",
];

const PROVIDER: &str = super::support::PROVIDER;
const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_PATH: &str = "providers.any-converter";
const MODEL_NAME: &str = "model.name";
const REASONING_EFFORT: &str = "model.reasoningEffort";
const PREFERENCES: &str = "model.preferences";

/// Reasoning levels Qoder forwards to thinking-capable models.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Where a Qoder variant keeps its configuration.
#[derive(Debug, Clone, Copy)]
pub struct QoderLayout {
    /// The agent id (`qoder`, `qoder-cn`).
    pub id: &'static str,
    /// Environment override tried before the default directory.
    pub env_var: &'static str,
    /// Directory under the home when no override applies.
    pub default_dir: &'static str,
}

/// One Qoder variant's wiring.
#[derive(Debug, Clone, Copy)]
pub struct QoderSpec {
    /// The variant's id, environment override, and default directory.
    pub layout: QoderLayout,
}

impl QoderSpec {
    fn dir(&self, ctx: &HomeCtx) -> PathBuf {
        let default_dir = self.layout.default_dir;
        env_dir(ctx, self.layout.env_var, |ctx| ctx.home.join(default_dir))
    }

    fn settings_path(&self, ctx: &HomeCtx) -> PathBuf {
        self.dir(ctx).join(SETTINGS_FILE)
    }
}

impl AgentSpec for QoderSpec {
    fn id(&self) -> &'static str {
        self.layout.id
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = self.dir(ctx);
        if config_dir.is_dir() || self.settings_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![self.settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.settings_path(ctx);
        let settings = read_config(&path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[MODEL_NAME]),
        );

        let model = model_key(wiring);
        let mut text = json::set(
            &settings,
            &[
                KV::new(PROVIDER_PATH, provider_entry(wiring)),
                KV::new(MODEL_NAME, json!(model)),
            ],
        )?;
        text = apply_effort(&text, wiring.effort, &model)?;

        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let text = json::del(&current, &[PROVIDER_PATH])?;
            let text = prune_if_empty(&text, "providers")?;
            let mut text = restore_model_name(&text, &old)?;
            text = remove_effort(&text)?;
            if is_empty_object(&text, "model") {
                text = json::del_pruning(&text, &["model"])?;
            }
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&self.settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&settings, &format!("{PROVIDER_PATH}.baseUrl"));
        let key = json::get(&settings, &format!("{PROVIDER_PATH}.apiKey"));
        let name = json::get(&settings, MODEL_NAME);

        let prefix = format!("{PROVIDER}/");
        let selected = name
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
        if let Ok(Some(settings)) = read_config(&self.settings_path(ctx)) {
            for path in [MODEL_NAME, REASONING_EFFORT] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
            }
            for key in ["baseUrl", "displayName"] {
                let path = format!("{PROVIDER_PATH}.{key}");
                if let Some(value) = json::get(&settings, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The qualified model identifier Qoder selects on.
fn model_key(wiring: &WiringCtx<'_>) -> String {
    format!("{PROVIDER}/{}", wiring.default_model)
}

fn provider_entry(wiring: &WiringCtx<'_>) -> Value {
    json!({
        "displayName": PROVIDER,
        "protocol": "openai",
        "baseUrl": gateway_path(wiring.gateway_url, "v1"),
        "apiKey": wiring.agent_key,
        "model": wiring.default_model,
        "models": model_entries(wiring),
    })
}

/// One provider model entry per catalog model.
///
/// `maxOutputTokens` is omitted because the catalog has no output limit
/// (Qoder also omits it). Vision is false because the catalog carries no
/// modality flags. Thinking capabilities are advertised only for models the
/// catalog marks as reasoning; the catalog has no per-model effort ladder, so
/// those models expose the full ladder this spec accepts.
fn model_entries(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = Map::new();
            entry.insert("model".to_owned(), json!(model.id));
            entry.insert("displayName".to_owned(), json!(model.name));
            if let Some(context) = model.context_tokens {
                entry.insert("contextWindow".to_owned(), json!(context));
            }
            let capabilities = if model.reasoning {
                json!({
                    "tools": true,
                    "vision": false,
                    "thinking": {
                        "modes": ["enabled"],
                        "supportsEffort": true,
                        "supportedEffortLevels": EFFORTS,
                        "requiresBudgetForEnabled": false,
                    },
                })
            } else {
                json!({
                    "tools": true,
                    "vision": false,
                })
            };
            entry.insert("capabilities".to_owned(), capabilities);
            Value::Object(entry)
        })
        .collect()
}

/// Writes or clears our reasoning effort and its preferences mirror.
fn apply_effort(text: &str, effort: Option<&str>, model: &str) -> Result<String, WiringError> {
    let Some(effort) = effort else {
        return Ok(text.to_owned());
    };
    if effort.is_empty() {
        let cleared = json::del(text, &[REASONING_EFFORT])?;
        return write_preferences(&cleared, model, None);
    }
    let written = json::set(text, &[KV::new(REASONING_EFFORT, json!(effort))])?;
    write_preferences(&written, model, Some(effort))
}

/// Replaces our key in `model.preferences`, leaving user entries alone.
fn write_preferences(text: &str, model: &str, effort: Option<&str>) -> Result<String, WiringError> {
    let mut preferences = preferences_map(text);
    preferences.remove(model);
    if let Some(effort) = effort {
        preferences.insert(
            model.to_owned(),
            json!({ "reasoning": { "effort": effort } }),
        );
    }
    if preferences.is_empty() {
        Ok(json::del(text, &[PREFERENCES])?)
    } else {
        Ok(json::set(
            text,
            &[KV::new(PREFERENCES, Value::Object(preferences))],
        )?)
    }
}

/// Removes our effort setting and our preferences entry on disconnect.
fn remove_effort(text: &str) -> Result<String, WiringError> {
    let text = json::del(text, &[REASONING_EFFORT])?;
    let mut preferences = preferences_map(&text);
    let prefix = format!("{PROVIDER}/");
    preferences.retain(|key, _| !key.starts_with(&prefix));
    if preferences.is_empty() {
        Ok(json::del(&text, &[PREFERENCES])?)
    } else {
        Ok(json::set(
            &text,
            &[KV::new(PREFERENCES, Value::Object(preferences))],
        )?)
    }
}

fn preferences_map(text: &str) -> Map<String, Value> {
    json::get(text, PREFERENCES)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Restores `model.name`, or deletes it when it was absent.
fn restore_model_name(text: &str, old: &BTreeMap<String, String>) -> Result<String, WiringError> {
    match old.get(MODEL_NAME) {
        Some(raw) if !raw.is_empty() => Ok(json::set(
            text,
            &[KV::new(MODEL_NAME, Value::String(raw.clone()))],
        )?),
        _ => Ok(json::del(text, &[MODEL_NAME])?),
    }
}

/// Deletes `path` when our removal left it literally empty.
fn prune_if_empty(text: &str, path: &str) -> Result<String, WiringError> {
    if is_empty_object(text, path) {
        Ok(json::del(text, &[path])?)
    } else {
        Ok(text.to_owned())
    }
}

fn is_empty_object(text: &str, path: &str) -> bool {
    json::get(text, path).as_deref() == Some("{}")
}

/// The standard Qoder agent.
pub fn qoder() -> QoderSpec {
    QoderSpec {
        layout: QoderLayout {
            id: "qoder",
            env_var: "QODER_CONFIG_DIR",
            default_dir: ".qoder",
        },
    }
}

/// The Qoder CN (China) agent.
pub fn qoder_cn() -> QoderSpec {
    QoderSpec {
        layout: QoderLayout {
            id: "qoder-cn",
            env_var: "QODERCN_CONFIG_DIR",
            default_dir: ".qoder-cn",
        },
    }
}
