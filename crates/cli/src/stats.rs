//! `any-converter stats` — the usage page of the proxy, in the terminal.
//!
//! A formatted report, not a TUI: one screen of totals, a timeline, and the
//! same per-provider / per-model rankings the Web UI shows. `--watch` just
//! clears and redraws, so keeping it open next to a running proxy needs no
//! terminal state machine.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use any_converter_server::config::ServerConfig;
use any_converter_server::period;
use any_converter_server::storage::{
    Granularity, SplitDim, SqliteStorage, Totals, UsageQuery, UsageSummary,
};
use chrono::Utc;
use clap::{Args, ValueEnum};

use crate::format::*;

/// Width of a timeline bar, in `█`.
const BAR_WIDTH: usize = 32;
/// `ESC[2J` (clear) + `ESC[H` (home).
const CLEAR: &[u8] = b"\x1b[2J\x1b[H";

#[derive(Debug, Clone, Args)]
pub struct StatsArgs {
    /// Path to a TOML configuration file to read `logging.dir` from
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Directory holding any-converter.sqlite3 (defaults to `logging.dir`)
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,
    /// Window: `today`, `7d`, `30d`, `all`, `last:24h`, or `2026-10-01..2026-10-07`
    #[arg(long, default_value = "7d", value_name = "PRESET")]
    period: String,
    /// Bucket width (default: `today`=hour, `all`=week, else day)
    #[arg(long, value_enum, value_name = "G")]
    granularity: Option<GranularityArg>,
    /// Only calls answered by this provider
    #[arg(long, value_name = "P")]
    provider: Option<String>,
    /// Only calls that requested this model
    #[arg(long, value_name = "M")]
    model: Option<String>,
    /// Only calls from this agent (`client_id`)
    #[arg(long, value_name = "A")]
    agent: Option<String>,
    /// Only calls answered by this subscription account
    #[arg(long, value_name = "C")]
    account: Option<String>,
    /// Break the timeline down by a dimension
    #[arg(long, value_enum, value_name = "DIM")]
    split: Option<SplitArg>,
    /// Timeline bar metric
    #[arg(long, value_enum, default_value_t = Metric::Tokens)]
    metric: Metric,
    /// Print the UsageSummary as JSON instead of a report
    #[arg(long)]
    json: bool,
    /// Redraw the report every SECS seconds (Ctrl-C to stop)
    #[arg(long, value_name = "SECS")]
    watch: Option<u64>,
    /// Open the live terminal screen instead of printing once
    #[arg(long)]
    tui: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GranularityArg {
    Hour,
    Day,
    Week,
    Month,
}

impl GranularityArg {
    fn to_granularity(self) -> Granularity {
        match self {
            Self::Hour => Granularity::Hour,
            Self::Day => Granularity::Day,
            Self::Week => Granularity::Week,
            Self::Month => Granularity::Month,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SplitArg {
    Provider,
    Agent,
    Model,
    Account,
}

impl SplitArg {
    fn to_dim(self) -> SplitDim {
        match self {
            Self::Provider => SplitDim::Provider,
            Self::Agent => SplitDim::Agent,
            Self::Model => SplitDim::Model,
            Self::Account => SplitDim::Account,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Metric {
    Tokens,
    Cost,
    Calls,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::Calls => "calls",
        }
    }

    /// The bar scale for a bucket. Cost is counted in micro-USD so the bar keeps
    /// its resolution on cheap periods instead of rounding everything to zero.
    fn units(self, t: &Totals) -> u64 {
        match self {
            Self::Tokens => tokens(t),
            Self::Cost => (t.cost_usd.max(0.0) * 1_000_000.0).round() as u64,
            Self::Calls => t.calls,
        }
    }

    fn format(self, t: &Totals) -> String {
        match self {
            Self::Tokens => compact(tokens(t)),
            Self::Cost => money(t.cost_usd),
            Self::Calls => compact(t.calls),
        }
    }
}

/// Run `stats` against the log database, once or in a redraw loop.
pub fn run(args: StatsArgs) -> Result<(), Box<dyn std::error::Error>> {
    let log_dir = resolve_log_dir(&args)?;

    // `--json` wins: a screen is not machine output. A redirected stdout would
    // keep the escape codes, so the live screen is only for a real terminal.
    if args.tui && !args.json && io::stdout().is_terminal() {
        return live(&args, &log_dir);
    }

    match args.watch {
        None => {
            let summary = load(&log_dir, &build_query(&args)?, args.json)?;
            emit(&body(&summary, &args)?, false)?;
        }
        Some(secs) => {
            if !args.json {
                note_if_missing(&log_dir);
            }
            let every = Duration::from_secs(secs.max(1));
            loop {
                // Re-resolved every tick so a rolling period keeps moving.
                let summary = load(&log_dir, &build_query(&args)?, true)?;
                emit(&body(&summary, &args)?, true)?;
                std::thread::sleep(every);
            }
        }
    }
    Ok(())
}

fn body(summary: &UsageSummary, args: &StatsArgs) -> Result<String, serde_json::Error> {
    if args.json {
        Ok(format!("{}\n", serde_json::to_string_pretty(summary)?))
    } else {
        Ok(render(
            summary,
            args.metric,
            args.split.map(SplitArg::to_dim),
        ))
    }
}

fn emit(body: &str, clear: bool) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    if clear {
        stdout.write_all(CLEAR)?;
    }
    stdout.write_all(body.as_bytes())?;
    stdout.flush()
}

// ── live screen ─────────────────────────────────────────────────────

/// The `--tui` branch: the switches start on whatever the flags asked for.
fn live(args: &StatsArgs, log_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_live_store(log_dir)?;
    crate::tui::run(
        crate::tui::TuiConfig {
            // The screen has five windows; a rolling or picked range has no
            // switch to land on, so it starts at the week and says so in the
            // header, which always prints the window actually queried.
            preset: crate::tui::Preset::from_period(&args.period)
                .unwrap_or(crate::tui::Preset::Days7),
            granularity: args
                .granularity
                .map(GranularityArg::to_granularity)
                .unwrap_or_else(|| default_granularity(&args.period)),
            split: args.split.map(SplitArg::to_dim),
            metric: args.metric.into(),
            filters: crate::tui::Filters {
                provider: args.provider.clone(),
                model: args.model.clone(),
                agent: args.agent.clone(),
                account: args.account.clone(),
            },
        },
        store,
    )?;
    Ok(())
}

/// The store the live screen reads. Read-only while the proxy is writing, and
/// an empty database when there is none yet: a freshly pointed `--log-dir`
/// should open on zeros, not on an error.
fn open_live_store(log_dir: &Path) -> Result<SqliteStorage, Box<dyn std::error::Error>> {
    if log_dir.join("any-converter.sqlite3").exists() {
        return Ok(SqliteStorage::open_readonly_in_log_dir(log_dir)?);
    }
    Ok(SqliteStorage::open_in_log_dir(log_dir)?)
}

// ── input ────────────────────────────────────────────────────────────

/// `--log-dir`, else the `logging.dir` of the `--config` file. There is no
/// implicit default: a report of zeros from the wrong directory is worse than
/// naming the setting that is missing.
fn resolve_log_dir(args: &StatsArgs) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = &args.log_dir {
        return Ok(dir.clone());
    }
    if let Some(path) = &args.config {
        let content = std::fs::read_to_string(path)?;
        if let Some(dir) = ServerConfig::from_toml(&content)?.logging.dir {
            return Ok(PathBuf::from(dir));
        }
    }
    missing_log_dir()
}

fn missing_log_dir() -> ! {
    no_log_dir_message();
    std::process::exit(2)
}

#[allow(clippy::print_stderr)]
fn no_log_dir_message() {
    eprintln!(
        "error: no log directory: pass --log-dir or set logging.dir in the config given by --config"
    );
}

fn build_query(args: &StatsArgs) -> Result<UsageQuery, Box<dyn std::error::Error>> {
    let range = period::resolve(&args.period, Utc::now())?;
    Ok(UsageQuery {
        from: range.from,
        to: range.to,
        granularity: args
            .granularity
            .map(GranularityArg::to_granularity)
            .unwrap_or_else(|| default_granularity(&args.period)),
        provider: args.provider.clone(),
        model: args.model.clone(),
        agent: args.agent.clone(),
        account: args.account.clone(),
        split_by: args.split.map(SplitArg::to_dim),
    })
}

/// The Web UI's own mapping: a single day is read by hour, `all` by week.
fn default_granularity(period: &str) -> Granularity {
    match period {
        "today" => Granularity::Hour,
        "all" => Granularity::Week,
        _ => Granularity::Day,
    }
}

/// A database that does not exist yet, or has no rollup table, reads as an
/// empty period: a proxy that has served nothing has spent nothing.
fn load(
    log_dir: &Path,
    query: &UsageQuery,
    quiet: bool,
) -> Result<UsageSummary, Box<dyn std::error::Error>> {
    if !log_dir.join("any-converter.sqlite3").exists() {
        if !quiet {
            note_missing_store(log_dir);
        }
        return Ok(empty_summary(query));
    }
    match SqliteStorage::open_readonly_in_log_dir(log_dir).and_then(|s| s.usage_summary(query)) {
        Ok(summary) => Ok(summary),
        Err(err) if err.is_missing_table() => Ok(empty_summary(query)),
        Err(err) => Err(err.into()),
    }
}

fn note_if_missing(log_dir: &Path) {
    if !log_dir.join("any-converter.sqlite3").exists() {
        note_missing_store(log_dir);
    }
}

#[allow(clippy::print_stderr)]
fn note_missing_store(log_dir: &Path) {
    eprintln!(
        "stats: no usage database in {}; showing an empty period",
        log_dir.display()
    );
}

/// An empty `UsageSummary` for the given window, so a database that does not
/// exist yet reads as a period with no calls.
pub(crate) fn empty_summary(query: &UsageQuery) -> UsageSummary {
    UsageSummary {
        from: query.from.clone(),
        to: query.to.clone(),
        granularity: query.granularity,
        totals: Totals::default(),
        series: Vec::new(),
        by_provider: Vec::new(),
        by_model: Vec::new(),
        by_agent: Vec::new(),
        by_account: Vec::new(),
        by_session: Vec::new(),
    }
}

// ── report ───────────────────────────────────────────────────────────

fn render(summary: &UsageSummary, metric: Metric, split: Option<SplitDim>) -> String {
    let totals = &summary.totals;
    let mut out = String::new();

    push_line(
        &mut out,
        format_args!(
            "Usage — {} → {}  ({})",
            summary.from,
            summary.to,
            granularity_label(summary.granularity)
        ),
    );

    out.push('\n');
    out.push_str("Totals\n");
    row(
        &mut out,
        "calls",
        format!(
            "{}  ·  errors {} ({})  ·  retried {} ({})",
            compact(totals.calls),
            compact(totals.errors),
            pct(totals.errors, totals.calls),
            compact(totals.retried),
            pct(totals.retried, totals.calls)
        ),
    );
    row(
        &mut out,
        "tokens",
        format!(
            "{} in / {} out",
            compact(totals.input_tokens),
            compact(totals.output_tokens)
        ),
    );
    row(
        &mut out,
        "cache",
        format!(
            "{} hit  ·  read {}  ·  write {}",
            hit_rate(totals),
            compact(totals.cache_read_tokens),
            compact(totals.cache_write_tokens)
        ),
    );
    let mut cost = money(totals.cost_usd);
    if totals.unpriced > 0 {
        let _ = write!(cost, "  ·  {} unpriced", compact(totals.unpriced));
    }
    row(&mut out, "cost", cost);
    row(
        &mut out,
        "latency",
        match totals.mean_latency_ms() {
            Some(mean) => format!("mean {} / max {}", ms(mean), ms(totals.latency_max_ms)),
            None => NONE.to_string(),
        },
    );
    row(
        &mut out,
        "ttft",
        match totals.mean_ttft_ms() {
            Some(mean) => format!("mean {}", ms(mean)),
            None => NONE.to_string(),
        },
    );
    row(
        &mut out,
        "speed",
        match totals.speed() {
            Some(speed) => format!("{speed:.1} tok/s"),
            None => NONE.to_string(),
        },
    );

    out.push('\n');
    push_line(&mut out, format_args!("Timeline ({})", metric.label()));
    timeline(&mut out, summary, metric);

    if let Some(dim) = split {
        out.push('\n');
        table(
            &mut out,
            &format!("By {}", split_label(dim)),
            &split_rows(summary),
            totals,
        );
    }

    out.push('\n');
    table(
        &mut out,
        "By provider",
        &summary
            .by_provider
            .iter()
            .map(|group| (group.id.clone(), group.totals.clone()))
            .collect::<Vec<_>>(),
        totals,
    );

    out.push('\n');
    table(
        &mut out,
        "By model",
        &summary
            .by_model
            .iter()
            .map(|group| (group.id.clone(), group.totals.clone()))
            .collect::<Vec<_>>(),
        totals,
    );

    out
}

fn timeline(out: &mut String, summary: &UsageSummary, metric: Metric) {
    if summary.series.is_empty() {
        out.push_str("  (no calls)\n");
        return;
    }
    let max = summary
        .series
        .iter()
        .map(|bucket| metric.units(&bucket.totals))
        .max()
        .unwrap_or(0);
    for bucket in &summary.series {
        push_line(
            out,
            format_args!(
                "  {:<22}  {:<width$}  {:>9}  {:>9}",
                bucket.label,
                bar(metric.units(&bucket.totals), max, BAR_WIDTH),
                metric.format(&bucket.totals),
                money(bucket.totals.cost_usd),
                width = BAR_WIDTH
            ),
        );
    }
}

fn table(out: &mut String, title: &str, rows: &[(String, Totals)], total: &Totals) {
    push_line(out, format_args!("{title}"));
    if rows.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0)
        .max("value".len());
    push_line(
        out,
        format_args!(
            "  {:<width$}  {:>9}  {:>7}  {:>10}  {:>8}",
            "value", "tokens", "share%", "cost", "hit-rate"
        ),
    );
    for (label, totals) in rows {
        push_line(
            out,
            format_args!(
                "  {:<width$}  {:>9}  {:>7}  {:>10}  {:>8}",
                label,
                compact(tokens(totals)),
                pct(tokens(totals), tokens(total)),
                money(totals.cost_usd),
                hit_rate(totals)
            ),
        );
    }
}

/// The `--split` table, assembled from every bucket's `by` map. `by` keeps only
/// the busiest values of a bucket, so the unfolded rest of each bucket is the
/// `other` row, always printed last.
fn split_rows(summary: &UsageSummary) -> Vec<(String, Totals)> {
    let mut values: BTreeMap<String, Totals> = BTreeMap::new();
    let mut other = Totals::default();
    for bucket in &summary.series {
        let mut kept = Totals::default();
        if let Some(by) = &bucket.by {
            for (value, totals) in by {
                accumulate(values.entry(value.clone()).or_default(), totals);
                accumulate(&mut kept, totals);
            }
        }
        let mut rest = bucket.totals.clone();
        subtract(&mut rest, &kept);
        accumulate(&mut other, &rest);
    }
    let mut rows: Vec<(String, Totals)> = values.into_iter().collect();
    rows.sort_by(|a, b| tokens(&b.1).cmp(&tokens(&a.1)).then_with(|| a.0.cmp(&b.0)));
    rows.push(("other".to_string(), other));
    rows
}

fn row(out: &mut String, label: &str, value: String) {
    push_line(out, format_args!("  {label:<9}{value}"));
}

fn push_line(out: &mut String, args: std::fmt::Arguments<'_>) {
    let _ = writeln!(out, "{args}");
}

// ── totals & labels ─────────────────────────────────────────────────

/// Input + output, as the Web UI's token cards count them.
fn tokens(t: &Totals) -> u64 {
    t.input_tokens.saturating_add(t.output_tokens)
}

fn granularity_label(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Hour => "hour",
        Granularity::Day => "day",
        Granularity::Week => "week",
        Granularity::Month => "month",
    }
}

fn split_label(dim: SplitDim) -> &'static str {
    match dim {
        SplitDim::Provider => "provider",
        SplitDim::Agent => "agent",
        SplitDim::Model => "model",
        SplitDim::Account => "account",
    }
}

