//! omp specification (`PI_CODING_AGENT_DIR` or `~/.omp/agent`).
//!
//! omp splits its configuration across two YAML files: `config.yml` holds the
//! role-to-model mapping (`modelRoles.default`) and the thinking level, while
//! `models.yml` holds provider definitions. Connect writes an
//! OpenAI-completions provider that identifies itself with a `User-Agent`
//! header; disconnect restores the role mapping and removes the provider once
//! no role references it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::support;
use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gw_base_match,
    join_relative, load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, yaml};
use crate::error::WiringError;

/// Configuration keys owned by the omp spec.
pub const OUR_KEYS: &[&str] = &[
    "modelRoles.default",
    "defaultThinkingLevel",
    "enabledModels",
    "providers.any-converter",
];

/// Reasoning levels omp accepts in its picker.
///
/// `max` is offered by the settings UI but must NOT be written into a model's
/// `thinking.efforts`: omp before 16.4.0 rejects the whole `models.yml` when
/// it sees one (magpie `omp.go:618-621`), and the gateway upgrades an `xhigh`
/// to the model's `max` anyway.
const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Effort ladder written into a model's `thinking.efforts`: omp's order with
/// the version-sensitive `max` left out.
const THINKING_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// The entry added to `enabledModels`: every model the gateway writes
/// (`any-converter/<provider>/<model>`), matched by an omp Bun glob.
const SCOPE_EVERY: &str = "any-converter/**";

/// The `enabledModels` key in `config.yml`.
const ENABLED_MODELS: &str = "enabledModels";

const MODEL_ROLE: &str = "modelRoles.default";
const DEFAULT_THINKING: &str = "defaultThinkingLevel";
const PROVIDER_PATH: &str = "providers.any-converter";

/// The marker a role value carries when it selects our provider.
const PROVIDER_PREFIX: &str = "any-converter/";

/// omp wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct OmpSpec;

impl OmpSpec {
    /// omp's agent directory.
    ///
    /// `PI_CODING_AGENT_DIR` is honored when it is absolute; a relative value
    /// is ignored rather than resolved against the process directory.
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
        join_relative(&ctx.home, ".omp/agent")
    }

    /// The settings file: `config.yml`, else `config.yaml`, else `config.yml`.
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        pick(Self::dir(ctx), "config.yml", "config.yaml")
    }

    /// The provider file: `models.yml`, else `models.yaml`, else `models.yml`.
    fn models_path(ctx: &HomeCtx) -> PathBuf {
        pick(Self::dir(ctx), "models.yml", "models.yaml")
    }
}

