//! QwenPaw specification (a custom provider file plus `agent.json`).
//!
//! QwenPaw keeps custom providers under a secret directory and each agent's
//! selected model in `<workspace>/agent.json` as
//! `active_model: { provider_id, model }`. The provider descriptor lists the
//! catalog as extra models and asks QwenPaw to forward the effort through the
//! model's `thinking_level`.
//!
//! The working directory is `$QWENPAW_WORKING_DIR`, otherwise a legacy
//! `~/.copaw` when it exists, otherwise `~/.qwenpaw`. The default agent's
//! workspace is `<working>/workspaces/default`, so its model file is
//! `<working>/workspaces/default/agent.json`.
//!
//! The secret directory is `$QWENPAW_SECRET_DIR`, which `<working>/.env` may
//! also set, otherwise the sibling `<working>.secret` (for example
//! `~/.qwenpaw.secret`). The provider file is
//! `providers/custom/any-converter.json` inside it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, expand_tilde,
    gateway_path, gw_base_match, load_stash, read_config, record_old_values, remove_config,
    save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the QwenPaw spec.
pub const OUR_KEYS: &[&str] = &[
    "agent.active_model",
    "agent.thinking_level",
    "providers/custom/any-converter.json",
];

/// Provider id written into the custom provider file.
pub const PROVIDER_ID: &str = "any-converter";

const ACTIVE_MODEL: &str = "active_model";
const THINKING: &str = "thinking_level";
const SUBAGENT_MODEL: &str = "subagent_model";
const FALLBACK_MODELS: &str = "fallback_models";
const DEFAULT_WORKSPACE: &str = "default";

/// Reasoning levels QwenPaw accepts as a thinking level.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Effort values advertised on each reasoning model.
const REASONING_OPTIONS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// QwenPaw wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct QwenPawSpec;

impl QwenPawSpec {
    fn work_dir(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("QWENPAW_WORKING_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                return expand_tilde(ctx, value);
            }
        }
        let legacy = ctx.home.join(".copaw");
        if legacy.exists() {
            return legacy;
        }
        ctx.home.join(".qwenpaw")
    }

    fn secret_dir(ctx: &HomeCtx) -> PathBuf {
        let work = Self::work_dir(ctx);
        if let Some(value) = (ctx.env)("QWENPAW_SECRET_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                return expand_tilde(ctx, value);
            }
        }
        if let Some(value) = dotenv_value(&work.join(".env"), "QWENPAW_SECRET_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                return expand_tilde(ctx, value);
            }
        }
        // QwenPaw's secret folder is the working folder's sibling, not a
        // child: `~/.qwenpaw` -> `~/.qwenpaw.secret`.
        let mut name: OsString = work.into_os_string();
        name.push(".secret");
        PathBuf::from(name)
    }

    fn provider_path(ctx: &HomeCtx) -> PathBuf {
        Self::secret_dir(ctx)
            .join("providers")
            .join("custom")
            .join("any-converter.json")
    }

    fn agent_path(ctx: &HomeCtx) -> PathBuf {
        Self::work_dir(ctx)
            .join("workspaces")
            .join(DEFAULT_WORKSPACE)
            .join("agent.json")
    }
}

