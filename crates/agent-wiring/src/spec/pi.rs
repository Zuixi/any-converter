//! Pi specification (`PI_CODING_AGENT_DIR` or `~/.pi/agent`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gateway_path,
    gw_base_match, join_relative, load_stash, read_config, record_old_values, save_stash,
    write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Pi spec.
pub const OUR_KEYS: &[&str] = &[
    "models.providers.any-converter",
    "settings.defaultProvider",
    "settings.defaultModel",
    "settings.enabledModels",
];

pub(crate) const PROVIDER: &str = "any-converter";

/// Pi wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct PiSpec;

impl PiSpec {
    /// Pi's agent directory.
    ///
    /// `PI_CODING_AGENT_DIR` is honored only when it resolves to an absolute
    /// path (a leading `~` expands against the home). A relative value is
    /// resolved against Pi's working directory, which the gateway cannot know
    /// (magpie `pi_dir_test.go`), so it is ignored in favor of `~/.pi/agent`.
    fn dir(ctx: &HomeCtx) -> PathBuf {
        if let Some(value) = (ctx.env)("PI_CODING_AGENT_DIR") {
            let value = value.trim();
            if !value.is_empty() {
                let path = expand_tilde(ctx, value);
                if path.is_absolute() {
                    return path;
                }
            }
        }
        join_relative(&ctx.home, ".pi/agent")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }

    fn models_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("models.json")
    }
}

impl AgentSpec for PiSpec {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir()
            || Self::settings_path(ctx).exists()
            || Self::models_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx), Self::models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = Self::settings_path(ctx);
        let models_path = Self::models_path(ctx);
        let settings = read_config(&settings_path)?.unwrap_or_default();
        let models = read_config(&models_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in ["defaultProvider", "defaultModel", "enabledModels"] {
            if let Some(value) = json::get(&settings, key) {
                old.insert(format!("settings.{key}"), value);
            }
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let provider = json!({
            "name": PROVIDER,
            "baseUrl": gateway_path(wiring.gateway_url, "v1"),
            "api": "openai-completions",
            "apiKey": wiring.agent_key,
            // Pi names the session to a provider only when asked to; without
            // this its Chat and Messages turns reach the gateway with no
            // session id (magpie agents.go:690-691).
            "compat": { "sendSessionAffinityHeaders": true },
            "models": provider_models(wiring),
        });
        let models_text = json::set(&models, &[KV::new("providers.any-converter", provider)])?;

        let mut settings_text = json::set(
            &settings,
            &[
                KV::new("defaultProvider", json!(PROVIDER)),
                KV::new("defaultModel", json!(wiring.default_model)),
            ],
        )?;
        settings_text = add_enabled_model(&settings_text, PROVIDER, wiring.default_model)?;

        let written_models = write_config(&models_path, &models_text)?;
        let written_settings = write_config(&settings_path, &settings_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_models, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = Self::settings_path(ctx);
        let models_path = Self::models_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&models_path)? {
            let text = json::del(&current, &["providers.any-converter"])?;
            written.push(write_config(&models_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let mut sets = Vec::new();
            let mut deletes = Vec::new();
            for key in ["defaultProvider", "defaultModel"] {
                match old.get(&format!("settings.{key}")) {
                    Some(value) if !value.is_empty() => {
                        sets.push(KV::new(key, json!(value)));
                    }
                    _ => deletes.push(key),
                }
            }
            let mut text = json::set(&current, &sets)?;
            text = json::del(&text, &deletes)?;
            text = strip_enabled_models(&text, PROVIDER)?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(models)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&models, "providers.any-converter.baseUrl");
        let key = json::get(&models, "providers.any-converter.apiKey");
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in ["defaultProvider", "defaultModel", "enabledModels"] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(format!("settings.{key}"), value);
                }
            }
        }
        if let Ok(Some(models)) = read_config(&Self::models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json::get(&models, &format!("providers.any-converter.{key}")) {
                    values.insert(format!("models.providers.any-converter.{key}"), value);
                }
            }
        }
        values
    }
}

/// Pi's provider model list: an ARRAY of entries with an `id` key, not an
/// id-keyed map (magpie `agents.go:675-691`, `pi_thinking_test.go`).
///
/// Catalog approximation: our [`CatalogModel`](crate::sync::catalog::CatalogModel)
/// has no images/efforts/output data, so every entry carries only the id,
/// name, reasoning flag, and window (as `contextWindow`). Pi treats a model
/// without `maxTokens` as capped at its own conservative default, which we
/// accept; image input is simply absent from `input`, matching magpie's
/// `!Images` default.
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    wiring
        .catalog
        .models
        .iter()
        .map(|model| {
            let mut entry = Map::new();
            entry.insert("id".to_owned(), json!(model.id));
            entry.insert("name".to_owned(), json!(model.name));
            entry.insert("reasoning".to_owned(), json!(model.reasoning));
            if let Some(context) = model.context_tokens {
                // Pi takes every model for a 128K one without this key
                // (magpie `agents.go:906-908`).
                entry.insert("contextWindow".to_owned(), json!(context));
            }
            Value::Object(entry)
        })
        .collect()
}

