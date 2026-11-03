//! Rendering for the live `stats --tui` screen.
//!
//! Pure drawing: no terminal setup, no storage, no keys. Every number is either
//! formatted by [`crate::format`] or lifted straight out of the [`App`], so a
//! frame can be produced from any state the reducer can reach. Every colour
//! comes from [`App::palette`], which was decided once at startup; the timeline
//! line is written out exactly as [`App::refresh`] left it.

use std::collections::BTreeMap;

use any_converter_server::storage::{Granularity, SplitDim, Totals};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::format::{NONE, compact, hit_rate, money, ms, pct};
use crate::tui::app::{App, Focus, Metric, Preset};
use crate::tui::theme::Palette;

/// The fixed bands of the frame: switches, totals, timeline, breakdown, footer.
/// Whatever is left over belongs to the ledger.
const SWITCHES_HEIGHT: u16 = 3;
const TOTALS_HEIGHT: u16 = 5;
const TIMELINE_HEIGHT: u16 = 1;
/// The breakdown panel is sized to the rows it has, up to this many: a table
/// with three providers should not hold eight rows while the ledger, the part
/// someone watching live actually reads, is squeezed to two.
const BREAKDOWN_ROWS: u16 = 6;
const FOOTER_HEIGHT: u16 = 2;

const KEYS: &str = "q quit · p/P period · g granularity · s split · m metric · \
                    ↑↓ select · tab focus · space pause · r refresh";

const ATOMIC: [(Granularity, &str); 4] = [
    (Granularity::Hour, "hour"),
    (Granularity::Day, "day"),
    (Granularity::Week, "week"),
    (Granularity::Month, "month"),
];

const SPLITS: [(Option<SplitDim>, &str); 4] = [
    (None, "none"),
    (Some(SplitDim::Provider), "provider"),
    (Some(SplitDim::Model), "model"),
    (Some(SplitDim::Agent), "agent"),
];

pub fn render(frame: &mut Frame, app: &App) {
    let [switches, totals, timeline, breakdown, ledger, footer] = Layout::vertical([
        Constraint::Length(SWITCHES_HEIGHT),
        Constraint::Length(TOTALS_HEIGHT),
        // One row, whatever the terminal height: the chart is a line now.
        Constraint::Length(TIMELINE_HEIGHT),
        Constraint::Length(breakdown_height(app)),
        // Every row the other bands do not need goes to the ledger.
        Constraint::Min(8),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .areas(frame.area());

    frame.render_widget(self::switches(app), switches);
    frame.render_widget(self::totals(app), totals);
    frame.render_widget(self::timeline(app), timeline);
    self::breakdown(frame, app, breakdown);
    calls(frame, app, ledger);
    frame.render_widget(footer_line(app), footer);
}

// ── row 1: the switches ──────────────────────────────────────────────

fn switches(app: &App) -> Paragraph<'static> {
    let metric = Metric::ALL.map(|value| (value, value.label()));
    let mut last = group(app.palette, "split", SPLITS, app.split);
    last.push(Span::styled("·  ", app.palette.muted()));
    last.extend(group(app.palette, "metric", metric, app.metric));

    Paragraph::new(vec![
        Line::from(group(
            app.palette,
            "period",
            Preset::ALL.map(|p| (p, p.label())),
            app.preset,
        )),
        Line::from(group(app.palette, "granularity", ATOMIC, app.granularity)),
        Line::from(last),
    ])
}

/// One switch group: every value, the active one reversed and bracketed.
fn group<T: PartialEq>(
    palette: Palette,
    title: &'static str,
    values: impl IntoIterator<Item = (T, &'static str)>,
    active: T,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(format!("{title:<12}"), palette.muted())];
    for (value, label) in values {
        if value == active {
            spans.push(Span::styled(
                format!("[{label}]"),
                palette.selected().add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, palette.muted()));
        }
        spans.push(Span::raw(" "));
    }
    spans
}

// ── row 2: the totals ────────────────────────────────────────────────

fn totals(app: &App) -> Paragraph<'static> {
    let t = &app.summary.totals;

    let mut cost = money(t.cost_usd);
    if t.unpriced > 0 {
        cost.push_str(&format!("  ·  {} unpriced", compact(t.unpriced)));
    }

    Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Totals  ", app.palette.bold_text()),
            Span::styled(
                format!(
                    "{} → {}  ({})",
                    app.summary.from,
                    app.summary.to,
                    granularity_label(app.summary.granularity)
                ),
                app.palette.text(),
            ),
        ]),
        row(vec![
            kv(app.palette, "calls", compact(t.calls)),
            dot(app.palette),
            kv(
                app.palette,
                "errors",
                format!("{} ({})", compact(t.errors), pct(t.errors, t.calls)),
            ),
            dot(app.palette),
            kv(
                app.palette,
                "retried",
                format!("{} ({})", compact(t.retried), pct(t.retried, t.calls)),
            ),
        ]),
        row(vec![
            kv(
                app.palette,
                "tokens",
                format!(
                    "{} in / {} out",
                    compact(t.input_tokens),
                    compact(t.output_tokens)
                ),
            ),
            dot(app.palette),
            kv(app.palette, "cache", format!("{} hit", hit_rate(t))),
        ]),
        row(vec![
            kv(app.palette, "cost", cost),
            dot(app.palette),
            kv(
                app.palette,
                "latency",
                match t.mean_latency_ms() {
                    Some(mean) => format!("mean {} / max {}", ms(mean), ms(t.latency_max_ms)),
                    None => NONE.to_string(),
                },
            ),
        ]),
        row(vec![
            kv(
                app.palette,
                "ttft",
                t.mean_ttft_ms().map(ms).unwrap_or_else(|| NONE.to_string()),
            ),
            dot(app.palette),
            kv(
                app.palette,
                "speed",
                t.speed()
                    .map(|speed| format!("{speed:.1} tok/s"))
                    .unwrap_or_else(|| NONE.to_string()),
            ),
        ]),
    ])
}

