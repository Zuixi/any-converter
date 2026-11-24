//! Snow specification (`~/.snow/profiles/any-converter.json`).
//!
//! Snow keeps one JSON file per profile plus `active-profile.json` selecting
//! the current one. The profile written here is an OpenAI-chat profile at the
//! gateway's `/v1`, with the selected catalog model in both the advanced and
//! basic slots and an optional `chatThinking` block carrying the effort.
//!
//! The active profile is also mirrored into `config.json` at the root, because
//! some Snow builds read the root file when no profile is resolved. Both the
//! profile bytes and the root file are stashed before connect, so a user's own
//! files return byte-for-byte on disconnect.
//!
//! The root is `$SNOW_CONFIG_DIR`, otherwise `~/.snow`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, remove_config, save_stash, write_config,
};
use crate::backup::Stash;
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Snow spec.
pub const OUR_KEYS: &[&str] = &["snowcfg", "activeProfile"];

/// Provider id written into the profile.
pub const PROVIDER_ID: &str = "any-converter";

const SNOWCFG: &str = "snowcfg";
const ACTIVE: &str = "activeProfile";

const STASH_PROFILE_EXISTS: &str = "__profile_exists";
const STASH_PROFILE_BYTES: &str = "__profile_bytes";
const STASH_CONFIG_EXISTS: &str = "__config_exists";
const STASH_CONFIG_BYTES: &str = "__config_bytes";
const STASH_ACTIVE_EXISTS: &str = "__active_exists";
const STASH_ACTIVE_VALUE: &str = "__active_value";

/// Reasoning levels Snow accepts in `chatThinking`.
const EFFORTS: &[&str] = &["low", "medium", "high"];

/// Snow wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SnowSpec;

impl SnowSpec {
    fn root(ctx: &HomeCtx) -> PathBuf {
        // Snow takes SNOW_CONFIG_DIR only when it is already absolute; a
        // relative value or a `~` it cannot resolve is ignored.
        if let Some(value) = (ctx.env)("SNOW_CONFIG_DIR") {
            let value = value.trim();
            if !value.is_empty() && Path::new(value).is_absolute() {
                return PathBuf::from(value);
            }
        }
        ctx.home.join(".snow")
    }

    fn profile_path(ctx: &HomeCtx) -> PathBuf {
        Self::root(ctx).join("profiles").join("any-converter.json")
    }

    fn active_path(ctx: &HomeCtx) -> PathBuf {
        Self::root(ctx).join("active-profile.json")
    }

    fn legacy_active_path(ctx: &HomeCtx) -> PathBuf {
        Self::root(ctx).join("active-profile.txt")
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::root(ctx).join("config.json")
    }

    /// The name of the active profile: `active-profile.json`, otherwise a
    /// legacy `active-profile.txt`, otherwise `default`.
    fn active_name(ctx: &HomeCtx) -> String {
        if let Ok(Some(text)) = read_config(&Self::active_path(ctx)) {
            if let Some(value) = json::get(&text, ACTIVE) {
                if !value.trim().is_empty() {
                    return value;
                }
            }
        }
        if let Ok(Some(text)) = read_config(&Self::legacy_active_path(ctx)) {
            let name = text.trim().trim_start_matches('\u{feff}');
            if !name.is_empty() {
                return name.to_owned();
            }
        }
        "default".to_owned()
    }
}

impl AgentSpec for SnowSpec {
    fn id(&self) -> &'static str {
        "snow"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::root(ctx);
        if config_dir.is_dir()
            || Self::profile_path(ctx).exists()
            || Self::active_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![
            Self::profile_path(ctx),
            Self::active_path(ctx),
            Self::config_path(ctx),
        ]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let profile_path = Self::profile_path(ctx);
        let active_path = Self::active_path(ctx);
        let config_path = Self::config_path(ctx);
        let profile_current = read_config(&profile_path)?;
        let config_current = read_config(&config_path)?;
        let active_current = read_config(&active_path)?;

        let mut stash = load_stash(ctx);
        if let Some(bytes) = &profile_current {
            record_once(&mut stash, self.id(), STASH_PROFILE_EXISTS, "1");
            record_once(&mut stash, self.id(), STASH_PROFILE_BYTES, bytes);
        }
        if let Some(bytes) = &config_current {
            record_once(&mut stash, self.id(), STASH_CONFIG_EXISTS, "1");
            record_once(&mut stash, self.id(), STASH_CONFIG_BYTES, bytes);
        }
        if let Some(bytes) = &active_current {
            record_once(&mut stash, self.id(), STASH_ACTIVE_EXISTS, "1");
            if let Some(value) = json::get(bytes, ACTIVE) {
                record_once(&mut stash, self.id(), STASH_ACTIVE_VALUE, &value);
            }
        }

