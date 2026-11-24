//! Codex specification (`~/.codex/config.toml`, `auth.json`, catalog JSON).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, remove_config, save_stash, write_config,
};
use crate::edit::{EditError, KV, json, toml};
use crate::error::WiringError;

/// Configuration keys owned by the Codex spec.
pub const OUR_KEYS: &[&str] = &[
    "config.model_provider",
    "config.model",
    "config.model_catalog_json",
    "config.model_reasoning_effort",
    "config.model_providers.any-converter",
];

/// Reasoning levels Codex accepts, in its own order.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

const REASONING_EFFORT: &str = "model_reasoning_effort";

/// Codex fails the whole catalog on an entry with neither `base_instructions`
/// nor `model_messages.instructions_template`, so every entry carries a
/// prompt. Codex's own bundled prompts are per-model; a third-party model gets
/// this one, which names no tools or wire details that could disagree with the
/// gateway.
const BASE_INSTRUCTIONS: &str = "You are a coding agent working in the user's \
repository. Follow the user's instructions, inspect the repository before \
changing it, and keep changes focused. Use the available tools to read and \
edit files and to run commands. Report what you changed and any command \
output that matters.";

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
        for key in [
            "model_provider",
            "model",
            "model_catalog_json",
            REASONING_EFFORT,
        ] {
            if let Some(value) = toml::get(&config, key) {
                old.insert(format!("config.{key}"), value);
            }
        }
        // Codex's own API key is the user's: it is stashed for a restore but
        // never rewritten. Our key lives in the provider table instead
        // (`experimental_bearer_token`), which is what a provider of its own
        // is wired with.
        if let Some(value) = json::get(&auth, "OPENAI_API_KEY") {
            old.insert("auth.OPENAI_API_KEY".to_owned(), value);
        }

        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let mut config_text = toml::set(
            &config,
            &[
                KV::new("model_provider", json!(PROVIDER)),
                KV::new("model", json!(wiring.default_model)),
                KV::new("model_catalog_json", json!(catalog_ref(&catalog_path))),
            ],
        )?;
        config_text = apply_effort(&config_text, wiring.effort)?;
        config_text = toml::set_table(
            &config_text,
            PROVIDER_TABLE,
            &[
                KV::new("name", json!(PROVIDER)),
                KV::new("base_url", json!(gateway_path(wiring.gateway_url, "v1"))),
                KV::new("wire_api", json!("responses")),
                KV::new("experimental_bearer_token", json!(wiring.agent_key)),
                KV::new(
                    "http_headers",
                    json!({ "x-openai-actor-authorization": PROVIDER }),
                ),
            ],
        )?;
        // Magpie writes no `requires_openai_auth` on a Codex with no ChatGPT
        // sign-in; a true left by an older wiring would make Codex expect one.
        config_text = toml::del(
            &config_text,
            &[&format!("{PROVIDER_TABLE}.requires_openai_auth")],
        )?;
        let catalog_text = render_catalog(wiring)?;

        let written_config = write_config(&config_path, &config_text)?;
        let written_catalog = write_config(&catalog_path, &catalog_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_config, written_catalog])
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
            for key in [
                "model_provider",
                "model",
                "model_catalog_json",
                REASONING_EFFORT,
            ] {
                match old.get(&format!("config.{key}")) {
                    Some(value) if !value.is_empty() => {
                        sets.push(KV::new(key, json!(value)));
                    }
                    _ => deletes.push(key),
                }
            }
            text = toml::set(&text, &sets)?;
            text = toml::del(&text, &deletes)?;
            // The provider table stays: a thread started on it can't be
            // opened again without it ("Model provider `any-converter` not
            // found"), and Codex refuses the whole config when a
            // `model_provider` names a missing table. Only our secret and
            // header go.
            text = toml::del(
                &text,
                &[
                    &format!("{PROVIDER_TABLE}.experimental_bearer_token"),
                    &format!("{PROVIDER_TABLE}.http_headers.x-openai-actor-authorization"),
                ],
            )?;
            written.push(write_config(&config_path, &text)?);
        }

        // The user's own OPENAI_API_KEY is never rewritten on connect; put a
        // stashed one back only if something else replaced it, and leave
        // auth.json untouched when there was none.
        if let Some(value) = old
            .get("auth.OPENAI_API_KEY")
            .filter(|value| !value.is_empty())
        {
            if let Some(auth) = read_config(&auth_path)? {
                if json::get(&auth, "OPENAI_API_KEY").as_deref() != Some(value.as_str()) {
                    let text = json::set(&auth, &[KV::new("OPENAI_API_KEY", json!(value))])?;
                    written.push(write_config(&auth_path, &text)?);
                }
            }
        }

        if catalog_path.exists() {
            remove_config(&catalog_path)?;
            written.push(catalog_path);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let config = read_config(&Self::config_path(ctx)).ok().flatten();
        // The provider table outlives a disconnect, so `base_url` alone would
        // read as drift forever: `model_provider` says whether Codex is on it.
        let provider = config
            .as_deref()
            .and_then(|text| toml::get(text, "model_provider"));
        let base = config
            .as_deref()
            .and_then(|text| toml::get(text, "model_providers.any-converter.base_url"));
        let key = config.as_deref().and_then(|text| {
            toml::get(
                text,
                "model_providers.any-converter.experimental_bearer_token",
            )
        });
        let gateway = provider.as_deref() == Some(PROVIDER)
            && base
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
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in [
                "model_provider",
                "model",
                "model_catalog_json",
                REASONING_EFFORT,
            ] {
                if let Some(value) = toml::get(&config, key) {
                    values.insert(format!("config.{key}"), value);
                }
            }
            for key in ["base_url", "experimental_bearer_token"] {
                if let Some(value) = toml::get(&config, &format!("{PROVIDER_TABLE}.{key}")) {
                    values.insert(format!("config.{PROVIDER_TABLE}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Writes `model_reasoning_effort` the way Codex stores it.
///
/// `None` leaves the file untouched; `Some("")` removes our setting so the
/// model default applies.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(toml::del(text, &[REASONING_EFFORT])?),
        Some(effort) => Ok(toml::set(
            text,
            &[KV::new(REASONING_EFFORT, json!(effort))],
        )?),
    }
}

fn render_catalog(wiring: &WiringCtx<'_>) -> Result<String, WiringError> {
    let models: Vec<serde_json::Value> = wiring
        .catalog
        .models
        .iter()
        .enumerate()
        .map(|(index, model)| catalog_entry(model, index + 1))
        .collect();
    let payload = json!({ "models": models });
    let mut text = serde_json::to_string_pretty(&payload)
        .map_err(|error| WiringError::Edit(EditError::Parse(error.to_string())))?;
    text.push('\n');
    Ok(text)
}

/// One `models.json` entry in the shape Codex's own catalog uses.
///
/// Codex deserializes every entry strictly: a missing required field fails the
/// whole catalog, not just that model. The field set below mirrors
/// `codexcat.model` (magpie's renderer for the same file): the fields Codex
/// needs or that change behaviour, with Codex's defaults for the rest.
fn catalog_entry(model: &crate::sync::catalog::CatalogModel, priority: usize) -> serde_json::Value {
    let name = if model.name.is_empty() {
        model.id.as_str()
    } else {
        model.name.as_str()
    };
    // Per-model effort lists are not part of the catalog we receive, so a
    // model that reasons is offered the levels Codex itself understands and a
    // model that does not is offered none. `default_reasoning_level` is
    // written even when null: the field has no `omitempty` in Codex's shape.
    let levels: Vec<serde_json::Value> = if model.reasoning {
        EFFORTS
            .iter()
            .map(|effort| json!({ "effort": effort, "description": effort_label(effort) }))
            .collect()
    } else {
        Vec::new()
    };
    let mut entry = serde_json::Map::new();
    entry.insert("slug".to_owned(), json!(model.id));
    entry.insert("display_name".to_owned(), json!(name));
    entry.insert(
        "description".to_owned(),
        json!(format!("{name} via any-converter")),
    );
    entry.insert("base_instructions".to_owned(), json!(BASE_INSTRUCTIONS));
    entry.insert(
        "default_reasoning_level".to_owned(),
        if model.reasoning {
            json!("medium")
        } else {
            serde_json::Value::Null
        },
    );
    entry.insert("supported_reasoning_levels".to_owned(), json!(levels));
    entry.insert("shell_type".to_owned(), json!("unified_exec"));
    entry.insert("visibility".to_owned(), json!("list"));
    entry.insert("supported_in_api".to_owned(), json!(true));
    entry.insert("priority".to_owned(), json!(priority));
    entry.insert("support_verbosity".to_owned(), json!(false));
    entry.insert("default_verbosity".to_owned(), serde_json::Value::Null);
    entry.insert("apply_patch_tool_type".to_owned(), json!("freeform"));
    entry.insert(
        "truncation_policy".to_owned(),
        json!({ "mode": "tokens", "limit": 10000 }),
    );
    entry.insert("experimental_supported_tools".to_owned(), json!([]));
    entry.insert("input_modalities".to_owned(), json!(["text"]));
    if let Some(context) = model.context_tokens {
        entry.insert("context_window".to_owned(), json!(context));
    }
    entry.insert("service_tiers".to_owned(), json!([]));
    entry.insert("supports_search_tool".to_owned(), json!(true));
    entry.insert("supports_parallel_tool_calls".to_owned(), json!(true));
    entry.insert(
        "supports_reasoning_summaries".to_owned(),
        json!(model.reasoning),
    );
    serde_json::Value::Object(entry)
}

/// The display name of a reasoning effort, as Codex's pickers show it.
fn effort_label(effort: &str) -> &str {
    match effort {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        other => other,
    }
}

/// How `config.toml` names the catalog.
///
/// On Windows the name is relative to `CODEX_HOME`: a `C:\...` path means
/// nothing to the same Codex reading Windows' config from WSL, which resolves
/// a relative path against its home directory.
fn catalog_ref(path: &Path) -> String {
    if cfg!(windows) {
        if let Some(name) = path.file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    absolute(path).to_string_lossy().into_owned()
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
