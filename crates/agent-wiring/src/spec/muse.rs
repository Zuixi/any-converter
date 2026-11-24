//! Muse specification (`<xdg config>/muse/settings.json`).
//!
//! Muse stores a schema version, an endpoint transport, and the selected model
//! in one settings file. The transport pins the gateway base URL with keyless
//! auth. Muse refuses to read a file without a schema version, so connect adds
//! one only when it is absent; an existing (possibly newer) version is never
//! downgraded.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, gateway_path, gw_base_match,
    load_stash, read_config, record_old_values, save_stash, write_config, xdg_config_home,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Muse spec.
pub const OUR_KEYS: &[&str] = &["schema_version", "endpoint_transport", "model"];

const SETTINGS_FILE: &str = "settings.json";
const SCHEMA_VERSION: &str = "schema_version";
const TRANSPORT: &str = "endpoint_transport";
const MODEL: &str = "model";

/// Muse wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct MuseSpec;

impl MuseSpec {
    fn dir(ctx: &HomeCtx) -> PathBuf {
        xdg_config_home(ctx).join("muse")
    }

    fn settings_path(ctx: &HomeCtx) -> PathBuf {
        Self::dir(ctx).join(SETTINGS_FILE)
    }
}

impl AgentSpec for MuseSpec {
    fn id(&self) -> &'static str {
        "muse"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = Self::dir(ctx);
        if config_dir.is_dir() || Self::settings_path(ctx).exists() {
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
        let settings = read_config(&path)?.unwrap_or_default();

        let mut stash = load_stash(ctx);
        record_old_values(
            &mut stash,
            self.id(),
            super::support::record_keys(&settings, &[MODEL, TRANSPORT, SCHEMA_VERSION]),
        );

        let mut kvs = vec![
            KV::new(
                TRANSPORT,
                json!({
                    "base_url": gateway_path(wiring.gateway_url, "v1"),
                    "auth": "none",
                }),
            ),
            KV::new(MODEL, json!(wiring.default_model)),
        ];
        // Muse only reads files that declare a schema version, but a newer
        // version written by the app must survive.
        if json::get(&settings, SCHEMA_VERSION).is_none() {
            kvs.push(KV::new(SCHEMA_VERSION, json!(1)));
        }
        let text = json::set(&settings, &kvs)?;
        let written = write_config(&path, &text)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::settings_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let mut text = current;
            for key in [MODEL, TRANSPORT] {
                text = restore_key(&text, key, &old)?;
            }
            // Return a stashed user version, but never delete one that connect
            // added: Muse cannot read the file without it.
            if let Some(raw) = old.get(SCHEMA_VERSION).filter(|raw| !raw.is_empty()) {
                text = json::set(&text, &[KV::new(SCHEMA_VERSION, parse_value(raw))])?;
            }
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, gateway_url: &str) -> WiredState {
        let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let base = json::get(&settings, "endpoint_transport.base_url");
        let model = json::get(&settings, MODEL);

        if base.is_none() && model.is_none() {
            return WiredState::Disconnected;
        }

        let gateway = base
            .as_deref()
            .is_some_and(|value| !value.is_empty() && gw_base_match(value, gateway_url));
        let selected = model.as_deref().is_some_and(|value| !value.is_empty());
        if gateway && selected {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(settings)) = read_config(&Self::settings_path(ctx)) {
            for key in [SCHEMA_VERSION, MODEL] {
                if let Some(value) = json::get(&settings, key) {
                    values.insert(key.to_owned(), value);
                }
            }
            if let Some(value) = json::get(&settings, "endpoint_transport.base_url") {
                values.insert("endpoint_transport.base_url".to_owned(), value);
            }
        }
        values
    }
}

/// Restores a stashed value, or deletes the key when it was absent.
fn restore_key(
    text: &str,
    key: &str,
    old: &BTreeMap<String, String>,
) -> Result<String, WiringError> {
    match old.get(key) {
        Some(raw) if !raw.is_empty() => Ok(json::set(text, &[KV::new(key, parse_value(raw))])?),
        _ => Ok(json::del_pruning(text, &[key])?),
    }
}

/// Parses a stashed compact JSON value, keeping plain strings intact.
fn parse_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}