/// `label  value` — the label dim, the value plain.
fn kv(palette: Palette, label: &'static str, value: impl Into<String>) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label:<8}"), palette.muted()),
        Span::styled(value.into(), palette.text()),
    ]
}

fn dot(palette: Palette) -> Vec<Span<'static>> {
    vec![Span::styled("  ·  ", palette.muted())]
}

/// Glues the fields of one totals line together with their separators.
fn row(parts: Vec<Vec<Span<'static>>>) -> Line<'static> {
    Line::from(parts.into_iter().flatten().collect::<Vec<_>>())
}

// ── row 3: the timeline ─────────────────────────────────────────────

/// The one-line timeline, exactly as [`App::refresh`] built it. The renderer
/// does not touch the numbers: they were formatted once, when they changed.
fn timeline(app: &App) -> Paragraph<'_> {
    let style = if app.focus == Focus::Timeline {
        app.palette.accent()
    } else {
        app.palette.text()
    };
    Paragraph::new(Span::styled(app.timeline.as_str(), style))
}

/// Rows the breakdown panel needs: its data, the table header, and the two
/// borders. Leaving out the header truncated the last provider.
fn breakdown_height(app: &App) -> u16 {
    let rows = app.summary.by_provider.len().max(1) as u16;
    rows.min(BREAKDOWN_ROWS) + 3
}

// ── row 4: providers and the split ranking ──────────────────────────

