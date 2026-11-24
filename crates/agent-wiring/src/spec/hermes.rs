//! Hermes Agent specification (YAML config under `HERMES_HOME`).
//!
//! Hermes reads providers from the `providers` mapping and starts on
//! `model.provider` + `model.default`. Each bot is a profile with its own
//! `config.yaml` under `<root>/profiles/<name>`; the root config and every
//! profile are wired so a model picked here reaches whichever bot starts.
//!
//! The entry written here is OpenAI chat completions at the gateway's `/v1`
//! with the agent key, listing the catalog, plus a `User-Agent` header that
//! identifies Hermes in the gateway's usage view.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, expand_tilde, gateway_path,
    gw_base_match, load_stash, local_app_data_dir, read_config, record_old_values, save_stash,
    write_config,
};
use crate::edit::{KV, yaml};
use crate::error::WiringError;

/// Configuration keys owned by the Hermes spec.
pub const OUR_KEYS: &[&str] = &[
    "providers.any-converter",
    "model.provider",
    "model.default",
    "model.model",
    "agent.reasoning_effort",
];

/// Reasoning levels Hermes accepts.
const EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

const REASONING_EFFORT: &str = "agent.reasoning_effort";

/// Provider id written under `providers`.
pub const PROVIDER_ID: &str = "any-converter";
const PROVIDER_PATH: &str = "providers.any-converter";
const MODEL_PROVIDER: &str = "model.provider";
const MODEL_DEFAULT: &str = "model.default";
const MODEL_MODEL: &str = "model.model";

/// Hermes Agent wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct HermesSpec;

impl HermesSpec {
    /// Hermes's default folder: `%LOCALAPPDATA%\hermes` on Windows,
    /// `~/.hermes` elsewhere.
    fn default_root(ctx: &HomeCtx) -> PathBuf {
        #[cfg(windows)]
        {
            local_app_data_dir(ctx).join("hermes")
        }
        #[cfg(not(windows))]
        {
            ctx.home.join(".hermes")
        }
    }

    /// The profile root, honoring a `HERMES_HOME` outside the default folder.
    fn root(ctx: &HomeCtx) -> PathBuf {
        let default = Self::default_root(ctx);
        let Some(home) = (ctx.env)("HERMES_HOME") else {
            return default;
        };
        let home = expand_tilde(ctx, home.trim());
        if !home.is_absolute() || home.starts_with(&default) {
            return default;
        }
        // `<root>/profiles/<name>` names a profile of that root.
        if home
            .parent()
            .and_then(|path| path.file_name())
            .is_some_and(|name| name == "profiles")
        {
            return home
                .parent()
                .and_then(|path| path.parent())
                .unwrap_or(&default)
                .to_path_buf();
        }
        home
    }

    /// Every profile's `config.yaml`, root first.
    fn configs(ctx: &HomeCtx) -> Vec<PathBuf> {
        let root = Self::root(ctx);
        let mut paths = vec![root.join("config.yaml")];
        if let Ok(entries) = std::fs::read_dir(root.join("profiles")) {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name();
                if name == "default" {
                    continue;
                }
                let config = entry.path().join("config.yaml");
                if config.is_file() {
                    paths.push(config);
                }
            }
        }
        paths
    }

    /// The config `hermes` itself starts in: the active profile.
    fn active(ctx: &HomeCtx) -> PathBuf {
        let root = Self::root(ctx);
        if let Some(home) = (ctx.env)("HERMES_HOME") {
            let home = expand_tilde(ctx, home.trim());
            if home.join("config.yaml").exists()
                && home
                    .parent()
                    .and_then(|path| path.file_name())
                    .is_some_and(|name| name == "profiles")
            {
                return home;
            }
        }
        if let Ok(name) = std::fs::read_to_string(root.join("active_profile")) {
            let name = name.trim().trim_start_matches('\u{feff}');
            let dir = root.join("profiles").join(name);
            if dir.is_dir() {
                return dir;
            }
        }
        root
    }
}

