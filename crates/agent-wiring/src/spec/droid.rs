//! Factory's Droid specification (`FACTORY_HOME_OVERRIDE` or `~/.factory`).
//!
//! Droid keeps user-supplied models in `settings.json`'s `customModels` array,
//! each naming the upstream model, the base URL, the key, and the wire
//! protocol (`generic-chat-completion-api` for Chat Completions). The model a
//! session starts on is `sessionDefaultSettings.model` and is referenced as
//! `custom:<id>`.
//!
//! Entries written here use ids under `custom:any-converter/`, which is what
//! tells them from the user's own. The array is replaced wholesale, so the
//! user's entries stay untouched and in front of ours.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gw_base_match, load_stash,
    read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Droid spec.
pub const OUR_KEYS: &[&str] = &["customModels", "sessionDefaultSettings.model"];

const MODELS: &str = "customModels";
const DEFAULT_MODEL: &str = "sessionDefaultSettings.model";
const ID_PREFIX: &str = "custom:any-converter/";
const PROVIDER: &str = "generic-chat-completion-api";

/// Reasoning levels Droid accepts per custom model.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Droid wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct DroidSpec;

impl DroidSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        match (ctx.env)("FACTORY_HOME_OVERRIDE") {
            Some(home) if !home.trim().is_empty() => {
                super::expand_tilde(ctx, home.trim()).join(".factory")
            }
            _ => ctx.home.join(".factory"),
        }
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for DroidSpec {
    fn id(&self) -> &'static str {
        "droid"
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

        let mut old = BTreeMap::new();
        if let Some(value) = json::get(&settings, DEFAULT_MODEL) {
            old.insert(DEFAULT_MODEL.to_owned(), value);
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut entries = user_entries(&settings, MODELS)?;
        entries.extend(
            wiring
                .catalog
                .models
                .iter()
                .map(|model| entry(wiring, &model.id, &model.name, model.context_tokens)),
        );

        let text = json::set(
            &settings,
            &[
                KV::new(MODELS, Value::Array(entries)),
                KV::new(
                    DEFAULT_MODEL,
                    json!(format!("{ID_PREFIX}{}", wiring.default_model)),
                ),
            ],
        )?;
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
            let kept = user_entries(&current, MODELS)?;
            let mut text = if kept.is_empty() {
                json::del(&current, &[MODELS])?
            } else {
                json::set(&current, &[KV::new(MODELS, Value::Array(kept))])?
            };

            text = match old.get(DEFAULT_MODEL) {
                Some(value) if !value.is_empty() => {
                    json::set(&text, &[KV::new(DEFAULT_MODEL, json!(value))])?
                }
                _ => json::del(&text, &[DEFAULT_MODEL])?,
            };
            if json::get(&text, "sessionDefaultSettings").is_some_and(|value| value.trim() == "{}")
            {
                text = json::del(&text, &["sessionDefaultSettings"])?;
            }

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
        let ours = our_entries(&settings, MODELS);
        let base = ours
            .iter()
            .all(|entry| entry_base(entry).is_some_and(|value| gw_base_match(value, gateway_url)));
        let key = ours
            .iter()
            .all(|entry| entry_key(entry).is_some_and(|value| !value.is_empty()));
        let selected = json::get(&settings, DEFAULT_MODEL).is_some_and(|name| {
            ours.iter()
                .any(|entry| entry_id(entry).is_some_and(|id| id == name))
        });

        if ours.is_empty() {
            return WiredState::Disconnected;
        }
        if base && key && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for path in [MODELS, DEFAULT_MODEL] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(path.to_owned(), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// One custom model entry as Droid reads it.
fn entry(wiring: &WiringCtx<'_>, id: &str, name: &str, context: Option<u64>) -> Value {
    let provider = provider_for(id);
    let mut entry = serde_json::Map::new();
    entry.insert("model".to_owned(), json!(id));
    entry.insert("id".to_owned(), json!(format!("{ID_PREFIX}{id}")));
    entry.insert(
        "displayName".to_owned(),
        json!(if name.is_empty() { id } else { name }),
    );
    // Droid's OpenAI SDKs join their path to `/v1`; its Anthropic client adds
    // the `/v1` itself, so that provider takes the gateway root.
    entry.insert(
        "baseUrl".to_owned(),
        json!(base_url(wiring.gateway_url, provider)),
    );
    entry.insert("apiKey".to_owned(), json!(wiring.agent_key));
    entry.insert("provider".to_owned(), json!(provider));
    if let Some(context) = context {
        entry.insert("maxContextLimit".to_owned(), json!(context));
    }
    if let Some(effort) = wiring.effort.filter(|effort| !effort.is_empty()) {
        entry.insert("reasoningEffort".to_owned(), json!(effort));
    }
    // The catalog carries no image flag, so every entry is text-only (the
    // magpie default for a model whose `Images` is false). Droid warns at
    // session start about a model it reads as OpenAI's or Claude's on
    // another provider, which is why those get a provider of their own.
    entry.insert("noImageSupport".to_owned(), json!(true));
    Value::Object(entry)
}

/// The provider Droid is told a custom model speaks.
fn provider_for(id: &str) -> &'static str {
    let lower = id.to_ascii_lowercase();
    if looks_like_gpt(&lower) {
        "openai"
    } else if ["claude", "haiku", "sonnet", "opus"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        "anthropic"
    } else {
        PROVIDER
    }
}

fn base_url(gateway_url: &str, provider: &str) -> String {
    if provider == "anthropic" {
        gateway_url.trim_end_matches('/').to_owned()
    } else {
        super::gateway_path(gateway_url, "v1")
    }
}

/// Matches the model names Droid itself reads as OpenAI's: `gpt-5`,
/// `gpt_5`, `gpt.5`, any `codex`, or `o3`/`o4-mini` (`\bgpt[-_.]?\d`,
/// `codex`, `\bo[1-9](?:[-_.]|$)` case-insensitively).
fn looks_like_gpt(lower: &str) -> bool {
    if lower.contains("codex") {
        return true;
    }
    let chars: Vec<char> = lower.chars().collect();
    let word = |index: usize| -> bool { index == 0 || !is_word_char(chars[index - 1]) };
    for index in 0..chars.len() {
        if chars[index] == 'g'
            && chars.get(index + 1) == Some(&'p')
            && chars.get(index + 2) == Some(&'t')
            && word(index)
        {
            let mut next = index + 3;
            if chars
                .get(next)
                .is_some_and(|ch| matches!(*ch, '-' | '_' | '.'))
            {
                next += 1;
            }
            if chars.get(next).is_some_and(char::is_ascii_digit) {
                return true;
            }
        }
        if chars[index] == 'o'
            && word(index)
            && chars
                .get(index + 1)
                .is_some_and(|ch| matches!(*ch, '1'..='9'))
        {
            match chars.get(index + 2) {
                None => return true,
                Some(ch) if matches!(*ch, '-' | '_' | '.') => return true,
                _ => {}
            }
        }
    }
    false
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Reads `path` as an array, refusing to clobber a non-array value.
fn read_array(text: &str, path: &str) -> Result<Vec<Value>, WiringError> {
    let Some(raw) = json::get(text, path) else {
        return Ok(Vec::new());
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(WiringError::Refused(format!("{path} is not an array"))),
        Err(error) => Err(WiringError::Refused(format!(
            "{path} is not valid JSON: {error}"
        ))),
    }
}

/// Entries in `path` that this spec did not write.
fn user_entries(text: &str, path: &str) -> Result<Vec<Value>, WiringError> {
    Ok(read_array(text, path)?
        .into_iter()
        .filter(|entry| !is_ours(entry))
        .collect())
}

/// Entries in `path` that this spec wrote.
fn our_entries(text: &str, path: &str) -> Vec<Value> {
    read_array(text, path)
        .unwrap_or_default()
        .into_iter()
        .filter(is_ours)
        .collect()
}

fn is_ours(entry: &Value) -> bool {
    entry_id(entry).is_some_and(|id| id.starts_with(ID_PREFIX))
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id")?.as_str()
}

fn entry_base(entry: &Value) -> Option<&str> {
    entry.get("baseUrl")?.as_str()
}

fn entry_key(entry: &Value) -> Option<&str> {
    entry.get("apiKey")?.as_str()
}