fn breakdown(frame: &mut Frame, app: &App, area: Rect) {
    let Some(dim) = app.split else {
        providers(frame, app, area);
        return;
    };
    let [provider_area, split_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
    providers(frame, app, provider_area);
    split_panel(frame, app, split_area, dim);
}

fn providers(frame: &mut Frame, app: &App, area: Rect) {
    let total = tokens(&app.summary.totals);
    let rows: Vec<Row> = app
        .summary
        .by_provider
        .iter()
        .map(|group| {
            Row::new(vec![
                Cell::from(group.id.clone()),
                Cell::from(compact(tokens(&group.totals))),
                Cell::from(pct(tokens(&group.totals), total)),
                Cell::from(money(group.totals.cost_usd)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(9),
        ],
    )
    .header(Row::new(["provider", "tokens", "share", "cost"]).style(app.palette.bold_text()))
    .column_spacing(1)
    .style(app.palette.text())
    .block(
        Block::bordered()
            .title("by provider")
            .border_style(app.palette.faint())
            .title_style(app.palette.muted()),
    );
    frame.render_widget(table, area);
}

fn split_panel(frame: &mut Frame, app: &App, area: Rect, dim: SplitDim) {
    let block = Block::bordered()
        .title(format!("split · {}", split_label(dim)))
        .border_style(app.palette.faint())
        .title_style(app.palette.muted());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(split_rows(app, inner.height)), inner);
}

/// The `--split` ranking as it stands over the whole window: the unfolded values
/// of every bucket, busiest first. `by` keeps only the busiest values of a
/// bucket, so this is a floor on each name, never a total of all of them.
fn split_rows(app: &App, height: u16) -> Vec<Line<'static>> {
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    for bucket in &app.summary.series {
        if let Some(by) = &bucket.by {
            for (value, totals_at) in by {
                let entry = totals.entry(value.as_str()).or_default();
                *entry = entry.saturating_add(tokens(totals_at));
            }
        }
    }
    let mut rows: Vec<(&str, u64)> = totals.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let window = tokens(&app.summary.totals);
    let width = rows
        .iter()
        .take(height as usize)
        .map(|(value, _)| value.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    rows.into_iter()
        .take(height as usize)
        .map(|(value, count)| {
            Line::from(vec![
                Span::styled(format!("{value:<width$}  "), app.palette.text()),
                Span::styled(
                    format!("{:>8}  {:>6}", compact(count), pct(count, window)),
                    app.palette.muted(),
                ),
            ])
        })
        .collect()
}

// ── row 5: the recent calls ──────────────────────────────────────────

fn calls(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(format!("recent calls ({})", app.recent.len()))
        .border_style(border(app.palette, app.focus == Focus::Calls))
        .title_style(app.palette.muted());
    let inner = block.inner(area);
    // Two borders and the header row leave this many rows for the ledger.
    let visible = inner.height.saturating_sub(1) as usize;
    let offset = if visible == 0 || app.selected < visible {
        0
    } else {
        app.selected + 1 - visible
    };

    let rows: Vec<Row> = app
        .recent
        .iter()
        .enumerate()
        .skip(offset)
        .map(|(index, row)| {
            Row::new(vec![
                Cell::from(clock(&row.timestamp)),
                Cell::from(row.provider.clone()),
                Cell::from(row.client_model.clone()),
                Cell::from(row.status.to_string()).style(status(app.palette, row.status)),
                Cell::from(ms(row.latency_ms)),
                Cell::from(format!(
                    "{} / {}",
                    compact(row.input_tokens),
                    compact(row.output_tokens)
                )),
            ])
            .style(if index == app.selected {
                app.palette.selected()
            } else {
                app.palette.text()
            })
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(6),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(["time", "provider", "model", "status", "latency", "tokens"])
            .style(app.palette.bold_text()),
    )
    .column_spacing(1)
    .style(app.palette.text())
    .block(block);
    frame.render_widget(table, area);
}

/// `2026-07-10T12:34:56Z` → `12:34:56`; anything unexpected is passed through.
fn clock(timestamp: &str) -> String {
    timestamp
        .get(11..19)
        .map(str::to_string)
        .unwrap_or_else(|| timestamp.to_string())
}

fn status(palette: Palette, code: u16) -> Style {
    if code >= 400 {
        palette.bad()
    } else {
        palette.ok()
    }
}

// ── row 6: the footer ────────────────────────────────────────────────

fn footer_line(app: &App) -> Paragraph<'static> {
    let mut state = if app.paused {
        vec![Span::styled(
            "⏸ paused",
            app.palette.accent().add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![
            Span::styled("● live", app.palette.ok()),
            Span::styled(
                format!(
                    "  {}s since last refresh",
                    app.last_refresh.elapsed().as_secs()
                ),
                app.palette.muted(),
            ),
        ]
    };
    if let Some(error) = &app.error {
        state.push(Span::styled(
            format!("  ·  {error}"),
            app.palette.bad().add_modifier(Modifier::BOLD),
        ));
    }

    Paragraph::new(vec![
        Line::from(Span::styled(KEYS, app.palette.muted())),
        Line::from(state),
    ])
}

// ── shared bits ──────────────────────────────────────────────────────

fn border(palette: Palette, focused: bool) -> Style {
    if focused {
        palette.accent()
    } else {
        palette.faint()
    }
}

fn split_label(dim: SplitDim) -> &'static str {
    match dim {
        SplitDim::Provider => "provider",
        SplitDim::Model => "model",
        SplitDim::Agent => "agent",
        SplitDim::Account => "account",
    }
}

fn granularity_label(granularity: Granularity) -> &'static str {
    ATOMIC
        .iter()
        .find(|(value, _)| *value == granularity)
        .map(|(_, label)| *label)
        .unwrap_or("day")
}

/// Input + output, as the Web UI's token cards count them.
fn tokens(t: &Totals) -> u64 {
    t.input_tokens.saturating_add(t.output_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_keeps_the_time_of_day() {
        assert_eq!(clock("2026-07-10T12:34:56Z"), "12:34:56");
        assert_eq!(clock("unknown"), "unknown");
        assert_eq!(clock(""), "");
    }
}