impl AgentSpec for OmpSpec {
    fn id(&self) -> &'static str {
        "omp"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        let installed = config_dir.is_dir()
            || Self::config_path(ctx).exists()
            || Self::models_path(ctx).exists();
        if installed {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx), Self::models_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let models_path = Self::models_path(ctx);
        let config = read_config(&config_path)?.unwrap_or_default();
        let models = read_config(&models_path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [MODEL_ROLE, DEFAULT_THINKING] {
            if let Some(value) = yaml::get(&config, key) {
                old.insert(key.to_owned(), value);
            }
        }
        // Whole mappings are recorded so disconnect can distinguish an
        // explicitly empty mapping from an absent one.
        for key in ["modelRoles", "providers"] {
            let source = if key == "modelRoles" {
                &config
            } else {
                &models
            };
            if let Some(value) = yaml::get(source, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let provider = json!({
            "baseUrl": support::base_url(wiring),
            "api": "openai-completions",
            "apiKey": wiring.agent_key,
            // omp 16.x otherwise identifies itself with Bun's User-Agent
            // (magpie omp.go:710-711).
            "headers": { "User-Agent": "omp" },
            "models": provider_models(wiring),
        });
        let models_text = yaml::set(&models, &[KV::new(PROVIDER_PATH, provider)])?;

        let role = json!(format!("{PROVIDER_PREFIX}{}", wiring.default_model));
        // The YAML editor replaces a scalar leaf in place, but a leaf that is
        // a block sequence loses the space after the colon. When the role was
        // a fallback list, rebuild the whole mapping instead.
        let existing = yaml::get(&config, MODEL_ROLE);
        let is_container = existing
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .is_some_and(|value| matches!(value, Value::Array(_) | Value::Object(_)));
        let mut config_text = if is_container {
            let mut roles = parent_mapping(&config, "modelRoles");
            roles.insert("default".to_owned(), role);
            replace_mapping(
                &config,
                "modelRoles",
                roles,
                yaml::get(&config, "modelRoles").is_some(),
            )?
        } else {
            yaml::set(&config, &[KV::new(MODEL_ROLE, role)])?
        };
        config_text = apply_effort(&config_text, wiring.effort)?;
        // A new session starts on the default role only while enabledModels
        // takes it in (magpie omp_scope.go:10-24), so our models join an
        // existing list.
        config_text = add_enabled_scope(&config_text)?;

        let written_config = write_config(&config_path, &config_text)?;
        let written_models = write_config(&models_path, &models_text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written_config, written_models])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let config_path = Self::config_path(ctx);
        let models_path = Self::models_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        let config = read_config(&config_path)?;
        let models = read_config(&models_path)?;

        let config_text = match &config {
            Some(current) => {
                // Rebuild `modelRoles` as a whole: deleting its last entry
                // corrupts the following line in the YAML editor.
                let mut roles = parent_mapping(current, "modelRoles");
                roles.remove("default");
                if let Some(value) = old.get(MODEL_ROLE).filter(|value| !value.is_empty()) {
                    // A role may have been a YAML list of fallback models. The
                    // stash holds it as compact JSON, so parse it back rather
                    // than writing an escaped string (magpie omp.go:219-225,
                    // 310-314).
                    let value = serde_json::from_str::<Value>(value)
                        .unwrap_or_else(|_| Value::String(value.clone()));
                    roles.insert("default".to_owned(), value);
                }
                let mut text =
                    replace_mapping(current, "modelRoles", roles, old.contains_key("modelRoles"))?;
                text = restore_top(&text, DEFAULT_THINKING, old.get(DEFAULT_THINKING))?;
                text = strip_enabled_scope(&text)?;
                Some(text)
            }
            None => None,
        };

        let referenced = config_text.as_deref().is_some_and(references_provider)
            || models.as_deref().is_some_and(references_provider);

        if let Some(text) = config_text {
            written.push(write_config(&config_path, &text)?);
        }
        if let Some(current) = models {
            let text = if referenced {
                current
            } else {
                let mut providers = parent_mapping(&current, "providers");
                providers.remove(support::PROVIDER);
                replace_mapping(
                    &current,
                    "providers",
                    providers,
                    old.contains_key("providers"),
                )?
            };
            written.push(write_config(&models_path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(models)) = read_config(&Self::models_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = yaml::get(&models, &format!("{PROVIDER_PATH}.baseUrl"));
        let key = yaml::get(&models, &format!("{PROVIDER_PATH}.apiKey"));
        let base = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = key.as_deref().is_some_and(|value| !value.is_empty());
        match (base, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(config)) = read_config(&Self::config_path(ctx)) {
            for key in [MODEL_ROLE, DEFAULT_THINKING] {
                if let Some(value) = yaml::get(&config, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        if let Ok(Some(models)) = read_config(&Self::models_path(ctx)) {
            for key in ["baseUrl", "apiKey"] {
                if let Some(value) = yaml::get(&models, &format!("{PROVIDER_PATH}.{key}")) {
                    values.insert(format!("{PROVIDER_PATH}.{key}"), value);
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Chooses the first existing file, defaulting to the primary name.
fn pick(dir: PathBuf, primary: &str, fallback: &str) -> PathBuf {
    let primary = dir.join(primary);
    if primary.exists() {
        return primary;
    }
    let fallback = dir.join(fallback);
    if fallback.exists() {
        return fallback;
    }
    primary
}

/// The provider's model array, one entry per catalog model.
///
/// `thinking.mode` is the literal `"effort"` omp expects, never the selected
/// level (magpie `omp.go:661`): the level belongs in `defaultThinkingLevel`.
/// The `thinking` block is written only for a reasoning model.
///
/// Catalog approximation: [`CatalogModel`](crate::sync::catalog::CatalogModel)
/// carries no per-model efforts, so a reasoning model gets the full ladder
/// minus `max`, and a non-reasoning one gets none. Image input and output
/// caps fall back to omp's own defaults.
fn provider_models(wiring: &WiringCtx<'_>) -> Vec<Value> {
    support::catalog_models(wiring, |model| {
        let mut entry = Map::new();
        entry.insert("id".to_owned(), json!(model.id));
        entry.insert("name".to_owned(), json!(model.name));
        entry.insert("reasoning".to_owned(), json!(model.reasoning));
        if model.reasoning {
            entry.insert(
                "thinking".to_owned(),
                json!({ "mode": "effort", "efforts": THINKING_EFFORTS }),
            );
        }
        Value::Object(entry)
    })
}

/// Adds `any-converter/**` to an existing `enabledModels` list.
///
/// A missing or non-array list is left untouched: omp then holds sessions to
/// nothing, and the list is the user's to create (magpie `omp_scope.go`).
/// Folder-scoped mapping entries are preserved.
fn add_enabled_scope(text: &str) -> Result<String, WiringError> {
    let Some(raw) = yaml::get(text, ENABLED_MODELS) else {
        return Ok(text.to_owned());
    };
    let Ok(Value::Array(mut items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(text.to_owned());
    };
    if items.is_empty() || items.iter().any(|item| covers_every(item.as_str())) {
        return Ok(text.to_owned());
    }
    items.push(Value::String(SCOPE_EVERY.to_owned()));
    Ok(yaml::set(
        text,
        &[KV::new(ENABLED_MODELS, Value::Array(items))],
    )?)
}

/// Removes our plain entries (and `any-converter/**`), keeping the user's
/// globs and folder-scoped entries. A list left empty is deleted.
fn strip_enabled_scope(text: &str) -> Result<String, WiringError> {
    let Some(raw) = yaml::get(text, ENABLED_MODELS) else {
        return Ok(text.to_owned());
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Ok(text.to_owned());
    };
    let mut kept = Vec::new();
    for item in items {
        let ours = item.as_str().is_some_and(|value| {
            let value = value.trim();
            value == SCOPE_EVERY
                || (!value.contains(['*', '?', '[']) && value.starts_with(PROVIDER_PREFIX))
        });
        if !ours {
            kept.push(item);
        }
    }
    if kept.is_empty() {
        Ok(yaml::del(text, &[ENABLED_MODELS])?)
    } else {
        Ok(yaml::set(
            text,
            &[KV::new(ENABLED_MODELS, Value::Array(kept))],
        )?)
    }
}

/// Reports whether a plain `enabledModels` entry already takes in every model
/// the gateway writes.
fn covers_every(entry: Option<&str>) -> bool {
    let Some(entry) = entry else {
        return false;
    };
    let entry = entry.trim();
    entry == SCOPE_EVERY
        || entry.eq_ignore_ascii_case("any-converter/*")
        || entry.starts_with(SCOPE_EVERY)
}

/// Writes `defaultThinkingLevel`; `None` leaves the file untouched and
/// `Some("")` removes our setting.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(yaml::del(text, &[DEFAULT_THINKING])?),
        Some(level) => Ok(yaml::set(text, &[KV::new(DEFAULT_THINKING, json!(level))])?),
    }
}

/// Reports whether any `modelRoles` value still selects our provider.
fn references_provider(text: &str) -> bool {
    yaml::get(text, "modelRoles").is_some_and(|value| value.contains(PROVIDER_PREFIX))
}

/// Reads a mapping-valued key as a JSON object.
fn parent_mapping(text: &str, path: &str) -> Map<String, Value> {
    yaml::get(text, path)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Replaces (or removes) a top-level mapping in one safe edit.
///
/// The top-level key has no indentation to leak into the following line, so
/// this avoids the nested-entry deletion bug in the YAML editor. An explicitly
/// empty mapping is kept when it existed before connect.
fn replace_mapping(
    text: &str,
    path: &str,
    mapping: Map<String, Value>,
    existed: bool,
) -> Result<String, WiringError> {
    let text = yaml::del(text, &[path])?;
    if mapping.is_empty() {
        return if existed {
            Ok(yaml::set(&text, &[KV::new(path, json!({}))])?)
        } else {
            Ok(text)
        };
    }
    Ok(yaml::set(&text, &[KV::new(path, Value::Object(mapping))])?)
}

/// Restores a top-level scalar, deleting the key when it was absent.
fn restore_top(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    match old {
        Some(value) if !value.is_empty() => Ok(yaml::set(text, &[KV::new(path, json!(value))])?),
        _ => Ok(yaml::del(text, &[path])?),
    }
}