impl AgentSpec for QwenPawSpec {
    fn id(&self) -> &'static str {
        "qwenpaw"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::work_dir(ctx);
        if config_dir.is_dir()
            || Self::agent_path(ctx).exists()
            || Self::provider_path(ctx).exists()
            || bin_in_path(ctx, "qwenpaw")
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::agent_path(ctx), Self::provider_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let agent_path = Self::agent_path(ctx);
        let provider_path = Self::provider_path(ctx);
        let agent = read_config(&agent_path)?.unwrap_or_default();
        let existing_provider = read_config(&provider_path)?;

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&agent, &[ACTIVE_MODEL, THINKING]),
        );

        let mut kvs = vec![KV::new(
            ACTIVE_MODEL,
            json!({ "provider_id": PROVIDER_ID, "model": wiring.default_model }),
        )];
        match wiring.effort {
            None => {}
            Some("") => {}
            Some(effort) => kvs.push(KV::new(THINKING, json!(effort))),
        }
        let mut agent_text = json::set(&agent, &kvs)?;
        if wiring.effort == Some("") {
            agent_text = json::del(&agent_text, &[THINKING])?;
        }

        let provider_text = pretty_json(&provider_value(wiring, existing_provider.as_deref())?)?;
        let written_agent = write_config(&agent_path, &agent_text)?;
        let written_provider = write_config(&provider_path, &provider_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_agent, written_provider])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let agent_path = Self::agent_path(ctx);
        let provider_path = Self::provider_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();
        let mut referenced = false;

        if let Some(current) = read_config(&agent_path)? {
            let mut text = restore_json(&current, ACTIVE_MODEL, old.get(ACTIVE_MODEL))?;
            text = restore_string(&text, THINKING, old.get(THINKING))?;
            referenced = references_provider(&text);
            written.push(write_config(&agent_path, &text)?);
        }

        // `subagent_model` and `fallback_models` keep the provider alive just
        // as `active_model` does.
        if !referenced && provider_path.exists() {
            remove_config(&provider_path)?;
            written.push(provider_path);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let agent = read_config(&Self::agent_path(ctx)).ok().flatten();
        let provider = read_config(&Self::provider_path(ctx)).ok().flatten();
        if agent.is_none() && provider.is_none() {
            return WiredState::Disconnected;
        }
        let agent = agent.unwrap_or_default();
        let provider_id = json::get(&agent, &format!("{ACTIVE_MODEL}.provider_id"));
        let model = json::get(&agent, &format!("{ACTIVE_MODEL}.model"));
        let base = provider.and_then(|text| json::get(&text, "base_url"));

        let selected = provider_id.as_deref() == Some(PROVIDER_ID)
            && model.is_some_and(|value| !value.is_empty());
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        if !selected && !gateway {
            return WiredState::Disconnected;
        }
        if selected && gateway {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(agent)) = read_config(&Self::agent_path(ctx)) {
            for path in [
                format!("{ACTIVE_MODEL}.provider_id"),
                format!("{ACTIVE_MODEL}.model"),
                THINKING.to_owned(),
            ] {
                if let Some(value) = json::get(&agent, &path) {
                    values.insert(path, value);
                }
            }
        }
        if let Ok(Some(provider)) = read_config(&Self::provider_path(ctx)) {
            if let Some(value) = json::get(&provider, "base_url") {
                values.insert("base_url".to_owned(), value);
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The custom provider descriptor, merged over a provider file the user or
/// QwenPaw itself already wrote.
///
/// QwenPaw writes defaults into the file (discovery flags, generation
/// arguments, headers, a schema version) and its console lets the user change
/// settings beside ours, so only our keys are replaced; everything else
/// survives.
fn provider_value(wiring: &WiringCtx<'_>, existing: Option<&str>) -> Result<Value, WiringError> {
    let mut provider = match existing {
        Some(text) if !text.trim().is_empty() => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                return Err(WiringError::Refused(
                    "QwenPaw's provider root is not an object".to_owned(),
                ));
            }
            Err(error) => {
                return Err(WiringError::Refused(format!(
                    "QwenPaw's provider is not valid JSON: {error}"
                )));
            }
        },
        _ => Map::new(),
    };

    for (key, value) in [
        ("enabled", json!(true)),
        ("support_model_discovery", json!(false)),
        ("support_connection_check", json!(false)),
        ("models", json!([])),
        ("generate_kwargs", json!({})),
        ("custom_headers", json!({})),
        ("snapshot_schema_version", json!(2)),
    ] {
        provider.entry(key.to_owned()).or_insert(value);
    }

    provider.insert("id".to_owned(), json!(PROVIDER_ID));
    provider.insert("name".to_owned(), json!(PROVIDER_ID));
    provider.insert(
        "base_url".to_owned(),
        json!(gateway_path(wiring.gateway_url, "v1")),
    );
    provider.insert("api_key".to_owned(), json!(wiring.agent_key));
    provider.insert("chat_model".to_owned(), json!("OpenAIChatModel"));
    provider.insert("is_custom".to_owned(), json!(true));
    let models = merged_models(wiring, &provider);
    provider.insert("extra_models".to_owned(), Value::Array(models));
    Ok(Value::Object(provider))
}

/// The catalog as QwenPaw's `extra_models`, laid over entries of the same id
/// already in the file so user edits (an output limit, generation settings)
/// survive a reconnect.
fn merged_models(wiring: &WiringCtx<'_>, provider: &Map<String, Value>) -> Vec<Value> {
    let existing = provider
        .get("extra_models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let id = model.get("id").and_then(Value::as_str)?;
                    Some((id.to_owned(), model.clone()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    super::support::catalog_models(wiring, |model| {
        let mut entry = existing
            .get(&model.id)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        entry.insert("id".to_owned(), json!(model.id));
        entry.insert("name".to_owned(), json!(model.name));
        entry.insert("source".to_owned(), json!("user"));
        entry.insert("supports_tool_calling".to_owned(), json!(true));
        if let Some(context) = model.context_tokens {
            entry.insert("max_input_length".to_owned(), json!(context));
        }
        // QwenPaw only applies an agent's thinking level to a model whose card
        // declares how it thinks, so the effort fields belong on reasoning
        // models alone.
        if model.reasoning {
            entry.insert("thinking_param_style".to_owned(), json!("effort"));
            entry.insert(
                "reasoning_effort_options".to_owned(),
                json!(REASONING_OPTIONS),
            );
        } else {
            entry.remove("thinking_param_style");
            entry.remove("reasoning_effort_options");
        }
        Value::Object(entry)
    })
}

/// Whether any of the default agent's model slots still name our provider.
fn references_provider(agent: &str) -> bool {
    if json::get(agent, &format!("{ACTIVE_MODEL}.provider_id")).as_deref() == Some(PROVIDER_ID) {
        return true;
    }
    if json::get(agent, &format!("{SUBAGENT_MODEL}.provider_id")).as_deref() == Some(PROVIDER_ID) {
        return true;
    }
    let Some(raw) = json::get(agent, FALLBACK_MODELS) else {
        return false;
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| {
            value.as_array().map(|models| {
                models.iter().any(|model| {
                    model.get("provider_id").and_then(Value::as_str) == Some(PROVIDER_ID)
                })
            })
        })
        .unwrap_or(false)
}

/// A `KEY=value` line from a `.env` file, as python-dotenv reads one.
fn dotenv_value(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut value = None;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").map_or(line, str::trim);
        let Some((name, raw)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let raw = raw.trim();
        let parsed = if raw.len() >= 2 && (raw.starts_with('"') || raw.starts_with('\'')) {
            let quote = raw.chars().next().unwrap_or('"');
            match raw[1..].find(quote) {
                Some(end) => &raw[1..1 + end],
                None => raw,
            }
        } else {
            match raw.find(" #") {
                Some(index) => raw[..index].trim(),
                None => raw,
            }
        };
        value = Some(parsed.to_owned());
    }
    value
}

/// Restores a stashed JSON value, or removes the key when there was none.
fn restore_json(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let parsed: Value =
                serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()));
            Ok(json::set(text, &[KV::new(path, parsed)])?)
        }
        _ => Ok(json::del(text, &[path])?),
    }
}

/// Restores a stashed string, or removes the key when there was none.
fn restore_string(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

/// Serializes JSON with two-space indentation and a trailing newline.
fn pretty_json(value: &Value) -> Result<String, WiringError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| WiringError::Refused(format!("cannot serialize provider: {error}")))?;
    text.push('\n');
    Ok(text)
}
