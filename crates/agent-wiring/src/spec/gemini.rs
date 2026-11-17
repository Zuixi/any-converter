//! Gemini CLI specification (`~/.gemini/.env`, `~/.gemini/settings.json`).
//!
//! Gemini CLI speaks Google's own API only, so "auth" is the provider:
//! `security.auth.selectedType` in `settings.json` picks Google sign-in, a
//! Gemini API key, or Vertex AI. Pointing `GOOGLE_GEMINI_BASE_URL` at the
//! gateway and `GEMINI_API_KEY` at the agent key (both in `~/.gemini/.env`,
//! which the CLI loads) makes it an API-key session against the gateway, and
//! `model.name` picks the catalog model it starts on.
//!
//! `~/.gemini` is shared with Antigravity (which keeps its own folders there),
//! so the directory alone says nothing about Gemini CLI: detection asks for
//! the CLI's own files or its binary.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, env as env_edit, json};
use crate::error::WiringError;

/// Configuration keys owned by the Gemini CLI spec.
pub const OUR_KEYS: &[&str] = &[
    "env.GOOGLE_GEMINI_BASE_URL",
    "env.GEMINI_API_KEY",
    "env.GEMINI_MODEL",
    "env.GOOGLE_API_KEY",
    "settings.security.auth.selectedType",
    "settings.model.name",
];

const SELECTED_TYPE: &str = "security.auth.selectedType";
const MODEL_NAME: &str = "model.name";
/// The `selectedType` that makes Gemini CLI use the key from `.env`.
const API_KEY_AUTH: &str = "gemini-api-key";
/// Files Gemini CLI leaves in `~/.gemini`; Antigravity's folders differ.
const CLI_MARKERS: &[&str] = &[
    "settings.json",
    "projects.json",
    "oauth_creds.json",
    "google_accounts.json",
];

/// Gemini CLI wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiSpec;

impl GeminiSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        ctx.home.join(".gemini")
    }

    fn env_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(".env")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join("settings.json")
    }
}

impl AgentSpec for GeminiSpec {
    fn id(&self) -> &'static str {
        "gemini-cli"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        let tagged = CLI_MARKERS
            .iter()
            .any(|name| config_dir.join(name).exists());
        // Antigravity creates `~/.gemini` too, so the directory is not
        // evidence; the binary on PATH covers a CLI that has not run yet.
        if tagged || bin_in_path(ctx, "gemini") {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::env_path(ctx), Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let env_path = Self::env_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let env_text = read_config(&env_path)?.unwrap_or_default();
        let settings_text = read_config(&settings_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [
            "GOOGLE_GEMINI_BASE_URL",
            "GEMINI_API_KEY",
            "GEMINI_MODEL",
            "GOOGLE_API_KEY",
        ] {
            if let Some(value) = env_edit::get(&env_text, key) {
                old.insert(format!("env.{key}"), value);
            }
        }
        if let Some(value) = json::get(&settings_text, SELECTED_TYPE) {
            old.insert("settings.security.auth.selectedType".to_owned(), value);
        }
        if let Some(value) = json::get(&settings_text, MODEL_NAME) {
            old.insert("settings.model.name".to_owned(), value);
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut next_env = env_edit::set(
            &env_text,
            &[
                KV::new("GOOGLE_GEMINI_BASE_URL", json!(wiring.gateway_url)),
                KV::new("GEMINI_API_KEY", json!(wiring.agent_key)),
            ],
        );
        // `GOOGLE_API_KEY` outranks `GEMINI_API_KEY`, and an older version of
        // this spec wrote `GEMINI_MODEL`, which the CLI ignores.
        next_env = env_edit::del(&next_env, &["GOOGLE_API_KEY", "GEMINI_MODEL"]);

        let mut written = vec![write_config(&env_path, &next_env)?];

        let next_settings = json::set(
            &settings_text,
            &[
                KV::new(SELECTED_TYPE, json!(API_KEY_AUTH)),
                KV::new(MODEL_NAME, json!(wiring.default_model)),
            ],
        )?;
        written.push(write_config(&settings_path, &next_settings)?);

        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let env_path = Self::env_path(ctx);
        let settings_path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&env_path)? {
            let mut text = current;
            let mut sets = Vec::new();
            let mut deletes = Vec::new();
            for key in [
                "GOOGLE_GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                "GEMINI_MODEL",
                "GOOGLE_API_KEY",
            ] {
                match old.get(&format!("env.{key}")) {
                    Some(value) if !value.is_empty() => {
                        sets.push(KV::new(key, json!(value)));
                    }
                    _ => deletes.push(key),
                }
            }
            text = env_edit::set(&text, &sets);
            text = env_edit::del(&text, &deletes);
            written.push(write_config(&env_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let mut text = current;
            text = restore(
                &text,
                SELECTED_TYPE,
                old.get("settings.security.auth.selectedType"),
            )?;
            text = restore(&text, MODEL_NAME, old.get("settings.model.name"))?;
            text = prune_empty(&text, "model")?;
            text = prune_empty(&text, "security.auth")?;
            text = prune_empty(&text, "security")?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(env_text)) = read_config(&Self::env_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = env_edit::get(&env_text, "GOOGLE_GEMINI_BASE_URL").filter(|v| !v.is_empty());
        let key = env_edit::get(&env_text, "GEMINI_API_KEY").filter(|v| !v.is_empty());
        if base.is_none() && key.is_none() {
            return WiredState::Disconnected;
        }

        let settings = read_config(&Self::settings_path(ctx)).ok().flatten();
        let auth = settings
            .as_deref()
            .is_some_and(|text| json::get(text, SELECTED_TYPE).as_deref() == Some(API_KEY_AUTH));
        let model = settings
            .as_deref()
            .is_some_and(|text| json::get(text, MODEL_NAME).is_some_and(|value| !value.is_empty()));
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));

        if gateway && key.is_some() && auth && model {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(env_text)) = read_config(&Self::env_path(ctx)) {
            for key in ["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY"] {
                if let Some(value) = env_edit::get(&env_text, key) {
                    values.insert(format!("env.{key}"), value);
                }
            }
        }
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for path in [SELECTED_TYPE, MODEL_NAME] {
                if let Some(value) = json::get(&settings, path) {
                    values.insert(format!("settings.{path}"), value);
                }
            }
        }
        values
    }
}

/// Restores one stashed value, or removes the key when there was none.
fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => json::set(text, &[KV::new(path, json!(value))])?,
        _ => json::del(text, &[path])?,
    })
}

/// Removes `path` when it is now an empty object.
fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match json::get(text, path) {
        Some(value) if value.trim() == "{}" => Ok(json::del(text, &[path])?),
        _ => Ok(text.to_owned()),
    }
}
