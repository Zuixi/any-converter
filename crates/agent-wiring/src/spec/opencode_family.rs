//! OpenCode-format forks: mimocode and deveco.
//!
//! Both agents copied OpenCode's provider schema: an OpenAI-compatible
//! provider under `provider` (1.x) or `providers` (2.x), a model map keyed by
//! catalog id, and the top-level `model` / `small_model` selectors written as
//! `<provider>/<model>`. One parameterized spec covers the family; only the
//! agent id, environment override, directory name, and executable differ.
//!
//! The config file is chosen the way each fork chooses it: the first existing
//! file among `<name>.jsonc`, `<name>.json`, and `config.json`, otherwise a
//! fresh `<name>.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, gateway_path,
    gw_base_match, load_stash, read_config, record_old_values, save_stash, write_config,
    xdg_config_home,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by every OpenCode-family spec.
pub const OUR_KEYS: &[&str] = &[
    "provider.any-converter",
    "providers.any-converter",
    "model",
    "small_model",
];

/// Provider id written into both config shapes.
pub const PROVIDER_ID: &str = "any-converter";

const V1_PATH: &str = "provider.any-converter";
const V2_PATH: &str = "providers.any-converter";
const MODEL: &str = "model";
const SMALL_MODEL: &str = "small_model";

/// One OpenCode-format agent's wiring.
#[derive(Debug, Clone, Copy)]
pub struct OpenCodeFamilySpec {
    /// Stable agent id (`mimocode`, `deveco`).
    pub id: &'static str,
    /// Environment variable overriding the configuration location.
    pub env_var: &'static str,
    /// Directory name used under the XDG configuration root.
    pub dir_name: &'static str,
    /// Executable name used for installation detection.
    pub bin: &'static str,
}

impl OpenCodeFamilySpec {
    /// Resolves the agent's configuration directory.
    ///
    /// mimocode's `MIMOCODE_HOME` names a home root whose configuration lives
    /// in a `config` subdirectory; deveco's `DEVECO_CONFIG_DIR` names the
    /// configuration directory itself. Both overrides apply only when the
    /// value is already an absolute path: a relative value (or a `~` that was
    /// never expanded) is ignored, as each fork ignores it.
    fn dir(&self, ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)(self.env_var) {
            let value = value.trim();
            if !value.is_empty() {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    if self.id == "mimocode" {
                        return path.join("config");
                    }
                    return path;
                }
            }
        }
        xdg_config_home(ctx).join(self.dir_name)
    }

    /// The first existing candidate file, otherwise a fresh `<name>.json`.
    fn config_path(&self, ctx: &HomeCtx) -> PathBuf {
        let dir = self.dir(ctx);
        let candidates = [
            format!("{}.jsonc", self.dir_name),
            format!("{}.json", self.dir_name),
            "config.json".to_owned(),
        ];
        candidates
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| dir.join(format!("{}.json", self.dir_name)))
    }
}

impl AgentSpec for OpenCodeFamilySpec {
    fn id(&self) -> &'static str {
        self.id
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = self.dir(ctx);
        if config_dir.is_dir() || bin_in_path(ctx, self.bin) {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![self.config_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.config_path(ctx);
        let settings = read_config(&path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, OUR_KEYS),
        );

        let base = gateway_path(wiring.gateway_url, "v1");
        let selected = format!("{PROVIDER_ID}/{}", wiring.default_model);
        let mut kvs = vec![KV::new(MODEL, json!(selected.clone()))];
        // A small model the user chose is theirs and survives a connect; only
        // an absent one, or one of ours, is (re)pointed at the gateway.
        let small_is_ours = json::get(&settings, SMALL_MODEL)
            .as_deref()
            .is_none_or(|value| value.starts_with(&format!("{PROVIDER_ID}/")));
        if small_is_ours {
            kvs.push(KV::new(SMALL_MODEL, json!(selected)));
        }

        let native = json::get(&settings, V2_PATH).is_some();
        if native {
            // OpenCode 2 reads `providers`: write it in 2.x words, and drop a
            // leftover 1.x entry, which 2.x would ignore (and 1.x would read
            // in the wrong shape).
            let block = v2_provider(wiring, &base, supports_cache_key(&settings));
            kvs.push(KV::new(V2_PATH, block));
        } else {
            // v1 rejects unknown top-level keys, so `providers` is never
            // introduced into a file that does not already keep it.
            let block = v1_provider(wiring, &base, cache_key(&settings));
            kvs.push(KV::new(V1_PATH, block));
        }
        let mut text = json::set(&settings, &kvs)?;
        if native {
            text = json::del(&text, &[V1_PATH])?;
        }
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = self.config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = json::del(&current, &[V1_PATH, V2_PATH])?;
            text = restore_value(&text, MODEL, old.get(MODEL))?;
            text = restore_value(&text, SMALL_MODEL, old.get(SMALL_MODEL))?;
            // Provider blocks stash raw JSON, not scalars: restore the user's
            // pre-connect block whole instead of leaving it deleted.
            text = restore_block(&text, V1_PATH, old.get(V1_PATH))?;
            text = restore_block(&text, V2_PATH, old.get(V2_PATH))?;
            text = prune_empty(&text, "provider")?;
            text = prune_empty(&text, "providers")?;
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&self.config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let block = provider_value(&settings);
        let base = block
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/options/baseURL")
                    .or_else(|| value.pointer("/settings/baseURL"))
            })
            .and_then(Value::as_str);
        let key_present = block
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/options/apiKey")
                    .or_else(|| value.pointer("/settings/apiKey"))
            })
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let selected = json::get(&settings, MODEL);
        let model_known = block
            .as_ref()
            .and_then(|value| value.get("models"))
            .and_then(Value::as_object)
            .is_some_and(|models| {
                selected
                    .as_deref()
                    .and_then(|id| id.split_once('/'))
                    .is_some_and(|(provider, model)| {
                        provider == PROVIDER_ID && models.contains_key(model)
                    })
            });

        let selected_ours = selected
            .as_deref()
            .is_some_and(|id| id.starts_with(&format!("{PROVIDER_ID}/")));
        if block.is_none() && !selected_ours {
            return WiredState::Disconnected;
        }
        let gateway = base.is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && key_present && model_known && selected_ours {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&self.config_path(ctx)) {
            for path in [V1_PATH, MODEL] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
            }
        }
        values
    }
}

