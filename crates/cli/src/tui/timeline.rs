//! The one-line timeline drawn above the ledger.
//!
//! Everything here is a pure function of a [`UsageSummary`] and a [`Metric`]:
//! the bar scale, a bucket's integer height, and the line they are joined into.
//! The app builds the string once per refresh and the renderer only writes it.

use any_converter_server::storage::{Totals, UsageSummary};

use crate::format::{compact, money, ms};
use crate::tui::app::{Metric, tokens};

/// A bucket's height on the sparkline, always an integer.
///
/// Cost is counted in ten-thousandths of a dollar: a bucket worth two cents is
/// `200`, so the shape survives instead of every bar rounding to zero.
fn metric_value(metric: Metric, t: &Totals) -> u64 {
    match metric {
        Metric::Tokens => tokens(t),
        Metric::Cost => (t.cost_usd.max(0.0) * 10_000.0).round() as u64,
        Metric::Calls => t.calls,
        Metric::Latency => t.mean_latency_ms().unwrap_or(0),
    }
}

// ── the one-line timeline ────────────────────────────────────────────

/// The eight heights a bucket can take, plus the blank that means zero.
/// A nine-step rune scale, one column per bucket.
const BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The whole timeline as one line: the bars, the window they cover, the total
/// for the selected metric, and the busiest bucket. Built once per refresh,
/// never per frame.
pub(super) fn timeline_line(summary: &UsageSummary, metric: Metric) -> String {
    let top = summary
        .series
        .iter()
        .map(|bucket| metric_value(metric, &bucket.totals))
        .max()
        .unwrap_or(0);
    if top == 0 {
        // Either no buckets or all of them empty: a row of blanks would read
        // as a chart, not as "nothing happened".
        return "timeline  (no data)".to_string();
    }

    let bars: String = summary
        .series
        .iter()
        .map(|bucket| bar(metric_value(metric, &bucket.totals), top))
        .collect();
    format!(
        "timeline  {bars}  {} – {}  ·  {} {}  ·  peak {}",
        day_of(&summary.from),
        day_of(&summary.to),
        metric.format(&summary.totals),
        metric.label(),
        format_metric(metric, top),
    )
}

/// The character a value draws. Zero is blank; anything above zero gets at
/// least the lowest bar, so a single call is visible next to its busiest peer.
fn bar(value: u64, top: u64) -> char {
    BARS[bar_index(value, top)]
}

/// `0` for nothing, `max(1, value * 8 / top)` for something, capped at `█`.
/// A zero `top` has no scale to divide by and stays blank.
fn bar_index(value: u64, top: u64) -> usize {
    if value == 0 || top == 0 {
        return 0;
    }
    let scaled = value.min(top) as u128 * 8 / top as u128;
    scaled.clamp(1, 8) as usize
}

/// A metric's value as the timeline prints it: cost comes back from the
/// ten-thousandths the bars are counted in.
fn format_metric(metric: Metric, value: u64) -> String {
    match metric {
        Metric::Tokens | Metric::Calls => compact(value),
        Metric::Cost => money(value as f64 / 10_000.0),
        Metric::Latency => ms(value),
    }
}

/// `2026-10-28T00:00:00Z` → `10-28`; an unexpected bound is printed as it is.
fn day_of(bound: &str) -> &str {
    bound.get(5..10).unwrap_or(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::blank;
    use any_converter_server::storage::{Bucket, Granularity};

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

    #[test]
    fn the_bar_scale_is_maggies_eight_steps() {
        assert_eq!(bar(0, 100), ' ');
        assert_eq!(bar(50, 100), '▄');
        assert_eq!(bar(100, 100), '█');
        // Any non-zero value gets at least the lowest bar, and never more
        // than the full one.
        assert_eq!(bar(1, 1_000_000), '▁');
        assert_eq!(bar(200, 100), '█');
        // A zero top has no scale to divide by.
        assert_eq!(bar_index(5, 0), 0);
        assert_eq!(bar_index(0, 0), 0);
    }

    #[test]
    fn the_timeline_is_one_line_with_the_range_and_the_peak() {
        assert_eq!(
            timeline_line(&token_summary(), Metric::Tokens),
            "timeline  ▁▄█  10-28 – 10-30  ·  310 tokens  ·  peak 200"
        );
    }

    #[test]
    fn an_empty_or_flat_series_says_no_data_instead_of_drawing_blanks() {
        assert_eq!(
            timeline_line(&blank(Granularity::Day), Metric::Tokens),
            "timeline  (no data)"
        );
        let flat = token_summary();
        let flat = UsageSummary {
            series: flat
                .series
                .iter()
                .map(|bucket| Bucket {
                    totals: Totals::default(),
                    ..bucket.clone()
                })
                .collect(),
            ..flat
        };
        assert_eq!(timeline_line(&flat, Metric::Tokens), "timeline  (no data)");
    }

    #[test]
    fn metric_value_is_an_integer_of_the_bucket() {
        let bucket = Totals {
            calls: 4,
            input_tokens: 100,
            output_tokens: 20,
            latency_sum_ms: 1000,
            cost_usd: 0.0012,
            ..Totals::default()
        };
        assert_eq!(metric_value(Metric::Tokens, &bucket), 120);
        assert_eq!(metric_value(Metric::Calls, &bucket), 4);
        assert_eq!(metric_value(Metric::Latency, &bucket), 250);
        assert_eq!(metric_value(Metric::Cost, &bucket), 12);

        // An empty bucket is flat, not absent: zero milliseconds and zero cents.
        let empty = Totals::default();
        for metric in Metric::ALL {
            assert_eq!(metric_value(metric, &empty), 0, "{metric:?}");
        }
    }
}
