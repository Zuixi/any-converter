//! The Pi-format family: OmO, Prime Agent, and mu.
//!
//! These agents copied Pi's configuration layout wholesale: a `models.json`
//! holding `providers.<id>` blocks and a `settings.json` holding
//! `defaultProvider` / `defaultModel` / `defaultThinkingLevel`. One
//! parameterized spec covers all of them; only the configuration directory
//! and the environment override differ.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Where a family member keeps its configuration.
#[derive(Debug, Clone, Copy)]
pub struct PiFamilyLayout {
    /// The agent id (`omo`, `prime-agent`, `mu`).
    pub id: &'static str,
    /// Environment overrides, tried in order; the first absolute hit wins.
    /// A relative value is ignored because the agent resolves it against a
    /// working directory this process cannot know.
    pub env_vars: &'static [&'static str],
    /// The directory under the home when no environment override applies.
    pub default_dir: &'static [&'static str],
    /// An older directory name used only when the primary home directory is
    /// absent (mu's `~/.kyrn`).
    pub legacy_dir: &'static [&'static str],
}

/// Reasoning levels the Pi format forwards to thinking-capable models.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// One Pi-format agent's wiring.
#[derive(Debug, Clone, Copy)]
pub struct PiFamilySpec {
    /// Directories and ids the agent differs from Pi in.
    pub layout: PiFamilyLayout,
}

impl PiFamilySpec {
    fn dir(&self, ctx: &HomeCtx) -> PathBuf {
        for var in self.layout.env_vars {
            if let Some(value) = (ctx.env)(var) {
                let value = value.trim();
                if !value.is_empty() {
                    let path = super::expand_tilde(ctx, value);
                    // A relative value is the agent's working directory's,
                    // which the gateway cannot know (magpie omo.go:31-40,
                    // primeagent.go:37-44, mu.go:44-54); try the next one.
                    if path.is_absolute() {
                        return path;
                    }
                }
            }
        }
        let primary = dir_under(&ctx.home, self.layout.default_dir);
        if !self.layout.legacy_dir.is_empty()
            && !home_dir(&ctx.home, self.layout.default_dir).is_dir()
        {
            let legacy = dir_under(&ctx.home, self.layout.legacy_dir);
            if home_dir(&ctx.home, self.layout.legacy_dir).is_dir() {
                return legacy;
            }
        }
        primary
    }

    fn settings_path(&self, ctx: &HomeCtx) -> PathBuf {
        self.dir(ctx).join("settings.json")
    }

    fn models_path(&self, ctx: &HomeCtx) -> PathBuf {
        self.dir(ctx).join("models.json")
    }

    fn provider_path(&self) -> String {
        format!("providers.{}", super::support::PROVIDER)
    }
}

