//! OpenChamber specification (`~/.config/openchamber`).
//!
//! OpenChamber is a desktop shell around OpenCode. It stores its own model
//! choice in `preferences.json` (a versioned, timestamped field map) and in
//! `settings.json`, while the underlying OpenCode install still needs the
//! gateway provider entry. Both are written so the model picked here survives
//! whichever file the shell reads first.
//!
//! The data directory is `$OPENCHAMBER_DATA_DIR`, otherwise a plain
//! `~/.config/openchamber` (deliberately not XDG-shifted). The OpenCode config
//! follows OpenCode's own resolution: `$OPENCODE_CONFIG_DIR`, otherwise
//! `<xdg config>/opencode`, with the first existing file among
//! `opencode.jsonc`, `opencode.json`, and `config.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    join_relative, load_stash, read_config, record_old_values, save_stash, write_config,
    xdg_config_home,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the OpenChamber spec.
pub const OUR_KEYS: &[&str] = &[
    "preferences.fields.defaultModel",
    "preferences.fields.defaultVariant",
    "preferences.fields.smallModelUseDefault",
    "preferences.fields.smallModelOverride",
    "settings.defaultModel",
    "settings.defaultVariant",
    "settings.smallModelUseDefault",
    "settings.smallModelOverride",
    "provider.any-converter",
    "providers.any-converter",
];

/// Provider id written into the OpenCode config.
pub const PROVIDER_ID: &str = "any-converter";

const MODEL_PREFIX: &str = "any-converter/";
const V1_PATH: &str = "provider.any-converter";
const V2_PATH: &str = "providers.any-converter";

const PREF_VERSION: &str = "version";
const PREF_DEFAULT_MODEL: &str = "fields.defaultModel";
const PREF_DEFAULT_VARIANT: &str = "fields.defaultVariant";
const PREF_SMALL_USE_DEFAULT: &str = "fields.smallModelUseDefault";
const PREF_SMALL_OVERRIDE: &str = "fields.smallModelOverride";

const SET_DEFAULT_MODEL: &str = "defaultModel";
const SET_DEFAULT_VARIANT: &str = "defaultVariant";
const SET_SMALL_USE_DEFAULT: &str = "smallModelUseDefault";
const SET_SMALL_OVERRIDE: &str = "smallModelOverride";

const OPENCODE_CANDIDATES: &[&str] = &["opencode.jsonc", "opencode.json", "config.json"];

/// Reasoning levels OpenChamber accepts as a variant.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// OpenChamber wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenChamberSpec;

impl OpenChamberSpec {
    fn data_dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "OPENCHAMBER_DATA_DIR", |ctx| {
            join_relative(&ctx.home, ".config/openchamber")
        })
    }

    fn preferences_path(ctx: &HomeCtx) -> PathBuf {
        Self::data_dir(ctx).join("preferences.json")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::data_dir(ctx).join("settings.json")
    }

    fn opencode_path(ctx: &HomeCtx) -> PathBuf {
        let dir = env_dir(ctx, "OPENCODE_CONFIG_DIR", |ctx| {
            xdg_config_home(ctx).join("opencode")
        });
        OPENCODE_CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| dir.join("opencode.json"))
    }
}

impl AgentSpec for OpenChamberSpec {
    fn id(&self) -> &'static str {
        "openchamber"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::data_dir(ctx);
        if config_dir.is_dir()
            || Self::preferences_path(ctx).exists()
            || Self::settings_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![
            Self::preferences_path(ctx),
            Self::settings_path(ctx),
            Self::opencode_path(ctx),
        ]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let preferences_path = Self::preferences_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let opencode_path = Self::opencode_path(ctx);
        let preferences_current = read_config(&preferences_path)?;
        let preferences = preferences_current.clone().unwrap_or_default();
        let settings = read_config(&settings_path)?.unwrap_or_default();
        let opencode = read_config(&opencode_path)?.unwrap_or_default();

        // A preferences.json that exists but is not a version-1 document is
        // OpenChamber's (or a future build's): refuse to rewrite it rather
        // than destroy fields we do not understand.
        if preferences_current.is_some() {
            if let Some(version) = json::get(&preferences, PREF_VERSION) {
                let parsed: Value = serde_json::from_str(&version).unwrap_or(Value::Null);
                if parsed.as_i64() != Some(1) {
                    return Err(WiringError::Refused(format!(
                        "OpenChamber's preferences.json has version {version}, not 1; \
                         refusing to rewrite it"
                    )));
                }
            }
        }