        // Seed the profile from the one Snow is on, keeping the user's other
        // settings (streaming display, a system prompt, top-level flags) and
        // dropping a subscription sign-in; fall back to config.json, then to
        // Snow's own defaults.
        let active_name = Self::active_name(ctx);
        let seed = read_config(
            &Self::root(ctx)
                .join("profiles")
                .join(format!("{active_name}.json")),
        )?
        .or(read_config(&config_path)?);
        let mut profile = match seed {
            Some(text) if !text.trim().is_empty() => {
                serde_json::from_str::<Value>(&text).map_err(|error| {
                    WiringError::Refused(format!("Snow profile is not valid JSON: {error}"))
                })?
            }
            _ => Value::Object(Map::new()),
        };
        let object = profile
            .as_object_mut()
            .ok_or_else(|| WiringError::Refused("Snow profile root is not an object".to_owned()))?;
        if !object.get(SNOWCFG).is_some_and(Value::is_object) {
            object.insert(SNOWCFG.to_owned(), Value::Object(Map::new()));
        }
        let snowcfg = object
            .get_mut(SNOWCFG)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                WiringError::Refused("Snow profile's snowcfg is not an object".to_owned())
            })?;
        snowcfg.remove("oauth");
        snowcfg.insert(
            "baseUrl".to_owned(),
            json!(gateway_path(wiring.gateway_url, "v1")),
        );
        snowcfg.insert("baseUrlMode".to_owned(), json!("base"));
        snowcfg.insert("apiKey".to_owned(), json!(wiring.agent_key));
        snowcfg.insert("requestMethod".to_owned(), json!("chat"));
        snowcfg.insert("advancedModel".to_owned(), json!(wiring.default_model));
        snowcfg.insert("basicModel".to_owned(), json!(wiring.default_model));
        // The three settings Snow reads from the profile always come out
        // with a value: the catalog's window, 64000 output (cut to the
        // window), and text-only, since this catalog does not know vision.
        let context = wiring
            .catalog
            .find(wiring.default_model)
            .and_then(|model| model.context_tokens)
            .unwrap_or(200_000);
        snowcfg.insert("maxContextTokens".to_owned(), json!(context));
        snowcfg.insert("maxTokens".to_owned(), json!(64_000.min(context)));
        snowcfg.insert("supportsVision".to_owned(), json!(false));
        if let Some(effort) = wiring.effort.filter(|effort| !effort.is_empty()) {
            snowcfg.insert(
                "chatThinking".to_owned(),
                json!({ "enabled": true, "reasoning_effort": effort }),
            );
        }

        let profile_text = pretty_json(&profile)?;
        let active_text = json::set(
            active_current.as_deref().unwrap_or_default(),
            &[KV::new(ACTIVE, json!(PROVIDER_ID))],
        )?;

        let written = vec![
            write_config(&profile_path, &profile_text)?,
            write_config(&active_path, &active_text)?,
            write_config(&config_path, &profile_text)?,
        ];
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let profile_path = Self::profile_path(ctx);
        let active_path = Self::active_path(ctx);
        let config_path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if old.contains_key(STASH_PROFILE_EXISTS) {
            let bytes = old.get(STASH_PROFILE_BYTES).cloned().unwrap_or_default();
            written.push(write_config(&profile_path, &bytes)?);
        } else {
            remove_config(&profile_path)?;
        }

        if old.contains_key(STASH_CONFIG_EXISTS) {
            let bytes = old.get(STASH_CONFIG_BYTES).cloned().unwrap_or_default();
            written.push(write_config(&config_path, &bytes)?);
        } else {
            remove_config(&config_path)?;
        }

        if old.contains_key(STASH_ACTIVE_EXISTS) {
            let current = read_config(&active_path)?.unwrap_or_default();
            let text = match old.get(STASH_ACTIVE_VALUE) {
                Some(value) if !value.is_empty() => {
                    json::set(&current, &[KV::new(ACTIVE, json!(value))])?
                }
                _ => json::del(&current, &[ACTIVE])?,
            };
            written.push(write_config(&active_path, &text)?);
        } else {
            remove_config(&active_path)?;
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let profile = read_config(&Self::profile_path(ctx))
            .ok()
            .flatten()
            .or_else(|| read_config(&Self::config_path(ctx)).ok().flatten());
        let Some(profile) = profile else {
            return WiredState::Disconnected;
        };
        let base = json::get(&profile, &format!("{SNOWCFG}.baseUrl"));
        let key = json::get(&profile, &format!("{SNOWCFG}.apiKey"));
        let model = json::get(&profile, &format!("{SNOWCFG}.advancedModel"));

        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key_present = key.as_deref().is_some_and(|value| !value.is_empty());
        if !gateway && !key_present {
            return WiredState::Disconnected;
        }
        let model_present = model.as_deref().is_some_and(|value| !value.is_empty());
        let active_ours = Self::active_name(ctx) == PROVIDER_ID;
        if gateway && key_present && model_present && active_ours {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(profile)) = read_config(&Self::profile_path(ctx)) {
            for path in [
                format!("{SNOWCFG}.baseUrl"),
                format!("{SNOWCFG}.advancedModel"),
            ] {
                if let Some(value) = json::get(&profile, &path) {
                    values.insert(path, value);
                }
            }
        }
        if let Ok(Some(active)) = read_config(&Self::active_path(ctx)) {
            if let Some(value) = json::get(&active, ACTIVE) {
                values.insert(ACTIVE.to_owned(), value);
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Records a stashed value only when the namespace does not have it yet.
fn record_once(stash: &mut Stash, agent: &str, key: &str, value: &str) {
    if value.is_empty() || stash.old_values(agent).contains_key(key) {
        return;
    }
    let mut values = BTreeMap::new();
    values.insert(key.to_owned(), value.to_owned());
    stash.record(agent, values);
}

/// Serializes JSON with two-space indentation and a trailing newline.
fn pretty_json(value: &Value) -> Result<String, WiringError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| WiringError::Refused(format!("cannot serialize profile: {error}")))?;
    text.push('\n');
    Ok(text)
}
