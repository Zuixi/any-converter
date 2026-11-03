//! Gemini CLI specification (`~/.gemini/.env`, `~/.gemini/settings.json`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, is_gateway_base, load_stash,
    read_config, record_old_values, save_stash, write_config,
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
];

const SELECTED_TYPE: &str = "security.auth.selectedType";
const GEMINI_API_KEY_TYPE: &str = "GEMINI_API_KEY";

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
        if config_dir.is_dir() || Self::env_path(ctx).exists() || Self::settings_path(ctx).exists()
        {
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

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut next_env = env_edit::set(
            &env_text,
            &[
                KV::new("GOOGLE_GEMINI_BASE_URL", json!(wiring.gateway_url)),
                KV::new("GEMINI_API_KEY", json!(wiring.agent_key)),
                KV::new("GEMINI_MODEL", json!(wiring.default_model)),
            ],
        );
        next_env = env_edit::del(&next_env, &["GOOGLE_API_KEY"]);

        let mut written = vec![write_config(&env_path, &next_env)?];

        if let Some(selected) = json::get(&settings_text, SELECTED_TYPE) {
            if selected != GEMINI_API_KEY_TYPE {
                let next_settings = json::set(
                    &settings_text,
                    &[KV::new(SELECTED_TYPE, json!(GEMINI_API_KEY_TYPE))],
                )?;
                written.push(write_config(&settings_path, &next_settings)?);
            }
        }

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
            let text = match old.get("settings.security.auth.selectedType") {
                Some(value) if !value.is_empty() => {
                    json::set(&current, &[KV::new(SELECTED_TYPE, json!(value))])?
                }
                _ => json::del(&current, &[SELECTED_TYPE])?,
            };
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx) -> WiredState {
        let Ok(Some(env_text)) = read_config(&Self::env_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = env_edit::get(&env_text, "GOOGLE_GEMINI_BASE_URL");
        let key = env_edit::get(&env_text, "GEMINI_API_KEY");
        let gateway = base.as_deref().is_some_and(is_gateway_base);
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn base_url_key(&self) -> Option<&'static str> {
        Some("GOOGLE_GEMINI_BASE_URL")
    }

    fn read_key(&self, ctx: &HomeCtx, key: &str) -> Option<String> {
        let env_text = read_config(&Self::env_path(ctx)).ok().flatten()?;
        env_edit::get(&env_text, key)
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(env_text)) = read_config(&Self::env_path(ctx)) {
            for key in [
                "GOOGLE_GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                "GEMINI_MODEL",
                "GOOGLE_API_KEY",
            ] {
                if let Some(value) = env_edit::get(&env_text, key) {
                    values.insert(format!("env.{key}"), value);
                }
            }
        }
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            if let Some(value) = json::get(&settings, SELECTED_TYPE) {
                values.insert("settings.security.auth.selectedType".to_owned(), value);
            }
        }
        values
    }
}
