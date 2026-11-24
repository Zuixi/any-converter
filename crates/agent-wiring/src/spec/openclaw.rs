//! OpenClaw specification (`openclaw.json`).
//!
//! OpenClaw reads OpenAI-compatible providers from `models.providers` and
//! selects one through `agents.defaults.model`, written as
//! `<provider>/<model>`. The selected model must also appear in
//! `agents.defaults.models`; that map is merged so the user's own entries stay
//! intact. When the file already has a `modelPolicy.allow` list, our
//! `any-converter/*` wildcard is appended to it; without such a list no policy
//! is invented.
//!
//! The config path is `$OPENCLAW_CONFIG_PATH`, otherwise
//! `$OPENCLAW_STATE_DIR/openclaw.json`, otherwise `~/.openclaw/openclaw.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenClaw spec.
pub const OUR_KEYS: &[&str] = &[
    "models.providers.any-converter",
    "agents.defaults.model",
    "agents.defaults.models",
    "agents.defaults.modelPolicy.allow",
];

/// Provider id written into `models.providers`.
pub const PROVIDER_ID: &str = "any-converter";

const PROVIDER_PATH: &str = "models.providers.any-converter";
const MODEL: &str = "agents.defaults.model";
const MODELS: &str = "agents.defaults.models";
const ALLOW: &str = "agents.defaults.modelPolicy.allow";
const MODEL_PREFIX: &str = "any-converter/";
const ALLOW_PATTERN: &str = "any-converter/*";

/// OpenClaw wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenClawSpec;

impl OpenClawSpec {
    /// OpenClaw's `~`: `$OPENCLAW_HOME`, with a leading `~` meaning the
    /// system home, otherwise the system home itself.
    fn openclaw_home(ctx: &HomeCtx) -> PathBuf {
        match (ctx.env)("OPENCLAW_HOME") {
            Some(value) => {
                let value = value.trim();
                if value.is_empty() || value.eq_ignore_ascii_case("undefined") || value == "null" {
                    ctx.home.clone()
                } else {
                    tilde_with(value, &ctx.home)
                }
            }
            None => ctx.home.clone(),
        }
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        let home = Self::openclaw_home(ctx);
        let mut dir = home.join(".openclaw");
        if let Some(profile) = (ctx.env)("OPENCLAW_PROFILE") {
            let profile = profile.trim();
            if !profile.is_empty()
                && !profile.eq_ignore_ascii_case("default")
                && valid_profile(profile)
            {
                dir = home.join(format!(".openclaw-{profile}"));
            }
        }
        if let Some(value) = (ctx.env)("OPENCLAW_STATE_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                dir = tilde_with(value, &home);
            }
        }
        let mut path = dir.join("openclaw.json");
        if let Some(value) = (ctx.env)("OPENCLAW_CONFIG_PATH") {
            let value = value.trim();
            if !value.is_empty() {
                path = tilde_with(value, &home);
            }
        }
        path
    }
}

/// A leading `~` in `value` resolved against `home`.
fn tilde_with(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    for prefix in ["~/", "~\\"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return super::join_relative(home, rest);
        }
    }
    PathBuf::from(value)
}

/// Whether `name` is a profile OpenClaw accepts:
/// `^[a-z0-9][a-z0-9_-]*$`, case-insensitive, at most 64 characters.
fn valid_profile(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

impl AgentSpec for OpenClawSpec {
    fn id(&self) -> &'static str {
        "openclaw"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let path = Self::config_path(ctx);
        let config_dir = path
            .parent()
            .map_or_else(|| path.clone(), |parent| parent.to_path_buf());
        if path.exists() || config_dir.is_dir() || super::bin_in_path(ctx, "openclaw") {
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
        let settings = read_config(&path)?.unwrap_or_default();
        let reference = format!("{MODEL_PREFIX}{}", wiring.default_model);

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[MODEL, MODELS, ALLOW]),
        );

        let provider = json!({
            "baseUrl": gateway_path(wiring.gateway_url, "v1"),
            "apiKey": wiring.agent_key,
            "api": "openai-completions",
            "models": provider_models(wiring),
        });
        let mut kvs = vec![KV::new(PROVIDER_PATH, provider)];
        // `agents.defaults.model` is a string or `{primary, fallbacks}`: an
        // object must keep its fallbacks, so only its primary moves.
        let model_is_object =
            json::get(&settings, MODEL).is_some_and(|value| value.trim_start().starts_with('{'));
        if model_is_object {
            kvs.push(KV::new(format!("{MODEL}.primary"), json!(reference)));
        } else {
            kvs.push(KV::new(MODEL, json!(reference)));
        }
        // `agents.defaults.models` is only updated where the file already
        // keeps it as a map; OpenClaw creates neither the map nor the entry.
        if let Some(models) = merged_models(&settings, &reference)? {
            kvs.push(KV::new(MODELS, models));
        }
        if let Some(allow) = merged_allow(&settings)? {
            kvs.push(KV::new(ALLOW, Value::Array(allow)));
        }

        let text = json::set(&settings, &kvs)?;
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
            // A stashed object (`{primary, fallbacks}`) must come back as an
            // object, not a quoted string.
            let mut text = restore_value(&current, MODEL, old.get(MODEL))?;
            text = strip_models(&text, old.get(MODELS))?;
            text = strip_allow(&text, old.get(ALLOW))?;
            text = json::del(&text, &[PROVIDER_PATH])?;
            text = prune_empty(&text, "models.providers")?;
            text = prune_empty(&text, "models")?;
            text = prune_empty(&text, "agents.defaults")?;
            text = prune_empty(&text, "agents")?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&settings, &format!("{PROVIDER_PATH}.baseUrl"));
        let model = model_primary(&settings);

        let selected = model
            .as_deref()
            .is_some_and(|value| value.starts_with(MODEL_PREFIX));
        if base.is_none() && !selected {
            return WiredState::Disconnected;
        }
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::config_path(ctx)) {
            for path in [
                format!("{PROVIDER_PATH}.baseUrl"),
                MODEL.to_owned(),
                ALLOW.to_owned(),
            ] {
                if let Some(value) = json::get(&settings, &path) {
                    values.insert(path, value);
                }
            }
        }
        values
    }
}