        let mut old = BTreeMap::new();
        for key in [
            PREF_VERSION,
            PREF_DEFAULT_MODEL,
            PREF_DEFAULT_VARIANT,
            PREF_SMALL_USE_DEFAULT,
            PREF_SMALL_OVERRIDE,
        ] {
            if let Some(value) = json::get(&preferences, key) {
                old.insert(format!("preferences.{key}"), value);
            }
        }
        for key in [
            SET_DEFAULT_MODEL,
            SET_DEFAULT_VARIANT,
            SET_SMALL_USE_DEFAULT,
            SET_SMALL_OVERRIDE,
        ] {
            if let Some(value) = json::get(&settings, key) {
                old.insert(format!("settings.{key}"), value);
            }
        }
        // The OpenCode provider blocks are the user's until we overwrite
        // them: record them so disconnect can put a user's own block back
        // instead of deleting it.
        for key in [V1_PATH, V2_PATH] {
            if let Some(value) = json::get(&opencode, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let now = unix_millis();
        let reference = format!("{MODEL_PREFIX}{}", wiring.default_model);
        let mut preference_kvs = vec![
            KV::new(PREF_VERSION, json!(1)),
            KV::new(
                PREF_DEFAULT_MODEL,
                json!({ "value": reference, "updatedAt": now }),
            ),
            KV::new(
                PREF_SMALL_USE_DEFAULT,
                json!({ "value": false, "updatedAt": now }),
            ),
            KV::new(
                PREF_SMALL_OVERRIDE,
                json!({ "value": reference, "updatedAt": now }),
            ),
        ];
        let mut setting_kvs = vec![
            KV::new(SET_DEFAULT_MODEL, json!(reference)),
            KV::new(SET_SMALL_USE_DEFAULT, json!(false)),
            KV::new(SET_SMALL_OVERRIDE, json!(reference)),
        ];
        match wiring.effort {
            None => {}
            Some("") => {}
            Some(effort) => {
                preference_kvs.push(KV::new(
                    PREF_DEFAULT_VARIANT,
                    json!({ "value": effort, "updatedAt": now }),
                ));
                setting_kvs.push(KV::new(SET_DEFAULT_VARIANT, json!(effort)));
            }
        }

        let mut preference_text = json::set(&preferences, &preference_kvs)?;
        if wiring.effort == Some("") {
            preference_text = json::del(&preference_text, &[PREF_DEFAULT_VARIANT])?;
        }
        let mut setting_text = json::set(&settings, &setting_kvs)?;
        if wiring.effort == Some("") {
            setting_text = json::del(&setting_text, &[SET_DEFAULT_VARIANT])?;
        }

        let base = gateway_path(wiring.gateway_url, "v1");
        let mut kvs = vec![KV::new(V1_PATH, v1_provider(wiring, &base))];
        // OpenCode 1 rejects unknown top-level keys: only refresh a `providers`
        // block the file already keeps (2.x native), never introduce one.
        if json::get(&opencode, V2_PATH).is_some() {
            kvs.push(KV::new(V2_PATH, v2_provider(wiring, &base)));
        }
        let opencode_text = json::set(&opencode, &kvs)?;

        let written = vec![
            write_config(&preferences_path, &preference_text)?,
            write_config(&settings_path, &setting_text)?,
            write_config(&opencode_path, &opencode_text)?,
        ];
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let preferences_path = Self::preferences_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let opencode_path = Self::opencode_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        let settings_text = match read_config(&settings_path)? {
            Some(current) => {
                let mut text = restore_string(
                    &current,
                    SET_DEFAULT_MODEL,
                    old.get("settings.defaultModel"),
                )?;
                text = restore_string(
                    &text,
                    SET_DEFAULT_VARIANT,
                    old.get("settings.defaultVariant"),
                )?;
                text = restore_bool(
                    &text,
                    SET_SMALL_USE_DEFAULT,
                    old.get("settings.smallModelUseDefault"),
                )?;
                text = restore_string(
                    &text,
                    SET_SMALL_OVERRIDE,
                    old.get("settings.smallModelOverride"),
                )?;
                written.push(write_config(&settings_path, &text)?);
                text
            }
            None => String::new(),
        };

        let preferences_text = match read_config(&preferences_path)? {
            Some(current) => {
                let mut text =
                    restore_json(&current, PREF_VERSION, old.get("preferences.version"))?;
                text = restore_json(
                    &text,
                    PREF_DEFAULT_MODEL,
                    old.get("preferences.fields.defaultModel"),
                )?;
                text = restore_json(
                    &text,
                    PREF_DEFAULT_VARIANT,
                    old.get("preferences.fields.defaultVariant"),
                )?;
                text = restore_json(
                    &text,
                    PREF_SMALL_USE_DEFAULT,
                    old.get("preferences.fields.smallModelUseDefault"),
                )?;
                text = restore_json(
                    &text,
                    PREF_SMALL_OVERRIDE,
                    old.get("preferences.fields.smallModelOverride"),
                )?;
                text = prune_empty(&text, "fields")?;
                written.push(write_config(&preferences_path, &text)?);
                text
            }
            None => String::new(),
        };

        let referenced =
            references_provider(&settings_text) || preferences_reference(&preferences_text);
        if !referenced {
            if let Some(current) = read_config(&opencode_path)? {
                let mut text = json::del(&current, &[V1_PATH, V2_PATH])?;
                // Provider blocks stash raw JSON: put the user's pre-connect
                // blocks back once ours are gone, then prune what we created.
                text = restore_json(&text, V1_PATH, old.get(V1_PATH))?;
                text = restore_json(&text, V2_PATH, old.get(V2_PATH))?;
                text = prune_empty(&text, "provider")?;
                text = prune_empty(&text, "providers")?;
                written.push(write_config(&opencode_path, &text)?);
            }
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let settings = read_config(&Self::settings_path(ctx))
            .ok()
            .flatten()
            .unwrap_or_default();
        let preferences = read_config(&Self::preferences_path(ctx))
            .ok()
            .flatten()
            .unwrap_or_default();
        let Ok(Some(opencode)) = read_config(&Self::opencode_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let block = provider_value(&opencode);
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
        // A newer OpenChamber reads preferences.json first; settings.json is
        // the older build's copy. Either naming our model means ours.
        let selected = json::get(&settings, SET_DEFAULT_MODEL)
            .or_else(|| json::get(&preferences, &format!("{PREF_DEFAULT_MODEL}.value")));
        let selected_ok = selected
            .as_deref()
            .is_some_and(|value| value.starts_with(MODEL_PREFIX));

        if block.is_none() && !selected_ok {
            return WiredState::Disconnected;
        }
        let gateway = base.is_some_and(|value| gw_base_match(value, gateway_url));
        if gateway && key_present && selected_ok {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in [SET_DEFAULT_MODEL, SET_DEFAULT_VARIANT] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        if let Ok(Some(preferences)) = read_config(&Self::preferences_path(ctx)) {
            if let Some(value) = json::get(&preferences, PREF_DEFAULT_MODEL) {
                values.insert(PREF_DEFAULT_MODEL.to_owned(), value);
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// OpenCode 1.x provider shape.
fn v1_provider(wiring: &WiringCtx<'_>, base: &str) -> Value {
    json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": "Any Converter",
        "options": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
            "setCacheKey": true,
        },
        "models": models(wiring),
    })
}

/// OpenCode 2.x provider shape.
fn v2_provider(wiring: &WiringCtx<'_>, base: &str) -> Value {
    json!({
        "name": "Any Converter",
        "package": "aisdk:@ai-sdk/openai-compatible",
        "settings": {
            "baseURL": base,
            "apiKey": wiring.agent_key,
        },
        "compatibility": { "supportsPromptCacheKey": true },
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
        entries.insert(model.id.clone(), Value::Object(entry));
    }
    entries
}

/// Reads whichever provider shape the OpenCode file uses.
fn provider_value(settings: &str) -> Option<Value> {
    let raw = json::get(settings, V2_PATH).or_else(|| json::get(settings, V1_PATH))?;
    serde_json::from_str(&raw).ok()
}

/// Whether OpenChamber's settings point at the gateway provider.
fn references_provider(settings: &str) -> bool {
    [SET_DEFAULT_MODEL, SET_SMALL_OVERRIDE]
        .iter()
        .any(|key| json::get(settings, key).is_some_and(|value| value.starts_with(MODEL_PREFIX)))
}

/// Whether OpenChamber's preferences point at the gateway provider.
fn preferences_reference(preferences: &str) -> bool {
    [PREF_DEFAULT_MODEL, PREF_SMALL_OVERRIDE].iter().any(|key| {
        json::get(preferences, key)
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("value")
                    .and_then(Value::as_str)
                    .map(|value| value.starts_with(MODEL_PREFIX))
            })
            .unwrap_or(false)
    })
}

/// Restores a stashed string, or removes the key when there was none.
fn restore_string(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

/// Restores a stashed boolean, or removes the key when there was none.
fn restore_bool(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => {
            json::set(text, &[KV::new(path, json!(value == "true"))])?
        }
        _ => json::del(text, &[path])?,
    })
}

/// Restores a stashed JSON value, or removes the key when there was none.
fn restore_json(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => {
            let parsed: Value = serde_json::from_str(value)
                .map_err(|error| WiringError::Refused(format!("invalid stashed value: {error}")))?;
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
