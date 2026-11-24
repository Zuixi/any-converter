//! Claude Code specification (`~/.claude/settings.json`, JSONC).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, env_dir, gw_base_match, load_stash,
    read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Claude Code spec.
pub const OUR_KEYS: &[&str] = &[
    "env.ANTHROPIC_BASE_URL",
    "env.ANTHROPIC_AUTH_TOKEN",
    // Not written any more; removed on connect and restored on disconnect.
    // `env.ANTHROPIC_MODEL` outranks the top-level `model`, so a `/model` pick
    // would never persist.
    "env.ANTHROPIC_MODEL",
    "model",
    "env.ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
    "env.ANTHROPIC_DEFAULT_OPUS_MODEL",
    "env.ANTHROPIC_API_KEY",
    "env.CLAUDE_CODE_EFFORT_LEVEL",
    "effortLevel",
];

/// Reasoning levels Claude Code accepts, in its own order.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The effort env variable, which only `max` needs.
const EFFORT_ENV: &str = "env.CLAUDE_CODE_EFFORT_LEVEL";

/// Per-role model slots Claude Code understands.
const ROLES: &[&str] = &["opus", "sonnet", "haiku"];

/// The tier env variable for a role.
fn role_key(role: &str) -> &'static str {
    match role {
        "opus" => "env.ANTHROPIC_DEFAULT_OPUS_MODEL",
        "sonnet" => "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
        _ => "env.ANTHROPIC_DEFAULT_HAIKU_MODEL",
    }
}

/// Claude Code wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeSpec;

impl ClaudeSpec {
    /// Claude Code's configuration directory.
    ///
    /// With `CLAUDE_CONFIG_DIR` set, Claude Code keeps everything there and
    /// never creates `~/.claude`; the settings file it reads is the one under
    /// that directory.
    fn config_dir(ctx: &HomeCtx) -> PathBuf {
        env_dir(ctx, "CLAUDE_CONFIG_DIR", |home| home.home.join(".claude"))
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::config_dir(ctx).join("settings.json")
    }
}

impl AgentSpec for ClaudeSpec {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::config_dir(ctx);
        let settings = Self::settings_path(ctx);
        if settings.exists() || config_dir.is_dir() {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::settings_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let current = read_config(&path)?.unwrap_or_default();

        if json::get(&current, "apiKeyHelper").is_some() {
            return Err(WiringError::Refused(
                "apiKeyHelper is configured; remove it before connecting".to_owned(),
            ));
        }

        let model_name = claude_name(wiring.default_model);
        let model_effort_key = model_name
            .as_ref()
            .map(|name| format!("modelSettings.{name}.effortLevel"));

        let mut stash = load_stash(ctx);
        let mut old = OUR_KEYS
            .iter()
            .filter_map(|key| json::get(&current, key).map(|value| ((*key).to_owned(), value)))
            .collect::<BTreeMap<_, _>>();
        if let Some(key) = &model_effort_key {
            if let Some(value) = json::get(&current, key) {
                old.insert(key.clone(), value);
            }
        }
        record_old_values(&mut stash, self.id(), old);

        // The model goes in settings.json's top-level `model`, which is the
        // value `/model` reads and writes. `env.ANTHROPIC_MODEL` outranks it,
        // so it is removed on connect (and restored on disconnect).
        let mut kvs = vec![
            KV::new("env.ANTHROPIC_BASE_URL", json!(wiring.gateway_url)),
            KV::new("env.ANTHROPIC_AUTH_TOKEN", json!(wiring.agent_key)),
            KV::new("model", json!(wiring.default_model)),
        ];

        // A role the user picked wins; otherwise keep the catalog's tier
        // heuristic, so existing wiring behaves as before.
        let (sonnet, haiku, opus) = wiring.catalog.tier_models().unwrap_or((None, None, None));
        let tier = |role: &str| -> Option<String> {
            wiring.roles.get(role).cloned().or_else(|| {
                match role {
                    "opus" => opus,
                    "sonnet" => sonnet,
                    _ => haiku,
                }
                .map(|model| model.id.clone())
            })
        };
        for role in ROLES {
            if let Some(model) = tier(role) {
                kvs.push(KV::new(role_key(role), json!(model)));
            }
        }

        let mut text = json::set(&current, &kvs)?;
        let stale_roles: Vec<&str> = ROLES
            .iter()
            .filter_map(|role| {
                let key = role_key(role);
                json::get(&text, key)
                    .filter(|model| wiring.catalog.find(model).is_none())
                    .map(|_| key)
            })
            .collect();
        text = json::del(&text, &stale_roles)?;
        text = json::del(&text, &["env.ANTHROPIC_MODEL", "env.ANTHROPIC_API_KEY"])?;
        text = apply_effort(&text, wiring.effort, model_effort_key.as_deref())?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let Some(current) = read_config(&path)? else {
            let mut stash = load_stash(ctx);
            stash.take_namespace(self.id());
            save_stash(ctx, &stash)?;
            return Ok(Vec::new());
        };

        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        // Stashed keys include the dynamic `modelSettings.<model>` effort
        // key; restore every one of them, then drop any remaining owned key.
        let mut keys: Vec<String> = OUR_KEYS.iter().map(|key| (*key).to_owned()).collect();
        // The model-specific effort key only entered the stash when a value
        // predated the connect. When it did not, restore must still delete
        // the one our connect wrote, derived from the model we wired.
        if let Some(name) = json::get(&current, "model")
            .as_deref()
            .and_then(claude_name)
        {
            let key = format!("modelSettings.{name}.effortLevel");
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for key in old.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        for key in keys {
            match old.get(&key) {
                Some(value) if !value.is_empty() => sets.push(KV::new(key, json!(value))),
                _ => deletes.push(key),
            }
        }

        let mut text = json::set(&current, &sets)?;
        let delete_refs: Vec<&str> = deletes.iter().map(String::as_str).collect();
        text = json::del_pruning(&text, &delete_refs)?;
        let written = write_config(&path, &text)?;
        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&current, "env.ANTHROPIC_BASE_URL");
        let token = json::get(&current, "env.ANTHROPIC_AUTH_TOKEN");
        let gateway = base
            .as_deref()
            .is_some_and(|value| gw_base_match(value, gateway_url));
        let key = token.as_deref().is_some_and(|value| !value.is_empty());
        match (gateway, key) {
            (true, true) => WiredState::Connected,
            (false, false) => WiredState::Disconnected,
            _ => WiredState::Drifted,
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let Ok(Some(current)) = read_config(&Self::settings_path(ctx)) else {
            return BTreeMap::new();
        };
        OUR_KEYS
            .iter()
            .filter_map(|key| json::get(&current, key).map(|value| ((*key).to_owned(), value)))
            .collect()
    }

    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }

