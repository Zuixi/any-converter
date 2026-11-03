//! Query inputs and aggregate results for the usage statistics views.

use serde::{Deserialize, Serialize};

/// Width of the time buckets a usage series is split into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Granularity {
    Hour,
    Day,
    Week,
    Month,
}

impl Granularity {
    /// The SQL bucket expression applied to `column`, an ISO-8601 text column.
    pub(super) fn bucket_expr(self, column: &str) -> String {
        match self {
            Self::Hour => format!("substr({column},1,13) || ':00:00Z'"),
            Self::Day => format!("substr({column},1,10)"),
            Self::Week => format!("date({column},'weekday 1','-7 days')"),
            Self::Month => format!("substr({column},1,7)"),
        }
    }

    /// Truncates an ISO-8601 timestamp to the hour bucket that contains it. The
    /// rollup's finest grain is one hour, so a range filter must round a sub-hour
    /// bound down instead of dropping the partial first hour.
    pub(super) fn hour_floor(self, ts: &str) -> String {
        // `get` declines a slice that is not a char boundary, so a mangled
        // timestamp passes through instead of panicking.
        match ts.get(..13) {
            Some(prefix) => format!("{prefix}:00:00Z"),
            None => ts.to_owned(),
        }
    }
}

/// A dimension a series point can be broken down by, so a single line becomes a
/// stack: not just "600k on Saturday" but "600k, 400k of it from one provider".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDim {
    Provider,
    /// Matches `client_id`.
    Agent,
    /// Matches `client_model`.
    Model,
    /// Matches `provider_account`, which `rollup_hourly` does not carry.
    Account,
}

impl SplitDim {
    /// The `rollup_hourly` column this dimension lives in, or `None` when the
    /// rollup has no such column and the split cannot be pre-aggregated.
    pub(super) fn rollup_column(self) -> Option<&'static str> {
        match self {
            Self::Provider => Some("provider"),
            Self::Agent => Some("agent"),
            Self::Model => Some("client_model"),
            // An account is a high-cardinality detail-only label.
            Self::Account => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageQuery {
    /// Inclusive, UTC ISO-8601.
    pub from: String,
    /// Exclusive, UTC ISO-8601.
    pub to: String,
    pub granularity: Granularity,
    pub provider: Option<String>,
    /// Matches `client_model`.
    pub model: Option<String>,
    /// Matches `client_id`.
    pub agent: Option<String>,
    /// Matches `provider_account`.
    pub account: Option<String>,
    /// Break every series point down by this dimension. `None` leaves
    /// [`Bucket::by`] empty and skips the extra query entirely.
    pub split_by: Option<SplitDim>,
}

/// The filters a ledger page shares: one range, zero or more equalities, and a
/// free-text search. Every field is optional but the range, so the default filter
/// is "everything in this window".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LedgerFilter {
    /// Inclusive, UTC ISO-8601.
    pub from: String,
    /// Exclusive, UTC ISO-8601.
    pub to: String,
    /// `YYYY-MM-DD`, restricts the rows to one local day.
    pub day: Option<String>,
    pub provider: Option<String>,
    /// Matches `client_model`.
    pub model: Option<String>,
    /// Matches `client_id`.
    pub agent: Option<String>,
    /// Matches `provider_account`.
    pub account: Option<String>,
    /// Only `response_status >= 400`.
    #[serde(default)]
    pub failed: bool,
    /// Only calls answered after an earlier try failed (`attempt > 0`).
    #[serde(default)]
    pub retried: bool,
    /// Case-insensitive substring over model/provider/session.
    pub query: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub calls: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_write_1h: u64,
    /// Prompt tokens of the calls whose vendor counted no cache at all; they are
    /// left out of `hit_rate`'s denominator so "no data" never reads as "a miss".
    pub cache_unknown_tokens: u64,
    pub reasoning_tokens: u64,
    pub latency_sum_ms: u64,
    pub latency_max_ms: u64,
    pub ttft_sum_ms: u64,
    pub ttft_calls: u64,
    /// What the priced calls cost, in USD. It covers only `calls - unpriced` of this
    /// total, so a bare sum is never read as the cost of all the traffic.
    pub cost_usd: f64,
    /// Calls that reported tokens but whose model has no price, so `cost_usd`
    /// covers only the rest. magpie calls this `Unpriced`.
    pub unpriced: u64,
    /// Requests answered after a try failed first: a success with `attempt > 0`.
    /// magpie's `Retried`.
    pub retried: u64,
    /// Of `errors`, those the gateway ended for looping.
    pub loops: u64,
    /// Content time of the streamed replies whose speed was measured, and the
    /// tokens written in that time. `decode_n` counts those replies, so the two
    /// sums are only ever read against it.
    pub decode_ms: u64,
    pub decode_out: u64,
    pub decode_n: u64,
}

impl Totals {
    /// Share of the prompt read from the cache, of the calls whose vendor counted it.
    /// `None` when nothing is left after removing the vendors that count no cache.
    pub fn hit_rate(&self) -> Option<f64> {
        let prompt = self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
            - self.cache_unknown_tokens;
        (prompt > 0).then(|| self.cache_read_tokens as f64 / prompt as f64)
    }