impl AgentSpec for PiFamilySpec {
    fn id(&self) -> &'static str {
        self.layout.id
    }

    fn our_keys(&self) -> &'static [&'static str] {
        &[
            "models.providers.any-converter",
            "settings.defaultProvider",
            "settings.defaultModel",
            "settings.defaultThinkingLevel",
            "settings.enabledModels",
        ]
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = self.dir(ctx);
        if config_dir.is_dir() || self.settings_path(ctx).exists() || self.models_path(ctx).exists()
        {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![self.settings_path(ctx), self.models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = self.settings_path(ctx);
        let models_path = self.models_path(ctx);
        let settings = read_config(&settings_path)?.unwrap_or_default();
        let models = read_config(&models_path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &["defaultProvider", "defaultModel"]),
        );

        let provider = json!({
            "name": super::support::PROVIDER,
            "baseUrl": gateway_path(wiring.gateway_url, "v1"),
            "api": "openai-completions",
            "apiKey": wiring.agent_key,
            // Forks copied Pi's session-affinity switch along with the rest
            // of the layout (magpie agents.go:690-691).
            "compat": { "sendSessionAffinityHeaders": true },
            "models": provider_models(wiring),
        });
        let models_text = json::set(&models, &[KV::new(self.provider_path(), provider)])?;

        let mut kvs = vec![
            KV::new("defaultProvider", json!(super::support::PROVIDER)),
            KV::new("defaultModel", json!(wiring.default_model)),
        ];
        if let Some(effort) = wiring.effort.filter(|effort| !effort.is_empty()) {
            kvs.push(KV::new("defaultThinkingLevel", json!(effort)));
        }
        let mut settings_text = json::set(&settings, &kvs)?;
        // The forks keep Pi's enabledModels allow-list, and a default outside
        // it never starts (magpie pi.go:42-71).
        settings_text = super::pi::add_enabled_model(
            &settings_text,
            super::support::PROVIDER,
            wiring.default_model,
        )?;

        let written_models = write_config(&models_path, &models_text)?;
        let written_settings = write_config(&settings_path, &settings_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_models, written_settings])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let settings_path = self.settings_path(ctx);
        let models_path = self.models_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&models_path)? {
            let provider = self.provider_path();
            let text = json::del(&current, &[provider.as_str()])?;
            written.push(write_config(&models_path, &text)?);
        }

        if let Some(current) = read_config(&settings_path)? {
            let mut text = super::support::restore_keys(
                &current,
                &old,
                &["defaultProvider", "defaultModel", "defaultThinkingLevel"],
            )?;
            text = super::pi::strip_enabled_models(&text, super::support::PROVIDER)?;
            written.push(write_config(&settings_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(models)) = read_config(&self.models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let provider = self.provider_path();
        let base = json::get(&models, &format!("{provider}.baseUrl"));
        let key = json::get(&models, &format!("{provider}.apiKey"));
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
        if let Ok(Some(settings)) = read_config(&self.settings_path(ctx)) {
            for key in ["defaultProvider", "defaultModel", "defaultThinkingLevel"] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(format!("settings.{key}"), value);
                }
            }
        }
        let provider = self.provider_path();
        if let Ok(Some(models)) = read_config(&self.models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = json::get(&models, &format!("{provider}.{key}")) {
                    values.insert(format!("{provider}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// The provider's model list: an ARRAY of entries with an `id` key, matching
/// the Pi format the forks copied (magpie `agents.go:675-691`).
///
/// Catalog approximation: [`CatalogModel`](crate::sync::catalog::CatalogModel)
/// carries no image/effort/output data, so each entry has only id, name,
/// reasoning, and the window as `contextWindow`. Missing `maxTokens` and
/// image input fall back to Pi's conservative defaults.
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
                entry.insert("contextWindow".to_owned(), json!(context));
            }
            Value::Object(entry)
        })
        .collect()
}

/// Folds a layout's components under the home one at a time.
fn dir_under(home: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// The home-level root of a layout (`~/.mu` for `[".mu", "agent"]`).
fn home_dir(home: &Path, parts: &[&str]) -> PathBuf {
    parts
        .first()
        .map_or_else(|| home.to_path_buf(), |first| home.join(first))
}

/// OmO keeps its agent dir under `~/.omo/agent`.
pub fn omo() -> PiFamilySpec {
    PiFamilySpec {
        layout: PiFamilyLayout {
            id: "omo",
            env_vars: &["OMO_CODING_AGENT_DIR", "SENPI_CODING_AGENT_DIR"],
            default_dir: &[".omo", "agent"],
            legacy_dir: &[],
        },
    }
}

/// Prime Agent keeps its agent dir under `~/.prime/agent`.
pub fn prime_agent() -> PiFamilySpec {
    PiFamilySpec {
        layout: PiFamilyLayout {
            id: "prime-agent",
            env_vars: &["PRIME_AGENT_CODING_AGENT_DIR"],
            default_dir: &[".prime", "agent"],
            legacy_dir: &[],
        },
    }
}

/// mu keeps its agent dir under `~/.mu/agent`, or `~/.kyrn/agent` while the
/// old name exists and `~/.mu` does not (magpie `mu.go:44-54`).
pub fn mu() -> PiFamilySpec {
    PiFamilySpec {
        layout: PiFamilyLayout {
            id: "mu",
            env_vars: &[
                "MU_AGENT_DIR",
                "KYRN_AGENT_DIR",
                "MU_CODING_AGENT_DIR",
                "KYRN_CODING_AGENT_DIR",
            ],
            default_dir: &[".mu", "agent"],
            legacy_dir: &[".kyrn", "agent"],
        },
    }
}
