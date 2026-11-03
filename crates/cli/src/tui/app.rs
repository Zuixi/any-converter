//! State and the key reducer for the live `stats --tui` screen.
//!
//! No terminal lives here: a key goes in, fields change, and [`App::refresh`]
//! turns the current dimensions into the two queries the screen draws from. That
//! keeps the whole screen testable without a TTY.

use std::time::Instant;

use any_converter_server::period;
use any_converter_server::storage::{
    Granularity, LedgerFilter, LedgerRow, SplitDim, SqliteStorage, Totals, UsageQuery, UsageSummary,
};
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::format::{NONE, compact, money, ms};
use crate::stats::empty_summary;
use crate::tui::theme::Palette;
use crate::tui::timeline;

/// How many detail rows of the ledger the screen keeps to scroll through.
const RECENT_LIMIT: u64 = 40;

// ── dimensions ───────────────────────────────────────────────────────

/// The window switches on the first row. `label` is also the `--period` string
/// the same window is spelled with on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Today,
    Days3,
    Days7,
    Days30,
    All,
}

impl Preset {
    pub const ALL: [Self; 5] = [
        Self::Today,
        Self::Days3,
        Self::Days7,
        Self::Days30,
        Self::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Days3 => "3d",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::All => "all",
        }
    }

    /// The bucket width the one-shot report picks for this window: a single day
    /// by hour, everything by week. Pressing `p` switches this together with the
    /// window, so `today` never reads as one fat day bar.
    pub fn default_granularity(self) -> Granularity {
        match self {
            Self::Today => Granularity::Hour,
            Self::All => Granularity::Week,
            _ => Granularity::Day,
        }
    }

    fn step(self, forward: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|preset| *preset == self)
            .unwrap_or(0);
        let len = Self::ALL.len();
        let next = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        Self::ALL[next]
    }

    /// The preset a `--period` names, if it names one. The screen has five fixed
    /// windows, so a rolling window or a picked day range has no switch to land on.
    pub fn from_period(period: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|preset| preset.label() == period)
    }
}

/// What the sparkline's height counts. The one-shot `stats` flag stops at calls;
/// latency is added here because a live screen is where a jump is worth watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Tokens,
    Cost,
    Calls,
    Latency,
}

impl Metric {
    pub const ALL: [Self; 4] = [Self::Tokens, Self::Cost, Self::Calls, Self::Latency];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::Calls => "calls",
            Self::Latency => "latency",
        }
    }

    fn step(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|metric| *metric == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    /// The whole window's value, for the timeline title.
    pub fn format(self, t: &Totals) -> String {
        match self {
            Self::Tokens => compact(tokens(t)),
            Self::Cost => money(t.cost_usd),
            Self::Calls => compact(t.calls),
            Self::Latency => t
                .mean_latency_ms()
                .map(ms)
                .unwrap_or_else(|| NONE.to_string()),
        }
    }
}

impl From<crate::stats::Metric> for Metric {
    fn from(metric: crate::stats::Metric) -> Self {
        match metric {
            crate::stats::Metric::Tokens => Self::Tokens,
            crate::stats::Metric::Cost => Self::Cost,
            crate::stats::Metric::Calls => Self::Calls,
        }
    }
}

/// Input + output, as the Web UI's token cards count them.
pub(super) fn tokens(t: &Totals) -> u64 {
    t.input_tokens.saturating_add(t.output_tokens)
}

/// Which pane the next key is aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Timeline,
    Calls,
}

impl Focus {
    fn toggle(self) -> Self {
        match self {
            Self::Timeline => Self::Calls,
            Self::Calls => Self::Timeline,
        }
    }
}

/// The filter flags, which the switches never change: they are the ones the
/// screen was opened with.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub account: Option<String>,
}

/// Whether the loop keeps running after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

// ── state ────────────────────────────────────────────────────────────

pub struct App {
    pub preset: Preset,
    pub granularity: Granularity,
    pub split: Option<SplitDim>,
    pub metric: Metric,
    pub summary: UsageSummary,
    pub recent: Vec<LedgerRow>,
    pub focus: Focus,
    pub selected: usize,
    pub paused: bool,
    pub error: Option<String>,
    pub last_refresh: Instant,
    pub dirty: bool,
    pub filters: Filters,
    pub palette: Palette,
    /// The `timeline` line's content, rebuilt whenever the summary or the
    /// metric changes — never in `view`.
    pub timeline: String,
}

