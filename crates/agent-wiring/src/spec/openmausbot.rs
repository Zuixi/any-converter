//! OpenMausBot specification (`$OMB_DATA_DIR/config.json`).
//!
//! OpenMausBot launches agent instances from `instances.<name>` blocks. Wiring
//! registers an `any-converter` instance using the `claudeAgent` driver,
//! pointing the Claude environment at the gateway root. Our instance is merged
//! into the existing `instances` map rather than replacing the file: the saved
//! map is the whole fleet, and other engines must survive. (Unlike magpie we do
//! not seed a large default fleet; merging preserves whatever the user has.)
//!
//! The endpoint model list includes every catalog id, using Claude's own
//! spelling (`claude-opus-4-5`, without a date suffix) for Claude-named models.
//! Ids are filtered through the character set OpenMausBot accepts; when none
//! remain the instance is skipped. `ANTHROPIC_MODEL` is the first endpoint
//! model, in its written spelling.

use std::collections::BTreeMap;

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gw_base_match,
    load_stash, read_config, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenMausBot spec.
pub const OUR_KEYS: &[&str] = &["instances.any-converter"];

const PROVIDER: &str = "any-converter";
const INSTANCE_PATH: &str = "instances.any-converter";
const INSTANCES: &str = "instances";

/// OpenMausBot wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenMausBotSpec;

impl OpenMausBotSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        // A relative `OMB_DATA_DIR` is not a valid location; OpenMausBot
        // itself requires an absolute path, so ignore anything else.
        if let Some(value) = (ctx.env)("OMB_DATA_DIR") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                let expanded = expand_tilde(ctx, trimmed);
                if expanded.is_absolute() {
                    return expanded;
                }
            }
        }
        ctx.home.join(".openmausbot")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("config.json")
    }

    fn config_dir(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("providers").join(PROVIDER)
    }
}

impl AgentSpec for OpenMausBotSpec {
    fn id(&self) -> &'static str {
        "openmausbot"
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
        let current = read_config(&path)?.unwrap_or_default();

        let endpoint_models = endpoint_models(wiring);
        let Some(model) = endpoint_models.first().cloned() else {
            // Nothing the driver would accept: leave the file untouched.
            return Ok(Vec::new());
        };

        let instance = json!({
            "driver": "claudeAgent",
            "displayName": PROVIDER,
            "access": "custom",
            "config": {
                "configDir": Self::config_dir(ctx).to_string_lossy().into_owned(),
                "requireApiKey": true,
                "endpointModels": endpoint_models,
            },
            "environment": {
                "ANTHROPIC_BASE_URL": wiring.gateway_url,
                "ANTHROPIC_AUTH_TOKEN": wiring.agent_key,
                "ANTHROPIC_API_KEY": wiring.agent_key,
                "ANTHROPIC_MODEL": model,
            },
        });

        // Merge into the saved fleet map; a lone-entry replacement would drop
        // every other engine.
        let mut instances = instances(&current)?;
        instances.insert(PROVIDER.to_owned(), instance);
        let text = json::set(&current, &[KV::new(INSTANCES, Value::Object(instances))])?;
        let written = write_config(&path, &text)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let text = json::del_pruning(&current, &[INSTANCE_PATH])?;
            written.push(write_config(&path, &text)?);
        }

        let mut stash = load_stash(ctx);
        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(
            &current,
            "instances.any-converter.environment.ANTHROPIC_BASE_URL",
        );
        let token = json::get(
            &current,
            "instances.any-converter.environment.ANTHROPIC_AUTH_TOKEN",
        );
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = token.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(current)) = read_config(&Self::config_path(ctx)) {
            for key in ["ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL"] {
                if let Some(value) = json::get(
                    &current,
                    &format!("instances.any-converter.environment.{key}"),
                ) {
                    values.insert(format!("{INSTANCE_PATH}.environment.{key}"), value);
                }
            }
        }
        values
    }
}

/// Reads the existing `instances` map, refusing a non-object value.
fn instances(text: &str) -> Result<Map<String, Value>, WiringError> {
    match value_at(text, INSTANCES) {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(WiringError::Refused(format!(
            "{INSTANCES} is not an object"
        ))),
    }
}

/// The endpoint model list the Claude driver accepts.
///
/// Every catalog model is included; Claude-named ids use Claude's spelling.
/// Ids that fail the driver's character rules are dropped.
fn endpoint_models(wiring: &WiringCtx<'_>) -> Vec<String> {
    let mut models = Vec::new();
    for model in &wiring.catalog.models {
        let spelled = if model.id.to_ascii_lowercase().contains("claude-") {
            super::claude::claude_name(&model.id)
        } else {
            None
        };
        let candidate = spelled.unwrap_or_else(|| model.id.clone());
        if valid_endpoint_id(&candidate) && !models.contains(&candidate) {
            models.push(candidate);
        }
    }
    models
}

/// Whether an id matches `(?i)^[a-z0-9][a-z0-9._:/-]*(?:\[1m\])?$`.
fn valid_endpoint_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    let core = if lower.ends_with("[1m]") {
        &id[..id.len() - 4]
    } else {
        id
    };
    let mut chars = core.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '/' | '-'))
}

fn value_at(text: &str, path: &str) -> Option<Value> {
    serde_json::from_str(&json::get(text, path)?).ok()
}