    pub fn mean_latency_ms(&self) -> Option<u64> {
        (self.calls > 0).then(|| self.latency_sum_ms / self.calls)
    }

    pub fn mean_ttft_ms(&self) -> Option<u64> {
        (self.ttft_calls > 0).then(|| self.ttft_sum_ms / self.ttft_calls)
    }

    /// Tokens written per second over the replies whose speed was measured.
    ///
    /// `None` when nothing was measured: a reply that was not streamed has no
    /// content window, and a window of zero milliseconds has no speed rather
    /// than an infinite one.
    pub fn speed(&self) -> Option<f64> {
        if self.decode_n == 0 || self.decode_ms == 0 || self.decode_out == 0 {
            return None;
        }
        Some(self.decode_out as f64 / (self.decode_ms as f64 / 1000.0))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Bucket {
    pub label: String,
    pub timestamp: String,
    pub totals: Totals,
    /// The point told apart by `split_by`: value → that value's totals at this
    /// point. The 24 with the most tokens over the whole period are kept; the
    /// rest of the point is what remains of `totals` (magpie's `seriesKeep`).
    /// A value with no calls in this bucket has no entry, which a chart reads
    /// as zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<std::collections::BTreeMap<String, Totals>>,
}

/// How many split values a series point keeps. A stacked chart cannot tell
/// hundreds of lines apart, so the tail folds back into the point's own totals.
pub const SERIES_KEEP: usize = 24;

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub from: String,
    pub to: String,
    pub granularity: Granularity,
    pub totals: Totals,
    pub series: Vec<Bucket>,
    pub by_provider: Vec<Group>,
    pub by_model: Vec<Group>,
    pub by_agent: Vec<Group>,
    /// Calls grouped by the subscription account that answered them, read
    /// from `request_logs` (not the rollup: an account is a high-cardinality
    /// label the hourly buckets do not need).
    pub by_account: Vec<Group>,
    /// Calls grouped by the client's session id, read from `request_logs`.
    /// Empty when no call named a session.
    pub by_session: Vec<Group>,
}

/// Width of the calendar the heatmap draws: 52 whole weeks behind this one,
/// plus the current week's partial column.
pub const HEATMAP_WEEKS: u32 = 53;

/// One day of the heatmap. Only days with calls are listed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeatDay {
    pub date: String, // YYYY-MM-DD, UTC
    pub calls: u64,
    /// Input + output + cache traffic, as the chart counts it.
    pub tokens: u64,
    pub cost_usd: f64,
    /// Calls inside this day with no price, so `cost_usd` covers the rest.
    pub unpriced: u64,
}

/// The last `HeatmapWeeks` weeks, a day each, oldest first.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Heatmap {
    pub from: String, // YYYY-MM-DD of the first Monday shown
    pub to: String,   // YYYY-MM-DD of today
    pub weeks: u32,   // 53
    pub days: Vec<HeatDay>,
}
