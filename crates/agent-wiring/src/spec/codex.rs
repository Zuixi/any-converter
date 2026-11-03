//! Codex specification (`~/.codex/config.toml`, `auth.json`, catalog JSON).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path,
    is_gateway_base, load_stash, read_config, record_old_values, remove_config, save_stash,
    write_config,
};
use crate::edit::{EditError, KV, json, toml};
use crate::error::WiringError;

/// Configuration keys owned by the Codex spec.
pub const OUR_KEYS: &[&str] = &[
    "config.model_provider",
    "config.model",
    "config.model_catalog_json",
    "config.model_providers.any-converter",
    "auth.OPENAI_API_KEY",
];

/// Codex wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexSpec;

const PROVIDER: &str = "any-converter";
const PROVIDER_TABLE: &str = "model_providers.any-converter";

impl CodexSpec {
    fn home(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "CODEX_HOME", |home| home.home.join(".codex"))
    }

    fn config_path(ctx: &HomeCtx) -> PathBuf {
        Self::home(ctx).join("config.toml")
    }

    fn auth_path(ctx: &HomeCtx) -> PathBuf {
        Self::home(ctx).join("auth.json")
    }

    fn catalog_path(ctx: &HomeCtx) -> PathBuf {
        Self::home(ctx).join("any-converter-models.json")
    }
}

impl AgentSpec for CodexSpec {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::home(ctx);
        if config_dir.is_dir() || Self::config_path(ctx).exists() || Self::auth_path(ctx).exists() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![
            Self::config_path(ctx),
            Self::auth_path(ctx),
            Self::catalog_path(ctx),
        ]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let auth_path = Self::auth_path(ctx);
        let catalog_path = Self::catalog_path(ctx);

        let config = read_config(&config_path)?.unwrap_or_default();
        let auth = read_config(&auth_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in ["model_provider", "model", "model_catalog_json"] {
            if let Some(value) = toml::get(&config, key) {
                old.insert(format!("config.{key}"), value);
            }
        }
        if let Some(value) = json::get(&auth, "OPENAI_API_KEY") {
            old.insert("auth.OPENAI_API_KEY".to_owned(), value);
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let catalog_absolute = absolute(&catalog_path);
        let mut config_text = toml::set(
            &config,
            &[
                KV::new("model_provider", json!(PROVIDER)),
                KV::new("model", json!(wiring.default_model)),
                KV::new(
                    "model_catalog_json",
                    json!(catalog_absolute.to_string_lossy()),
                ),
            ],
        )?;
        config_text = toml::set_table(
            &config_text,
            PROVIDER_TABLE,
            &[
                KV::new("name", json!(PROVIDER)),
                KV::new("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
                KV::new("wire_api", json!("responses")),
                KV::new("requires_openai_auth", json!(true)),
            ],
        )?;

        let auth_text = json::set(&auth, &[KV::new("OPENAI_API_KEY", json!(wiring.agent_key))])?;
        let catalog_text = render_catalog(wiring)?;

        let written_config = write_config(&config_path, &config_text)?;
        let written_auth = write_config(&auth_path, &auth_text)?;
        let written_catalog = write_config(&catalog_path, &catalog_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_config, written_auth, written_catalog])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let auth_path = Self::auth_path(ctx);
        let catalog_path = Self::catalog_path(ctx);

        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(config) = read_config(&config_path)? {
            let mut text = config;
            let mut sets = Vec::new();
            let mut deletes = Vec::new();
            for key in ["model_provider", "model", "model_catalog_json"] {
                match old.get(&format!("config.{key}")) {
                    Some(value) if !value.is_empty() => {
                        sets.push(KV::new(key, json!(value)));
                    }
                    _ => deletes.push(key),
                }
            }
            text = toml::set(&text, &sets)?;
            text = toml::del(&text, &deletes)?;
            text = toml::del(&text, &[PROVIDER_TABLE])?;
            written.push(write_config(&config_path, &text)?);
        }

        if let Some(auth) = read_config(&auth_path)? {
            let text = match old.get("auth.OPENAI_API_KEY") {
                Some(value) if !value.is_empty() => {
                    json::set(&auth, &[KV::new("OPENAI_API_KEY", json!(value))])?
                }
                _ => json::del(&auth, &["OPENAI_API_KEY"])?,
            };
            written.push(write_config(&auth_path, &text)?);
        }

        if catalog_path.exists() {
            remove_config(&catalog_path)?;
            written.push(catalog_path);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx) -> WiredState {
        let config = read_config(&Self::config_path(ctx)).ok().flatten();
        let auth = read_config(&Self::auth_path(ctx)).ok().flatten();
        let base = config
            .as_deref()
            .and_then(|text| toml::get(text, "model_providers.any-converter.base_url"));
        let key = auth
            .as_deref()
            .and_then(|text| json::get(text, "OPENAI_API_KEY"));
        let gateway = base.as_deref().is_some_and(is_gateway_base);
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in ["model_provider", "model", "model_catalog_json"] {
                if let Some(value) = toml::get(&config, key) {
                    values.insert(format!("config.{key}"), value);
                }
            }
            if let Some(value) = toml::get(&config, "model_providers.any-converter.base_url") {
                values.insert(format!("config.{PROVIDER_TABLE}.base_url"), value);
            }
        }
        if let Ok(Some(auth)) = read_config(&Self::auth_path(ctx)) {
            if let Some(value) = json::get(&auth, "OPENAI_API_KEY") {
                values.insert("auth.OPENAI_API_KEY".to_owned(), value);
            }
        }
        values
    }
}

fn render_catalog(wiring: &WiringCtx<'_>) -> Result<String, WiringError> {
    let models: Vec<serde_json::Value> = wiring
        .catalog
        .models
        .iter()
        .map(|model| json!({ "slug": model.id, "display_name": model.name }))
        .collect();
    let payload = json!({ "models": models });
    let mut text = serde_json::to_string_pretty(&payload)
        .map_err(|error| WiringError::Edit(EditError::Parse(error.to_string())))?;
    text.push('\n');
    Ok(text)
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