impl App {
    pub fn new(
        preset: Preset,
        granularity: Granularity,
        split: Option<SplitDim>,
        metric: Metric,
    ) -> Self {
        // The opening frame is drawn from an empty summary (and says so)
        // until the first `refresh` brings the real one.
        let summary = blank(granularity);
        let timeline = timeline::timeline_line(&summary, metric);
        Self {
            preset,
            granularity,
            split,
            metric,
            summary,
            recent: Vec::new(),
            focus: Focus::Timeline,
            selected: 0,
            paused: false,
            error: None,
            last_refresh: Instant::now(),
            // Nothing is loaded yet, so the first tick must fetch.
            dirty: true,
            filters: Filters::default(),
            // Detected once: the terminal's background does not change while
            // the screen is open.
            palette: Palette::detect(),
            timeline,
        }
    }

    /// The filters the screen was opened with. Kept out of [`App::new`] so the
    /// reducer tests do not have to spell the command line out.
    pub fn with_filters(mut self, filters: Filters) -> Self {
        self.filters = filters;
        self
    }

    /// Applies one key. Only the dimensions that change the query set `dirty`;
    /// the metric and the selection are drawn from data that is already here.
    pub fn on_key(&mut self, key: KeyEvent) -> Flow {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            // Raw mode turns Ctrl-C from a signal into a key: without this it
            // would be the one key that does nothing.
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Flow::Quit;
            }
            KeyCode::Char('p') => {
                self.preset = self.preset.step(true);
                self.granularity = self.preset.default_granularity();
                self.dirty = true;
            }
            KeyCode::Char('P') => {
                self.preset = self.preset.step(false);
                self.granularity = self.preset.default_granularity();
                self.dirty = true;
            }
            KeyCode::Char('g') => {
                self.granularity = next_granularity(self.granularity);
                self.dirty = true;
            }
            KeyCode::Char('s') => {
                self.split = next_split(self.split);
                self.dirty = true;
            }
            KeyCode::Char('m') => {
                self.metric = self.metric.step();
                // The metric changes the bars and the totals on the timeline,
                // but not the query behind them: rebuild from what is here.
                self.rebuild_timeline();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = move_selection(self.selected, self.recent.len(), false);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = move_selection(self.selected, self.recent.len(), true);
            }
            KeyCode::Tab => self.focus = self.focus.toggle(),
            KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('r') => self.dirty = true,
            _ => {}
        }
        Flow::Continue
    }

    /// Re-queries the current window. A failure lands in `error` and the frame
    /// that is already on screen stays: a locked or corrupt database must not
    /// take the screen down with it.
    pub fn refresh(&mut self, store: &SqliteStorage) {
        match load(store, self, Utc::now()) {
            Ok((summary, recent)) => {
                self.summary = summary;
                self.recent = recent;
                self.error = None;
                self.selected = self.selected.min(self.recent.len().saturating_sub(1));
            }
            // The next tick tries again; a failed fetch is never retried in a
            // tight loop, because `dirty` is spent either way.
            Err(error) => self.error = Some(error),
        }
        self.rebuild_timeline();
        self.dirty = false;
        self.last_refresh = Instant::now();
    }

    /// Rebuilds the one-line timeline from the summary already on hand. Called
    /// by the constructor, by `refresh`, and wherever the metric changes; the
    /// renderer only ever writes the resulting string out.
    fn rebuild_timeline(&mut self) {
        self.timeline = timeline::timeline_line(&self.summary, self.metric);
    }
}

pub(super) fn blank(granularity: Granularity) -> UsageSummary {
    empty_summary(&UsageQuery {
        from: String::new(),
        to: String::new(),
        granularity,
        provider: None,
        model: None,
        agent: None,
        account: None,
        split_by: None,
    })
}

fn next_granularity(granularity: Granularity) -> Granularity {
    const ORDER: [Granularity; 4] = [
        Granularity::Hour,
        Granularity::Day,
        Granularity::Week,
        Granularity::Month,
    ];
    let index = ORDER
        .iter()
        .position(|value| *value == granularity)
        .unwrap_or(0);
    ORDER[(index + 1) % ORDER.len()]
}

