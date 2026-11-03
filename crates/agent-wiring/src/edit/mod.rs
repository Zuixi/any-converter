//! Format-preserving editors.
//!
//! Every editor in this module is a pure bytes-to-bytes function: it never
//! reads or writes the filesystem. On [`EditError`] the caller's input is
//! untouched (it was never written anywhere); the caller decides whether to
//! persist anything.

pub mod env;
pub mod json;
pub mod toml;

use std::fmt;

use serde::{Deserialize, Serialize};

/// The UTF-8 byte order mark.
pub(crate) const BOM: &str = "\u{feff}";

/// Splits an optional leading UTF-8 BOM from the document body.
pub(crate) fn split_bom(src: &str) -> (&'static str, &str) {
    match src.strip_prefix(BOM) {
        Some(body) => (BOM, body),
        None => ("", src),
    }
}

/// A single key/value assignment.
///
/// `path` is a dot-separated path for JSON and TOML editors. Dotenv editors
/// treat it as a literal variable name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KV {
    /// Dot-separated key path (or literal variable name for dotenv).
    pub path: String,
    /// Value to write.
    pub value: serde_json::Value,
}

impl KV {
    /// Creates a key/value assignment.
    pub fn new(path: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self {
            path: path.into(),
            value: value.into(),
        }
    }
}

/// Errors produced by format editors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The input (or the requested path) could not be parsed.
    Parse(String),
    /// The requested value was not found.
    NotFound(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "parse error: {message}"),
            Self::NotFound(key) => write!(f, "not found: {key}"),
        }
    }
}

impl std::error::Error for EditError {}