/// Adds all the numbers of `from` into `into`, so a split can be summed across
/// buckets.
fn accumulate(into: &mut Totals, from: &Totals) {
    into.calls = into.calls.saturating_add(from.calls);
    into.errors = into.errors.saturating_add(from.errors);
    into.input_tokens = into.input_tokens.saturating_add(from.input_tokens);
    into.output_tokens = into.output_tokens.saturating_add(from.output_tokens);
    into.cache_read_tokens = into
        .cache_read_tokens
        .saturating_add(from.cache_read_tokens);
    into.cache_write_tokens = into
        .cache_write_tokens
        .saturating_add(from.cache_write_tokens);
    into.cache_write_1h = into.cache_write_1h.saturating_add(from.cache_write_1h);
    into.cache_unknown_tokens = into
        .cache_unknown_tokens
        .saturating_add(from.cache_unknown_tokens);
    into.reasoning_tokens = into.reasoning_tokens.saturating_add(from.reasoning_tokens);
    into.latency_sum_ms = into.latency_sum_ms.saturating_add(from.latency_sum_ms);
    into.latency_max_ms = into.latency_max_ms.max(from.latency_max_ms);
    into.ttft_sum_ms = into.ttft_sum_ms.saturating_add(from.ttft_sum_ms);
    into.ttft_calls = into.ttft_calls.saturating_add(from.ttft_calls);
    into.cost_usd += from.cost_usd;
    into.unpriced = into.unpriced.saturating_add(from.unpriced);
    into.retried = into.retried.saturating_add(from.retried);
    into.loops = into.loops.saturating_add(from.loops);
    into.decode_ms = into.decode_ms.saturating_add(from.decode_ms);
    into.decode_out = into.decode_out.saturating_add(from.decode_out);
    into.decode_n = into.decode_n.saturating_add(from.decode_n);
}