/// `None → provider → model → agent → none`. The one-shot report also knows an
/// account split; a subscription account is too high-cardinality for a sparkline.
fn next_split(split: Option<SplitDim>) -> Option<SplitDim> {
    match split {
        None => Some(SplitDim::Provider),
        Some(SplitDim::Provider) => Some(SplitDim::Model),
        Some(SplitDim::Model) => Some(SplitDim::Agent),
        Some(SplitDim::Agent | SplitDim::Account) => None,
    }
}

/// One step down the list, clamped to the rows that exist.
fn move_selection(selected: usize, len: usize, forward: bool) -> usize {
    if forward {
        (selected + 1).min(len.saturating_sub(1))
    } else {
        selected.saturating_sub(1)
    }
}

fn load(
    store: &SqliteStorage,
    app: &App,
    now: chrono::DateTime<Utc>,
) -> Result<(UsageSummary, Vec<LedgerRow>), String> {
    let range = period::resolve(app.preset.label(), now).map_err(|error| error.to_string())?;
    let query = UsageQuery {
        from: range.from.clone(),
        to: range.to.clone(),
        granularity: app.granularity,
        provider: app.filters.provider.clone(),
        model: app.filters.model.clone(),
        agent: app.filters.agent.clone(),
        account: app.filters.account.clone(),
        split_by: app.split,
    };
    let summary = store
        .usage_summary(&query)
        .map_err(|error| error.to_string())?;
    let filter = LedgerFilter {
        from: range.from,
        to: range.to,
        provider: app.filters.provider.clone(),
        model: app.filters.model.clone(),
        agent: app.filters.agent.clone(),
        account: app.filters.account.clone(),
        ..LedgerFilter::default()
    };
    let page = store
        .ledger_page(&filter, None, RECENT_LIMIT)
        .map_err(|error| error.to_string())?;
    Ok((summary, page.rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use any_converter_server::storage::Bucket;
    use crossterm::event::KeyModifiers;

    fn app() -> App {
        App::new(Preset::Days7, Granularity::Day, None, Metric::Tokens)
    }

    /// A three-day token series (10, 100, 200) with a matching window.
    fn token_summary() -> UsageSummary {
        let values = [10_u64, 100, 200];
        let mut summary = blank(Granularity::Day);
        summary.from = "2026-10-28T00:00:00Z".to_string();
        summary.to = "2026-10-30T00:00:00Z".to_string();
        summary.series = values
            .iter()
            .enumerate()
            .map(|(index, value)| Bucket {
                label: format!("10-{:02}", 28 + index),
                timestamp: format!("2026-10-{:02}T00:00:00Z", 28 + index),
                totals: Totals {
                    input_tokens: *value,
                    // Cost rides along so a metric switch can still draw.
                    cost_usd: *value as f64 / 100.0,
                    ..Totals::default()
                },
                by: None,
            })
            .collect();
        summary.totals.input_tokens = values.iter().sum();
        summary
    }

    fn press(app: &mut App, code: KeyCode) -> Flow {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn the_metric_key_rebuilds_the_line_without_asking_for_data() {
        let mut app = app();
        app.summary = token_summary();
        app.rebuild_timeline();
        assert!(app.timeline.contains("tokens"));

        app.dirty = false;
        assert_eq!(press(&mut app, KeyCode::Char('m')), Flow::Continue);
        assert_eq!(app.metric, Metric::Cost);
        assert!(app.timeline.contains("cost"));
        assert!(!app.dirty, "the metric is drawn from data already on hand");
    }

    #[test]
    fn q_and_escape_quit_and_change_nothing() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('q')), Flow::Quit);
        assert_eq!(press(&mut app, KeyCode::Esc), Flow::Quit);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Flow::Quit
        );
        // A bare `c` is not a quit, it is just a letter nobody asked for.
        assert_eq!(press(&mut app, KeyCode::Char('c')), Flow::Continue);
        assert_eq!(app.preset, Preset::Days7);
        assert_eq!(app.metric, Metric::Tokens);
        assert!(!app.paused);
    }

    #[test]
    fn p_cycles_the_window_forward_and_back_and_brings_its_bucket_width() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('p')), Flow::Continue);
        assert_eq!(app.preset, Preset::Days30);
        assert_eq!(app.granularity, Granularity::Day);
        assert!(app.dirty);

        assert_eq!(press(&mut app, KeyCode::Char('p')), Flow::Continue);
        assert_eq!(app.preset, Preset::All);
        assert_eq!(app.granularity, Granularity::Week);

        assert_eq!(press(&mut app, KeyCode::Char('P')), Flow::Continue);
        assert_eq!(app.preset, Preset::Days30);
        assert_eq!(app.granularity, Granularity::Day);

        // Backwards past the first preset wraps to the last.
        let mut app = App::new(Preset::Today, Granularity::Hour, None, Metric::Tokens);
        assert_eq!(press(&mut app, KeyCode::Char('P')), Flow::Continue);
        assert_eq!(app.preset, Preset::All);
        assert_eq!(app.granularity, Granularity::Week);
    }

    #[test]
    fn g_cycles_every_bucket_width_and_sets_dirty() {
        let mut app = app();
        for expected in [
            Granularity::Week,
            Granularity::Month,
            Granularity::Hour,
            Granularity::Day,
        ] {
            app.dirty = false;
            assert_eq!(press(&mut app, KeyCode::Char('g')), Flow::Continue);
            assert_eq!(app.granularity, expected);
            assert!(app.dirty);
        }
    }

    #[test]
    fn s_cycles_the_split_and_sets_dirty() {
        let mut app = app();
        for expected in [
            Some(SplitDim::Provider),
            Some(SplitDim::Model),
            Some(SplitDim::Agent),
            None,
        ] {
            app.dirty = false;
            assert_eq!(press(&mut app, KeyCode::Char('s')), Flow::Continue);
            assert_eq!(app.split, expected);
            assert!(app.dirty);
        }
    }

    #[test]
    fn m_cycles_the_metric_without_asking_for_data() {
        let mut app = app();
        for expected in [Metric::Cost, Metric::Calls, Metric::Latency, Metric::Tokens] {
            app.dirty = false;
            assert_eq!(press(&mut app, KeyCode::Char('m')), Flow::Continue);
            assert_eq!(app.metric, expected);
            assert!(!app.dirty, "the metric is drawn from data already on hand");
        }
    }

    #[test]
    fn tab_space_and_r_do_not_rerun_the_query() {
        let mut app = app();
        app.dirty = false;

        assert_eq!(press(&mut app, KeyCode::Tab), Flow::Continue);
        assert_eq!(app.focus, Focus::Calls);
        assert_eq!(press(&mut app, KeyCode::Tab), Flow::Continue);
        assert_eq!(app.focus, Focus::Timeline);

        assert_eq!(press(&mut app, KeyCode::Char(' ')), Flow::Continue);
        assert!(app.paused);
        assert_eq!(press(&mut app, KeyCode::Char(' ')), Flow::Continue);
        assert!(!app.paused);

        assert!(!app.dirty);
        assert_eq!(press(&mut app, KeyCode::Char('r')), Flow::Continue);
        assert!(app.dirty);
    }

    #[test]
    fn arrows_walk_the_selection_within_the_rows_that_exist() {
        let mut app = app();
        app.recent = Vec::new();
        app.dirty = false;
        for code in [
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Up,
            KeyCode::Char('k'),
        ] {
            assert_eq!(press(&mut app, code), Flow::Continue);
            assert_eq!(app.selected, 0, "an empty ledger keeps the cursor at 0");
            assert!(!app.dirty);
        }

        assert_eq!(move_selection(0, 3, true), 1);
        assert_eq!(move_selection(1, 3, true), 2);
        assert_eq!(move_selection(2, 3, true), 2);
        assert_eq!(move_selection(2, 3, false), 1);
        assert_eq!(move_selection(0, 3, false), 0);
        assert_eq!(move_selection(0, 0, true), 0);
        assert_eq!(move_selection(0, 0, false), 0);
    }

    #[test]
    fn presets_round_trip_through_their_period_strings() {
        for preset in Preset::ALL {
            assert_eq!(Preset::from_period(preset.label()), Some(preset));
        }
        assert_eq!(Preset::from_period("last:24h"), None);
        assert_eq!(Preset::from_period("2026-10-01..2026-10-07"), None);
    }
}
