//! Model routes, the routing strategy, and the metadata a model carries.

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct ModelRouteConfig {
    pub pattern: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub strategy: RouteStrategy,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    #[default]
    Priority,
    RoundRobin,
}

/// Legacy format-based route. Not used for routing any more; kept so old configs still parse.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
}

/// Result of model-based provider resolution.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub provider_names: Vec<String>,
    pub upstream_model: String,
    pub strategy: RouteStrategy,
}

impl ModelRouteConfig {
    /// Collect all provider names (merging `provider` and `providers` fields).
    pub fn provider_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(ref p) = self.provider {
            names.push(p.clone());
        }
        names.extend(self.providers.iter().cloned());
        names
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelMetadata {
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub max_context_window: Option<i64>,
    #[serde(default)]
    pub supports_parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

// ── Logging ──────────────────────────────────────────────────────────