/// OpenCode 1's provider shape, written under `provider`.
fn v1_provider(wiring: &WiringCtx<'_>, base: &str, cache_key: bool) -> Value {
    json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": PROVIDER_ID,
        "options": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
            "setCacheKey": cache_key,
        },
        "models": models(wiring),
    })
}

/// OpenCode 2's provider shape, written under `providers`.
///
/// A 1.x-shaped entry there has no `settings.baseURL`, so OpenCode 2 cannot
/// reach the provider at all.
fn v2_provider(wiring: &WiringCtx<'_>, base: &str, supports_cache_key: bool) -> Value {
    json!({
        "name": PROVIDER_ID,
        "package": "aisdk:@ai-sdk/openai-compatible",
        "settings": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
        },
        "compatibility": { "supportsPromptCacheKey": supports_cache_key },
        "models": models(wiring),
    })
}

/// Model metadata keyed by catalog id.
fn models(wiring: &WiringCtx<'_>) -> Map<String, Value> {
    let mut entries = Map::new();
    for model in &wiring.catalog.models {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), json!(model.name));
        if let Some(context) = model.context_tokens {
            entry.insert(
                "limit".to_owned(),
                json!({ "context": context, "output": context.min(8192) }),
            );
        }
        // OpenCode only offers a thinking level for a model marked reasoning.
        if model.reasoning {
            entry.insert("reasoning".to_owned(), json!(true));
        }
        entries.insert(model.id.clone(), Value::Object(entry));
    }
    entries
}

/// A user's existing `options.setCacheKey`, defaulting to ours.
fn cache_key(settings: &str) -> bool {
    json::get(settings, "provider.any-converter.options.setCacheKey")
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true)
}

/// A user's existing `compatibility.supportsPromptCacheKey`, defaulting to
/// ours.
fn supports_cache_key(settings: &str) -> bool {
    json::get(
        settings,
        "providers.any-converter.compatibility.supportsPromptCacheKey",
    )
    .and_then(|value| value.parse::<bool>().ok())
    .unwrap_or(true)
}

/// Reads whichever provider shape the file uses.
fn provider_value(settings: &str) -> Option<Value> {
    let raw = json::get(settings, V2_PATH).or_else(|| json::get(settings, V1_PATH))?;
    serde_json::from_str(&raw).ok()
}

/// Restores a stashed value, or removes the key when there was none.
///
/// `json::get` returns a string without its quotes, while an object (OpenCode
/// 2's `model` may be `{providerID, model, variant}`) comes back as compact
/// JSON. Parsing objects and arrays keeps that object from being written back
/// as a quoted string.
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

/// Removes `path` when it is now an empty object.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}

/// Writes back a stashed provider block, whose stashed value is raw JSON.
fn restore_block(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let parsed: Value = serde_json::from_str(value)
                .map_err(|error| WiringError::Refused(format!("invalid stashed value: {error}")))?;
            json::set(text, &[KV::new(path, parsed)]).map_err(WiringError::from)
        }
        _ => Ok(text.to_owned()),
    }
}

/// mimocode wiring (`MIMOCODE_HOME`, `mimocode.json`, `mimo`).
pub fn mimocode() -> OpenCodeFamilySpec {
    OpenCodeFamilySpec {
        id: "mimocode",
        env_var: "MIMOCODE_HOME",
        dir_name: "mimocode",
        bin: "mimo",
    }
}

/// deveco wiring (`DEVECO_CONFIG_DIR`, `deveco.json`, `deveco`).
pub fn deveco() -> OpenCodeFamilySpec {
    OpenCodeFamilySpec {
        id: "deveco",
        env_var: "DEVECO_CONFIG_DIR",
        dir_name: "deveco",
        bin: "deveco",
    }
}
