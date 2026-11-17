//! Cursor CLI specification (`~/.cursor/cli-config.json`).
//!
//! Cursor CLI does not expose a base-URL setting; this spec only pins the model
//! it starts with. This spec does the same: `model.modelId` (plus the display
//! fields the CLI keeps in sync) and `hasChangedDefaultModel`, restored on
//! disconnect.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, bin_in_path, join_relative,
    load_stash, read_config, record_old_values, save_stash, write_config,
};
use crate::edit::{KV, json};
use crate::error::WiringError;

/// Configuration keys owned by the Cursor CLI spec.
pub const OUR_KEYS: &[&str] = &[
    "model.modelId",
    "model.displayModelId",
    "model.displayName",
    "hasChangedDefaultModel",
];

const MODEL_BLOCK: &str = "model";
const MODEL_ID: &str = "model.modelId";
const DISPLAY_ID: &str = "model.displayModelId";
const DISPLAY_NAME: &str = "model.displayName";
const CHANGED: &str = "hasChangedDefaultModel";

/// Cursor CLI wiring implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CursorSpec;

impl CursorSpec {
    fn config_path(ctx: &HomeCtx) -> PathBuf {
        join_relative(&ctx.home, ".cursor/cli-config.json")
    }
}

impl AgentSpec for CursorSpec {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn our_keys(&self) -> &'static [&'static str] {
        OUR_KEYS
    }

    fn detect(&self, ctx: &HomeCtx) -> DetectResult {
        let config_dir = ctx.home.join(".cursor");
        if config_dir.is_dir() || bin_in_path(ctx, "cursor-agent") || bin_in_path(ctx, "cursor") {
            DetectResult::Detected { config_dir }
        } else {
            DetectResult::Absent
        }
    }

    fn config_files(&self, ctx: &HomeCtx) -> Vec<PathBuf> {
        vec![Self::config_path(ctx)]
    }

    fn connect(&self, ctx: &HomeCtx, wiring: &WiringCtx<'_>) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let text = read_config(&path)?.unwrap_or_default();

        let mut old = BTreeMap::new();
        for key in [MODEL_BLOCK, CHANGED] {
            if let Some(value) = json::get(&text, key) {
                old.insert(key.to_owned(), value);
            }
        }
        let mut stash = load_stash(ctx);
        record_old_values(&mut stash, self.id(), old);

        let next = json::set(
            &text,
            &[
                KV::new(MODEL_ID, json!(wiring.default_model)),
                KV::new(DISPLAY_ID, json!(wiring.default_model)),
                KV::new(DISPLAY_NAME, json!(wiring.default_model)),
                KV::new(CHANGED, json!(true)),
            ],
        )?;
        let written = write_config(&path, &next)?;
        save_stash(ctx, &stash)?;
        Ok(vec![written])
    }

    fn disconnect(&self, ctx: &HomeCtx) -> Result<Vec<PathBuf>, WiringError> {
        let path = Self::config_path(ctx);
        let mut stash = load_stash(ctx);
        let old = stash.old_values(self.id());
        let mut written = Vec::new();

        if let Some(current) = read_config(&path)? {
            let text = match old.get(MODEL_BLOCK) {
                Some(value) if !value.is_empty() => {
                    let value = serde_json::from_str(value)
                        .unwrap_or_else(|_| Value::String(value.clone()));
                    json::set(&current, &[KV::new(MODEL_BLOCK, value)])?
                }
                _ => json::del(&current, &[MODEL_BLOCK])?,
            };
            let text = match old.get(CHANGED) {
                Some(value) if !value.is_empty() => {
                    json::set(&text, &[KV::new(CHANGED, json!(value))])?
                }
                _ => json::del(&text, &[CHANGED])?,
            };
            written.push(write_config(&path, &text)?);
        }

        stash.take_namespace(self.id());
        save_stash(ctx, &stash)?;
        Ok(written)
    }

    fn read_state(&self, ctx: &HomeCtx, _gateway_url: &str) -> WiredState {
        let Ok(Some(text)) = read_config(&Self::config_path(ctx)) else {
            return WiredState::Disconnected;
        };
        let model = json::get(&text, MODEL_ID).filter(|value| !value.is_empty());
        let changed = json::get(&text, CHANGED).as_deref() == Some("true");
        // The flag is the only marker that the pinned model came from us: a
        // model block without it is the user's own configuration, which we
        // report as disconnected rather than drifted.
        if !changed {
            return WiredState::Disconnected;
        }
        if model.is_some() {
            WiredState::Connected
        } else {
            WiredState::Drifted
        }
    }

    fn applied_values(&self, ctx: &HomeCtx) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Ok(Some(text)) = read_config(&Self::config_path(ctx)) {
            for key in [MODEL_ID, CHANGED] {
                if let Some(value) = json::get(&text, key) {
                    values.insert(key.to_owned(), value);
                }
            }
        }
        values
    }
}