impl AgentSpec for HermesSpec {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::root(ctx);
        let installed = config_dir.join("config.yaml").is_file()
            || config_dir.join("profiles").is_dir()
            || (ctx.env)("HERMES_HOME").is_some_and(|value| !value.trim().is_empty());
        if installed {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        Self::configs(ctx)
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let mut old = BTreeMap::new();
        let mut stash = load_stash(ctx);
        let mut written = Vec::new();
        let active = Self::active(ctx);

        // Unlike magpie, this spec writes no `any-converter-anthropic`
        // companion entry: routing individual models through the Anthropic
        // messages endpoint needs per-model API information the catalog does
        // not carry here, so every model goes through chat completions.
        let mut provider = serde_json::Map::new();
        provider.insert("name".to_owned(), json!(PROVIDER_ID));
        provider.insert(
            "base_url".to_owned(),
            json!(gateway_path(wiring.gateway_url, "v1")),
        );
        provider.insert("api_key".to_owned(), json!(wiring.agent_key));
        provider.insert("api_mode".to_owned(), json!("chat_completions"));
        provider.insert(
            "extra_headers".to_owned(),
            json!({ "User-Agent": "hermes-agent" }),
        );
        provider.insert(
            "models".to_owned(),
            json!(
                wiring
                    .catalog
                    .models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>()
            ),
        );

        for path in Self::configs(ctx) {
            let text = read_config(&path)?.unwrap_or_default();
            for key in [MODEL_PROVIDER, MODEL_DEFAULT, MODEL_MODEL, REASONING_EFFORT] {
                if path.parent() == Some(active.as_path()) {
                    if let Some(value) = yaml::get(&text, key) {
                        old.insert(key.to_owned(), value);
                    }
                }
            }

            // Write the merged entry one key at a time. Replacing the whole
            // mapping would leave an empty `providers:` parent behind when
            // ours was its only entry, and YAML then has two `providers`
            // keys.
            let merged = merged_provider(&text, &provider);
            let mut provider_kvs = Vec::new();
            if let Value::Object(entries) = merged {
                for (key, value) in entries {
                    provider_kvs.push(KV::new(
                        format!("{PROVIDER_PATH}.{}", yaml_key(&key)),
                        value,
                    ));
                }
            }
            let mut next = yaml::set(&text, &provider_kvs)?;
            if path.parent() == Some(active.as_path()) {
                next = yaml::set(
                    &next,
                    &[
                        KV::new(MODEL_PROVIDER, json!(PROVIDER_ID)),
                        KV::new(MODEL_DEFAULT, json!(wiring.default_model)),
                    ],
                )?;
                // `model.model` is Hermes' fallback when `model.default` is
                // unset; only disconnect (unwire) removes it, never connect.
                next = apply_effort(&next, wiring.effort)?;
            }
            written.push(write_config(&path, &next)?);
        }

        record_old_values(&mut stash, self.id(), old);
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let old = load_stash(ctx).old_values(self.id());
        let mut stash = load_stash(ctx);
        let mut written = Vec::new();
        let active = Self::active(ctx);

        for path in Self::configs(ctx) {
            let Some(current) = read_config(&path)? else {
                continue;
            };
            let mut text = yaml::del(&current, &[PROVIDER_PATH])?;
            text = prune_empty(&text, "providers")?;
            if path.parent() == Some(active.as_path()) {
                text = restore(&text, MODEL_PROVIDER, old.get(MODEL_PROVIDER))?;
                text = restore(&text, MODEL_DEFAULT, old.get(MODEL_DEFAULT))?;
                text = restore(&text, MODEL_MODEL, old.get(MODEL_MODEL))?;
                text = restore(&text, REASONING_EFFORT, old.get(REASONING_EFFORT))?;
                text = prune_empty(&text, "model")?;
                text = prune_empty(&text, "agent")?;
            }
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let mut found = false;
        let mut connected = false;
        for path in Self::configs(ctx) {
            let Ok(Some(text)) = read_config(&path) else {
                continue;
            };
            let Some(base) = yaml::get(&text, &format!("{PROVIDER_PATH}.base_url")) else {
                continue;
            };
            found = true;
            let provider = yaml::get(&text, MODEL_PROVIDER);
            let default = yaml::get(&text, MODEL_DEFAULT).or_else(|| yaml::get(&text, MODEL_MODEL));
            if gw_base_match(&base, gateway_url)
                && provider.as_deref() == Some(PROVIDER_ID)
                && default.is_some_and(|value| !value.is_empty())
            {
                connected = true;
            }
        }
        if connected {
            WiredState::Connected
        } else if found {
            WiredState::Drifted
        } else {
            WiredState::Disconnected
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        for path in Self::configs(ctx) {
            if let Ok(Some(text)) = read_config(&path) {
                for key in [PROVIDER_PATH, MODEL_PROVIDER, MODEL_DEFAULT] {
                    if let Some(value) = yaml::get(&text, key) {
                        values.insert(key.to_owned(), value);
                    }
                }
            }
        }
        values
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
}

/// Escapes a key that itself contains a dot or a backslash for a YAML path.
fn yaml_key(key: &str) -> String {
    key.replace('\\', "\\\\").replace('.', "\\.")
}

/// Lays our provider entry over the one already in the file.
///
/// Hermes and its dashboard write keys into the entry itself
/// (`default_model`, `key_env`, `request_timeout_seconds`, `extra_body`,
/// `context_length`, `enabled`, `ssl_*`, a custom `name`); only the address,
/// key, API mode, and model list are ours. A user's extra headers survive
/// beside our `User-Agent`.
fn merged_provider(text: &str, ours: &Map<String, Value>) -> Value {
    let mut provider = yaml::get(text, PROVIDER_PATH)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    if !provider
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.trim().is_empty())
    {
        if let Some(name) = ours.get("name") {
            provider.insert("name".to_owned(), name.clone());
        }
    }
    for key in ["base_url", "api_key", "api_mode", "models"] {
        if let Some(value) = ours.get(key) {
            provider.insert(key.to_owned(), value.clone());
        }
    }
    let mut headers = provider
        .get("extra_headers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(ours) = ours.get("extra_headers").and_then(Value::as_object) {
        for (key, value) in ours {
            headers.insert(key.clone(), value.clone());
        }
    }
    provider.insert("extra_headers".to_owned(), Value::Object(headers));
    Value::Object(provider)
}

/// Writes `agent.reasoning_effort`; `None` leaves the file untouched and
/// `Some("")` removes our setting so Hermes's default applies.
fn apply_effort(text: &str, effort: Option<&str>) -> Result<String, WiringError> {
    match effort {
        None => Ok(text.to_owned()),
        Some("") => Ok(yaml::del(text, &[REASONING_EFFORT])?),
        Some(effort) => Ok(yaml::set(
            text,
            &[KV::new(REASONING_EFFORT, json!(effort))],
        )?),
    }
}

fn restore(text: &str, path: &str, old: Option<&String>) -> Result<String, WiringError> {
    Ok(match old {
        Some(value) if !value.is_empty() => yaml::set(text, &[KV::new(path, json!(value))])?,
        _ => yaml::del(text, &[path])?,
    })
}

fn prune_empty(text: &str, path: &str) -> Result<String, WiringError> {
    match yaml::get(text, path) {
        // Removing the last entry leaves `key:` with a null value, which reads
        // back as `null` (or `{}` for a genuinely empty mapping).
        Some(value) if matches!(value.trim(), "{}" | "null" | "~" | "") => {
            Ok(yaml::del(text, &[path])?)
        }
        _ => Ok(text.to_owned()),
    }
}
