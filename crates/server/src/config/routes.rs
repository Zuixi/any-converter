//! Model routes, the routing strategy, and the metadata a model carries.

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};

/// One member of a route's provider pool.
///
/// Members are the first-class unit of a modern route: each names a provider
/// and (optionally) its own upstream model, weight, and fallback status, so
/// one route can send the same client model to different upstream names per
/// provider. A route without `members` keeps the legacy `providers` +
/// `upstream_model` shape, and the two are folded into members on read.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RouteMemberConfig {
    pub provider: String,
    /// Upstream model sent to this provider. `None` falls back to the route's
    /// `upstream_model`, then to the provider's own `model_map`.
    #[serde(default)]
    pub model: Option<String>,
    /// Share of requests while `strategy = "weighted"`; 0 is treated as 1.
    #[serde(default)]
    pub weight: Option<u32>,
    /// A switched-off member stays in the route but takes no requests.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Fallbacks are only tried after every primary member has failed.
    #[serde(default)]
    pub fallback: Option<bool>,
}

impl RouteMemberConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn weight(&self) -> u32 {
        self.weight.unwrap_or(1).max(1)
    }

    pub fn is_fallback(&self) -> bool {
        self.fallback.unwrap_or(false)
    }
}

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
    /// Preferred member shape. Empty keeps the legacy `providers` fields.
    #[serde(default)]
    pub members: Vec<RouteMemberConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    #[default]
    Priority,
    RoundRobin,
    /// Rotate the starting member by weight, then keep failover order.
    Weighted,
}

impl RouteStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteStrategy::Priority => "priority",
            RouteStrategy::RoundRobin => "round_robin",
            RouteStrategy::Weighted => "weighted",
        }
    }
}

/// Legacy format-based route. Not used for routing any more; kept so old configs still parse.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
}

/// One concrete failover target: which provider, which upstream model, how
/// much weight it carries, and whether it is a fallback-only member.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMember {
    pub provider: String,
    pub upstream_model: String,
    pub weight: u32,
    pub fallback: bool,
}

/// Result of model-based provider resolution.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    /// Primary members first, fallback members after, each in config order.
    pub members: Vec<ResolvedMember>,
    /// The model of the first member; the default used for logging and
    /// count-token calls when no per-member model applies.
    pub upstream_model: String,
    pub strategy: RouteStrategy,
}

impl ResolvedRoute {
    pub fn provider_names(&self) -> Vec<String> {
        self.members.iter().map(|m| m.provider.clone()).collect()
    }
}

impl ModelRouteConfig {
    /// Collect all provider names, preferring `members` and merging the
    /// legacy `provider` / `providers` fields otherwise.
    pub fn provider_names(&self) -> Vec<String> {
        if !self.members.is_empty() {
            let mut names = Vec::new();
            for member in &self.members {
                if !names.contains(&member.provider) {
                    names.push(member.provider.clone());
                }
            }
            return names;
        }
        let mut names = Vec::new();
        if let Some(ref p) = self.provider {
            names.push(p.clone());
        }
        names.extend(self.providers.iter().cloned());
        names
    }

    /// Normalize both route shapes into members, retaining disabled entries
    /// for editors and admin views.
    pub fn configured_members(&self) -> Vec<RouteMemberConfig> {
        let legacy_model = || self.upstream_model.clone();
        if self.members.is_empty() {
            self.provider_names()
                .into_iter()
                .map(|provider| RouteMemberConfig {
                    provider,
                    model: legacy_model(),
                    weight: None,
                    enabled: None,
                    fallback: None,
                })
                .collect()
        } else {
            self.members
                .iter()
                .map(|member| RouteMemberConfig {
                    provider: member.provider.clone(),
                    model: member.model.clone().or_else(legacy_model),
                    weight: member.weight,
                    enabled: member.enabled,
                    fallback: member.fallback,
                })
                .collect()
        }
    }

    /// Enabled members ordered with primaries first, preserving each group's
    /// configured order. A legacy route yields one member per provider,
    /// sharing the route's upstream override.
    pub fn enabled_members(&self) -> Vec<RouteMemberConfig> {
        let mut primaries = Vec::new();
        let mut fallbacks = Vec::new();
        for member in self
            .configured_members()
            .into_iter()
            .filter(|m| m.is_enabled())
        {
            if member.is_fallback() {
                fallbacks.push(member);
            } else {
                primaries.push(member);
            }
        }
        primaries.extend(fallbacks);
        primaries
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