    fn roles(&self) -> &'static [&'static str] {
        ROLES
    }
}

/// Writes the selected reasoning effort the way Claude Code stores it.
///
/// `Some("")` means the model's own default: remove our effort settings.
/// `None` leaves the file untouched, so reconnecting without opening the
/// effort control never changes the user's choice.
fn apply_effort(
    text: &str,
    effort: Option<&str>,
    model_effort_key: Option<&str>,
) -> Result<String, WiringError> {
    let Some(effort) = effort else {
        return Ok(text.to_owned());
    };
    let mut keys: Vec<&str> = vec!["effortLevel", EFFORT_ENV];
    if let Some(key) = model_effort_key {
        keys.push(key);
    }
    if effort.is_empty() {
        return Ok(json::del(text, &keys)?);
    }
    if effort == "max" {
        // `max` outlives a session only in the env variable.
        let mut keys = vec!["effortLevel"];
        if let Some(key) = model_effort_key {
            keys.push(key);
        }
        let text = json::del(text, &keys)?;
        return Ok(json::set(&text, &[KV::new(EFFORT_ENV, json!("max"))])?);
    }
    let text = json::del(text, &[EFFORT_ENV])?;
    let mut kvs = vec![KV::new("effortLevel", json!(effort))];
    if let Some(key) = model_effort_key {
        kvs.push(KV::new(key.to_owned(), json!(effort)));
    }
    Ok(json::set(&text, &kvs)?)
}

/// The Claude model name an id names (`claude-opus-4-6`), when it is one.
///
/// Gateway ids are `<provider>/<model>`; Claude Code keys `modelSettings` by
/// the bare model name without a date suffix. A version written with a dot
/// (`claude-opus-4.6`) is the name Claude Code is given with dashes, so dots
/// become dashes before parsing.
pub(crate) fn claude_name(model: &str) -> Option<String> {
    let lower = model.to_ascii_lowercase();
    let start = lower.find("claude-")?;
    let rest = lower[start + "claude-".len()..].replace('.', "-");
    let mut parts = rest.split(['-', '/', ':']);
    let tier = parts.next()?;
    if !matches!(tier, "opus" | "sonnet" | "haiku" | "fable") {
        return None;
    }
    let major = parts.next()?;
    if !major.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let mut name = format!("claude-{tier}-{major}");
    if let Some(minor) = parts.next() {
        if !minor.is_empty() && minor.chars().all(|ch| ch.is_ascii_digit()) && minor.len() <= 2 {
            name.push('-');
            name.push_str(minor);
        }
    }
    Some(name)
}