/// The provider's model list.
///
/// `reasoning` follows the catalog's per-model flag, `name` is the model id as
/// OpenClaw expects it, `input` starts text-only (this catalog knows nothing
/// about image input), and an unknown window is OpenClaw's own 128000
/// default.
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("name".to_owned(), json!(model.id));
            entry.insert("reasoning".to_owned(), json!(model.reasoning));
            entry.insert("input".to_owned(), json!(["text"]));
            entry.insert(
                "contextWindow".to_owned(),
                json!(model.context_tokens.unwrap_or(128_000)),
            );
            Value::Object(entry)
        })
        .collect()
}

/// The default models map with our selected model added, or `None` when the
/// file does not already keep that key as an object.
fn merged_models(text: &str, reference: &str) -> Result<Option<Value>, WiringError> {
    let Some(raw) = json::get(text, MODELS) else {
        return Ok(None);
    };
    let Value::Object(mut entries) = parse_value(MODELS, &raw)? else {
        return Ok(None);
    };
    entries.insert(reference.to_owned(), json!({}));
    Ok(Some(Value::Object(entries)))
}

/// The allow list with our wildcard appended, or `None` when there is no
/// non-empty list or it already admits one of our models.
fn merged_allow(text: &str) -> Result<Option<Vec<Value>>, WiringError> {
    let Some(raw) = json::get(text, ALLOW) else {
        return Ok(None);
    };
    let Value::Array(mut items) = parse_value(ALLOW, &raw)? else {
        return Err(WiringError::Refused(format!("{ALLOW} is not an array")));
    };
    if items.is_empty()
        || items.iter().any(|item| {
            item.as_str()
                .is_some_and(|value| value == "*" || value == ALLOW_PATTERN)
        })
    {
        return Ok(None);
    }
    items.push(json!(ALLOW_PATTERN));
    Ok(Some(items))
}

/// Parses a compact JSON string produced by [`json::get`].
fn parse_value(path: &str, raw: &str) -> Result<Value, WiringError> {
    serde_json::from_str(raw)
        .map_err(|error| WiringError::Refused(format!("{path} is not valid JSON: {error}")))
}

/// The primary of `agents.defaults.model`, whether it is a string or an
/// object with fallbacks.
fn model_primary(text: &str) -> Option<String> {
    let raw = json::get(text, MODEL)?;
    if !raw.trim_start().starts_with('{') {
        return Some(raw);
    }
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("primary")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Restores a stashed value, or removes the key when there was none.
///
/// `agents.defaults.model` may be an object (`{primary, fallbacks}`), which
/// `json::get` returns as compact JSON; it must go back as an object, not a
/// quoted string.
fn restore_value(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let trimmed = value.trim_start();
            let parsed = if trimmed.starts_with('{') || trimmed.starts_with('[') {
                serde_json::from_str(value).unwrap_or_else(|_| json!(value))
            } else {
                json!(value)
            };
            Ok(json::set(text, &[KV::new(path, parsed)])?)
        }
        _ => Ok(json::del(text, &[path])?),
    }
}

/// Removes the `any-converter/<model>` entries we added to the defaults
/// models map, leaving the user's own entries alone.
///
/// Our entries are the ones whose value is an empty object; an entry of the
/// user's that happens to be under our prefix carries settings and stays.
fn strip_models(text: &str, old: Option<&String>) -> Result<String, WiringError> {
    let Some(raw) = json::get(text, MODELS) else {
        return Ok(text.to_owned());
    };
    let Value::Object(entries) = parse_value(MODELS, &raw)? else {
        return Ok(text.to_owned());
    };
    let kept: Map<String, Value> = entries
        .into_iter()
        .filter(|(key, value)| {
            !(key.starts_with(MODEL_PREFIX) && value.as_object().is_some_and(Map::is_empty))
        })
        .collect();
    if kept.is_empty() {
        // Put back a map that was there before us (the user's own entries)
        // rather than deleting the key wholesale.
        if let Some(value) = old.filter(|value| !value.is_empty()) {
            if let Ok(parsed) = serde_json::from_str::<Value>(value) {
                return Ok(json::set(text, &[KV::new(MODELS, parsed)])?);
            }
        }
        Ok(json::del(text, &[MODELS])?)
    } else {
        Ok(json::set(text, &[KV::new(MODELS, Value::Object(kept))])?)
    }
}

/// Removes our wildcard from the allow list when we appended it, deleting
/// the list when emptied.
fn strip_allow(text: &str, old: Option<&String>) -> Result<String, WiringError> {
    let Some(raw) = json::get(text, ALLOW) else {
        return Ok(text.to_owned());
    };
    let Value::Array(items) = parse_value(ALLOW, &raw)? else {
        return Ok(text.to_owned());
    };
    let had = old
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| {
            value.as_array().map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some(ALLOW_PATTERN))
            })
        })
        .unwrap_or(false);
    if had {
        return Ok(text.to_owned());
    }
    let kept: Vec<Value> = items
        .into_iter()
        .filter(|item| item.as_str() != Some(ALLOW_PATTERN))
        .collect();
    if kept.is_empty() {
        Ok(json::del(text, &[ALLOW])?)
    } else {
        Ok(json::set(text, &[KV::new(ALLOW, Value::Array(kept))])?)
    }
}

/// Removes `path` when it is now an empty object.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