/// Thinking-level suffixes Pi accepts after a `provider/model` reference.
const LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Reports whether an `enabledModels` pattern already covers `reference`.
///
/// Matching follows Pi's rules (magpie `pi.go:42-71`): a trailing `:level` is
/// ignored, comparison is case-insensitive, and a glob may cover either the
/// whole `provider/model` reference or the model id after the provider.
/// Unlike Go's `path.Match`, `*` here crosses `/`, so `any-converter/*` covers
/// every model the gateway writes.
pub(crate) fn enabled_model_takes(pattern: &str, reference: &str) -> bool {
    let mut pattern = pattern.trim();
    if let Some((base, level)) = pattern.rsplit_once(':') {
        if LEVELS.contains(&level.to_ascii_lowercase().as_str()) {
            pattern = base;
        }
    }
    let pattern = pattern.to_ascii_lowercase();
    let reference = reference.to_ascii_lowercase();
    if pattern == reference {
        return true;
    }
    if !pattern.contains(['*', '?', '[']) {
        return false;
    }
    let id = reference.split_once('/').map_or("", |(_, rest)| rest);
    glob_match(
        &pattern.chars().collect::<Vec<_>>(),
        &reference.chars().collect::<Vec<_>>(),
    ) || (!id.is_empty()
        && glob_match(
            &pattern.chars().collect::<Vec<_>>(),
            &id.chars().collect::<Vec<_>>(),
        ))
}

/// Matches one glob against text (`*` any run including `/`, `?` one char).
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return text.is_empty();
    };
    match first {
        '*' => glob_match(rest, text) || (!text.is_empty() && glob_match(pattern, &text[1..])),
        '?' => !text.is_empty() && glob_match(rest, &text[1..]),
        '[' => match rest.iter().position(|ch| *ch == ']') {
            Some(close) => {
                let Some(&ch) = text.first() else {
                    return false;
                };
                class_matches(&rest[..close], ch) && glob_match(&rest[close + 1..], &text[1..])
            }
            None => text.first() == Some(&'[') && glob_match(rest, &text[1..]),
        },
        other => text.first() == Some(&other) && glob_match(rest, &text[1..]),
    }
}

fn class_matches(class: &[char], ch: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') | Some('^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut matched = false;
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == '-' {
            if class[index] <= ch && ch <= class[index + 2] {
                matched = true;
            }
            index += 3;
        } else {
            if class[index] == ch {
                matched = true;
            }
            index += 1;
        }
    }
    matched != negated
}

/// Adds `provider/model` to an existing `enabledModels` list unless a pattern
/// there already covers it.
///
/// A missing or non-array list is left untouched: Pi treats an absent
/// allow-list as "everything", and the list is the user's to create.
pub(crate) fn add_enabled_model(
    settings: &str,
    provider: &str,
    model: &str,
) -> Result<String, WiringError> {
    let Some(raw) = json::get(settings, "enabledModels") else {
        return Ok(settings.to_owned());
    };
    let Ok(Value::Array(mut items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(settings.to_owned());
    };
    let reference = format!("{provider}/{model}");
    if items
        .iter()
        .filter_map(Value::as_str)
        .any(|pattern| enabled_model_takes(pattern, &reference))
    {
        return Ok(settings.to_owned());
    }
    items.push(Value::String(reference));
    Ok(json::set(
        settings,
        &[KV::new("enabledModels", Value::Array(items))],
    )?)
}

/// Removes the plain `provider/model` entries this spec added.
///
/// Globs (`any-converter/*`) are left alone: they are the user's own scope,
/// exactly as magpie's `piScopeWithout` keeps them. A list left empty is
/// deleted rather than written as `[]`.
pub(crate) fn strip_enabled_models(settings: &str, provider: &str) -> Result<String, WiringError> {
    let Some(raw) = json::get(settings, "enabledModels") else {
        return Ok(settings.to_owned());
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(settings.to_owned());
    };
    let prefix = format!("{provider}/");
    let mut kept = Vec::new();
    for item in items {
        let ours = item
            .as_str()
            .is_some_and(|value| !value.contains(['*', '?', '[']) && value.starts_with(&prefix));
        if !ours {
            kept.push(item);
        }
    }
    if kept.is_empty() {
        Ok(json::del(settings, &["enabledModels"])?)
    } else {
        Ok(json::set(
            settings,
            &[KV::new("enabledModels", Value::Array(kept))],
        )?)
    }
}