/// `into -= from`, clamped at zero so rounding in the split can never produce a
/// negative remainder.
fn subtract(into: &mut Totals, from: &Totals) {
    into.calls = into.calls.saturating_sub(from.calls);
    into.errors = into.errors.saturating_sub(from.errors);
    into.input_tokens = into.input_tokens.saturating_sub(from.input_tokens);
    into.output_tokens = into.output_tokens.saturating_sub(from.output_tokens);
    into.cache_read_tokens = into
        .cache_read_tokens
        .saturating_sub(from.cache_read_tokens);
    into.cache_write_tokens = into
        .cache_write_tokens
        .saturating_sub(from.cache_write_tokens);
    into.cache_write_1h = into.cache_write_1h.saturating_sub(from.cache_write_1h);
    into.cache_unknown_tokens = into
        .cache_unknown_tokens
        .saturating_sub(from.cache_unknown_tokens);
    into.reasoning_tokens = into.reasoning_tokens.saturating_sub(from.reasoning_tokens);
    into.latency_sum_ms = into.latency_sum_ms.saturating_sub(from.latency_sum_ms);
    into.ttft_sum_ms = into.ttft_sum_ms.saturating_sub(from.ttft_sum_ms);
    into.ttft_calls = into.ttft_calls.saturating_sub(from.ttft_calls);
    into.cost_usd = (into.cost_usd - from.cost_usd).max(0.0);
    into.unpriced = into.unpriced.saturating_sub(from.unpriced);
    into.retried = into.retried.saturating_sub(from.retried);
    into.loops = into.loops.saturating_sub(from.loops);
    into.decode_ms = into.decode_ms.saturating_sub(from.decode_ms);
    into.decode_out = into.decode_out.saturating_sub(from.decode_out);
    into.decode_n = into.decode_n.saturating_sub(from.decode_n);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_puts_the_split_remainder_last() {
        let totals = Totals {
            calls: 3,
            input_tokens: 300,
            output_tokens: 30,
            cost_usd: 0.3,
            ..Totals::default()
        };
        let mut by = BTreeMap::new();
        by.insert(
            "kept".to_string(),
            Totals {
                calls: 2,
                input_tokens: 200,
                output_tokens: 20,
                ..Totals::default()
            },
        );
        let summary = UsageSummary {
            from: "2026-07-09T00:00:00Z".to_string(),
            to: "2026-07-16T00:00:00Z".to_string(),
            granularity: Granularity::Day,
            totals: totals.clone(),
            series: vec![any_converter_server::storage::Bucket {
                label: "2026-07-10".to_string(),
                timestamp: "2026-07-10".to_string(),
                totals: totals.clone(),
                by: Some(by),
            }],
            by_provider: Vec::new(),
            by_model: Vec::new(),
            by_agent: Vec::new(),
            by_account: Vec::new(),
            by_session: Vec::new(),
        };

        let report = render(&summary, Metric::Tokens, Some(SplitDim::Provider));
        assert!(report.starts_with("Usage — 2026-07-09T00:00:00Z → 2026-07-16T00:00:00Z  (day)"));
        assert!(report.contains("By provider"));
        let other = report.find("other").expect("the other row is rendered");
        let kept = report.find("kept").expect("the split value is rendered");
        assert!(kept < other);
    }
}
