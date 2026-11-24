//! Morph specification (`MISTER_MORPH_CONFIG` or `~/.morph/config.yaml`).
//!
//! Morph reads one YAML file and takes its upstream from the `llm` mapping:
//! the provider kind, the endpoint, the API key, the model, and optional
//! headers. Connect points that mapping at the gateway's `/v1` using the
//! OpenAI responses-compatible protocol; disconnect restores every touched
//! value and prunes an emptied `llm.headers`.

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

/// Configuration keys owned by the Morph spec.
pub const OUR_KEYS: &[&str] = &[
    "llm.inference_provider",
    "llm.endpoint",
    "llm.api_key",
    "llm.model",
    "llm.headers.User-Agent",
    "llm.context_window_tokens",
    "llm.reasoning_effort",
];

/// Reasoning levels Morph accepts.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// The User-Agent magpie has Morph send, in `llm.headers` (magpie
/// `morph.go:28`). Without it Morph's requests carry the OpenAI SDK's.
const MORPH_UA: &str = "mistermorph";

const PROVIDER: &str = "llm.inference_provider";
const ENDPOINT: &str = "llm.endpoint";
const API_KEY: &str = "llm.api_key";
const MODEL: &str = "llm.model";
const USER_AGENT: &str = "llm.headers.User-Agent";
const CONTEXT: &str = "llm.context_window_tokens";
const EFFORT: &str = "llm.reasoning_effort";

/// Every key connect may write and disconnect restores.
const MANAGED: &[&str] = &[
    PROVIDER, ENDPOINT, API_KEY, MODEL, USER_AGENT, CONTEXT, EFFORT,
];

/// The scalar `llm` children, paired with their full dotted path.
const SCALARS: &[(&str, &str)] = &[
    ("inference_provider", PROVIDER),
    ("endpoint", ENDPOINT),
    ("api_key", API_KEY),
    ("model", MODEL),
    ("context_window_tokens", CONTEXT),
    ("reasoning_effort", EFFORT),
];

/// Morph wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MorphSpec;

impl MorphSpec {
    /// Morph's configuration file, honoring an explicit path override.
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        match (ctx.env)("MISTER_MORPH_CONFIG") {
            Some(value) if !value.trim().is_empty() => expand_tilde(ctx, value.trim()),
            _ => join_relative(&ctx.home, ".morph/config.yaml"),
        }
    }
}

impl AgentSpec for MorphSpec {
    fn id(&self) -> &'static str {
        "morph"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let path = Self::config_path(ctx);
        let config_dir = path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.home.clone());
        if path.exists() || config_dir.is_dir() {
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
        for key in MANAGED {
            if let Some(value) = yaml::get(&config, key) {
                old.insert((*key).to_owned(), value);
            }
        }
        // The whole mapping is also recorded so disconnect can tell an
        // explicitly empty `llm` from an absent one.
        if let Some(value) = yaml::get(&config, "llm") {
            old.insert("llm".to_owned(), value);
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let kvs = vec![
            KV::new(PROVIDER, json!("openai_response_compatible")),
            KV::new(ENDPOINT, json!(support::base_url(wiring))),
            KV::new(API_KEY, json!(wiring.agent_key)),
            KV::new(MODEL, json!(wiring.default_model)),
            KV::new(USER_AGENT, json!(MORPH_UA)),
        ];
        let text = yaml::set(&config, &kvs)?;
        // A window the catalog does not know must not leave a stale value:
        // Morph would keep sizing the new model by the previous one's
        // (magpie morph.go:141-145).
        let text = match context_window(wiring) {
            Some(context) => yaml::set(&text, &[KV::new(CONTEXT, json!(context))])?,
            None => yaml::del(&text, &[CONTEXT])?,
        };
        let text = apply_effort(&text, wiring.effort)?;

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
            // Rebuild `llm` as a whole: deleting the last entry of a nested
            // mapping corrupts the line that follows it in the YAML editor.
            let rebuilt = rebuild_llm(&current, &old);
            let text = replace_mapping(&current, "llm", rebuilt, old.contains_key("llm"))?;
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
        let endpoint = yaml::get(&config, ENDPOINT);
        let key = yaml::get(&config, API_KEY);
        let endpoint = endpoint
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (endpoint, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in MANAGED {
                if let Some(value) = yaml::get(&config, key) {
                    values.insert((*key).to_owned(), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The selected model's context window, when the catalog knows it.
fn context_window(wiring: &WiringCtx<'_>) -> Option<u64> {
    wiring
        .catalog
        .find(wiring.default_model)
        .and_then(|model| model.context_tokens)
}

/// Writes `llm.reasoning_effort`; `None` leaves the file untouched and
/// `Some("")` removes our setting.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(yaml::del(text, &[EFFORT])?),
        Some(level) => Ok(yaml::set(text, &[KV::new(EFFORT, json!(level))])?),
    }
}

/// Builds the `llm` mapping with our keys removed and stashed values back.
fn rebuild_llm(text: &str, old: &BTreeMap<String, String>) -> Map<String, Value> {
    let mut mapping = parent_mapping(text, "llm");
    for (relative, full) in SCALARS {
        mapping.remove(*relative);
        if let Some(value) = old.get(*full).filter(|value| !value.is_empty()) {
            mapping.insert((*relative).to_owned(), scalar(value));
        }
    }

    match mapping.remove("headers") {
        Some(Value::Object(mut headers)) => {
            headers.remove("User-Agent");
            if let Some(value) = old.get(USER_AGENT).filter(|value| !value.is_empty()) {
                headers.insert("User-Agent".to_owned(), Value::String(value.clone()));
            }
            if !headers.is_empty() {
                mapping.insert("headers".to_owned(), Value::Object(headers));
            }
        }
        // A non-mapping `headers` is not ours to touch.
        Some(other) => {
            mapping.insert("headers".to_owned(), other);
        }
        None => {}
    }
    mapping
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

/// Parses a stashed YAML scalar back into its JSON type.
fn scalar(value: &str) -> Value {
    serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_owned()))
}
