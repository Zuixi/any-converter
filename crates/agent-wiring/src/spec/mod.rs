//! Agent specifications and the shared wiring contract.
//!
//! Every supported agent is one self-contained file in this module. The trait
//! deliberately exposes only files and key/value transformations: it never
//! receives a data directory, so the orchestration layer owns snapshots,
//! stashes, and drift audits.

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod pi;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic;
use crate::backup::Stash;
use crate::error::WiringError;
use crate::sync::catalog::ModelCatalog;

/// The agent home plus an injectable environment lookup.
///
/// `home` is the user's home directory in tests and production. `env` is a
/// function pointer rather than a captured closure so specs and tests can pass
/// plain functions; tests commonly inject a thread-local map.
#[derive(Debug, Clone)]
pub struct HomeCtx {
    /// User home directory used to resolve default configuration locations.
    pub home: PathBuf,
    /// Environment lookup, injectable so tests never mutate the process.
    pub env: fn(&str) -> Option<String>,
}

/// Values the gateway supplies while connecting an agent.
#[derive(Debug, Clone, Copy)]
pub struct WiringCtx<'a> {
    /// Gateway base URL, for example `http://127.0.0.1:8787`.
    pub gateway_url: &'a str,
    /// Per-agent gateway key written into the agent configuration.
    pub agent_key: &'a str,
    /// Catalog of models exposed by the gateway.
    pub catalog: &'a ModelCatalog,
    /// Model selected for this agent.
    pub default_model: &'a str,
}

/// Result of checking whether an agent is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectResult {
    /// No configuration location exists.
    Absent,
    /// The agent is installed; its configuration directory is known.
    Detected {
        /// Existing (or expected) configuration directory.
        config_dir: PathBuf,
    },
}

/// High-level wiring state read cheaply from the agent's configuration files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiredState {
    /// Every gateway field currently points at the gateway.
    Connected,
    /// Some gateway fields are present but others were changed or removed.
    Drifted,
    /// No gateway fields are present.
    Disconnected,
}

/// One agent's complete wiring knowledge.
pub trait AgentSpec {
    /// Stable agent identifier (`claude-code`, `codex`, `gemini-cli`, `pi`).
    fn id(&self) -> &'static str;

    /// Every configuration key this spec owns.
    fn our_keys(&self) -> &'static [&'static str];

    /// Detects whether the agent is installed.
    fn detect(&self, ctx: &HomeCtx) -> DetectResult;

    /// Files that must be snapshotted and restored for this agent.
    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf>;

    /// Connects the agent, returning the files written.
    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError>;

    /// Semantically extracts this spec's keys, restoring stashed values.
    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError>;

    /// Reads the drift state from the agent configuration against the actual
    /// gateway address (magpie `Agent.Drift`'s wiring check): Connected when
    /// every wiring key points at THIS gateway, Disconnected when none do,
    /// Drifted when only some do. Asking "is this URL the gateway" instead
    /// of "does it look like a gateway" is what keeps third-party proxies
    /// from reading as our wiring.
    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState;

    /// Values the spec just wrote, used for the drift audit.
    ///
    /// The default implementation is empty so third-party specs remain valid.
    fn applied_values(&self, _ctx: &HomeCtx) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

/// Returns every supported agent specification.
pub fn all_specs() -> Vec<Box<dyn AgentSpec>> {
    vec![
        Box::new(claude::ClaudeSpec),
        Box::new(codex::CodexSpec),
        Box::new(gemini::GeminiSpec),
        Box::new(pi::PiSpec),
    ]
}

thread_local! {
    static STASH_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Temporarily redirects spec stash access to the wiring data directory.
///
/// Spec methods receive no data directory by contract, so the orchestrator
/// scopes the stash location around each call. Direct spec use (and tests)
/// falls back to a hidden `<home>/.any-converter/agent_stash.json`.
pub(crate) struct StashScope {
    previous: Option<PathBuf>,
}

impl StashScope {
    pub(crate) fn enter(path: PathBuf) -> Self {
        let previous = STASH_PATH.with(|slot| slot.replace(Some(path)));
        Self { previous }
    }
}

impl Drop for StashScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        STASH_PATH.with(|slot| {
            slot.replace(previous);
        });
    }
}

pub(crate) fn stash_path(ctx: &HomeCtx) -> PathBuf {
    STASH_PATH
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(|| ctx.home.join(".any-converter").join("agent_stash.json"))
}

pub(crate) fn load_stash(ctx: &HomeCtx) -> Stash {
    Stash::load(&stash_path(ctx))
}

pub(crate) fn save_stash(ctx: &HomeCtx, stash: &Stash) -> Result<(), WiringError> {
    stash.save(&stash_path(ctx))?;
    Ok(())
}

/// Records old values only when a key has no stash entry yet.
///
/// Reconnecting must never replace the pristine pre-connect value with one of
/// our own values.
pub(crate) fn record_old_values(stash: &mut Stash, agent: &str, values: BTreeMap<String, String>) {
    let existing = stash.old_values(agent);
    let fresh: BTreeMap<String, String> = values
        .into_iter()
        .filter(|(key, value)| !value.is_empty() && !existing.contains_key(key))
        .collect();
    if !fresh.is_empty() {
        stash.record(agent, fresh);
    }
}

/// Reports whether `value` starts with the gateway URL's host and port.
///
/// Schemes, paths, and trailing slashes are ignored; only the authority is
/// compared. This matches how agents normalize their base URLs.
pub fn gw_base_match(value: &str, gateway_url: &str) -> bool {
    let Some(gateway) = authority(gateway_url) else {
        return false;
    };
    let Some(candidate) = authority(value) else {
        return false;
    };
    gateway.eq_ignore_ascii_case(candidate)
}

fn authority(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    (!authority.is_empty()).then_some(authority)
}

/// Joins a gateway URL and a path suffix without duplicating slashes.
pub(crate) fn gateway_path(gateway_url: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        gateway_url.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

/// Expands a leading `~` using `ctx.home`.
pub(crate) fn expand_tilde(ctx: &HomeCtx, value: &str) -> PathBuf {
    if value == "~" {
        return ctx.home.clone();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return ctx.home.join(rest);
    }
    PathBuf::from(value)
}

/// Resolves an environment-selected directory, falling back to `default`.
pub(crate) fn env_dir(
    ctx: &HomeCtx,
    variable: &str,
    default: impl FnOnce(&HomeCtx) -> PathBuf,
) -> PathBuf {
    match (ctx.env)(variable) {
        Some(value) if !value.trim().is_empty() => expand_tilde(ctx, value.trim()),
        _ => default(ctx),
    }
}

/// Reads a configuration file as UTF-8, returning `None` when it is absent.
pub(crate) fn read_config(path: &Path) -> Result<Option<String>, WiringError> {
    let Some(bytes) = atomic::read(path)? else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        WiringError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("configuration is not utf-8: {error}"),
        ))
    })?;
    Ok(Some(text))
}

/// Atomically writes a configuration file and returns its path.
pub(crate) fn write_config(path: &Path, text: &str) -> Result<PathBuf, WiringError> {
    atomic::write_atomic(path, text.as_bytes())?;
    Ok(path.to_path_buf())
}

/// Removes a file, treating an already-absent file as success.
pub(crate) fn remove_config(path: &Path) -> Result<(), WiringError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WiringError::Io(error)),
    }
}
