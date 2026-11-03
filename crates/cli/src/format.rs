//! Pure number formatting for the `stats` report.
//!
//! Everything here takes numbers (or a `Totals`) and returns a `String`: no
//! stdout, no storage, no layout. [`crate::stats`] owns where these strings go.

use any_converter_server::storage::Totals;

/// Stands in for "this metric was never measured".
pub(crate) const NONE: &str = "—";

/// `820ms`, `1.2s`.
pub(crate) fn ms(value: u64) -> String {
    if value < 1000 {
        format!("{value}ms")
    } else {
        format!("{:.1}s", value as f64 / 1000.0)
    }
}

/// `42`, `1.2k`, `3.4M`, `1.1G` — one decimal below 100, whole numbers at 100+.
pub(crate) fn compact(n: u64) -> String {
    const UNITS: [&str; 4] = ["", "k", "M", "G"];
    if n < 1000 {
        return n.to_string();
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 99.95 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

/// `$0.00`, `$1.23`, `$1,234.56`.
pub(crate) fn money(v: f64) -> String {
    let cents = (v.abs() * 100.0).round() as u64;
    let sign = if v < 0.0 && cents > 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", grouped(cents / 100), cents % 100)
}

fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(crate) fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        return NONE.to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

pub(crate) fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let filled = (value.min(max) as u128 * width as u128 / max as u128) as usize;
    "█".repeat(filled)
}

pub(crate) fn hit_rate(t: &Totals) -> String {
    match t.hit_rate() {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => NONE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_keeps_small_numbers_and_scales_the_rest() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(1), "1");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1.0k");
        assert_eq!(compact(1234), "1.2k");
        assert_eq!(compact(99_999), "100k");
        assert_eq!(compact(1_000_000), "1.0M");
        assert_eq!(compact(3_400_000), "3.4M");
        assert_eq!(compact(999_999), "1.0M");
        assert_eq!(compact(1_100_000_000), "1.1G");
    }

    #[test]
    fn money_rounds_to_cents_and_groups_thousands() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(1.0), "$1.00");
        assert_eq!(money(1.234), "$1.23");
        assert_eq!(money(1234.56), "$1,234.56");
        assert_eq!(money(1_234_567.891), "$1,234,567.89");
    }

    #[test]
    fn pct_reports_one_decimal_or_a_dash() {
        assert_eq!(pct(0, 0), "—");
        assert_eq!(pct(0, 10), "0.0%");
        assert_eq!(pct(1, 8), "12.5%");
        assert_eq!(pct(10, 10), "100.0%");
    }

    #[test]
    fn bar_is_only_blocks_and_empty_without_a_max() {
        assert_eq!(bar(0, 0, 32), "");
        assert_eq!(bar(5, 0, 32), "");
        assert_eq!(bar(0, 10, 32), "");
        assert_eq!(bar(5, 10, 32), "█".repeat(16));
        assert_eq!(bar(10, 10, 32), "█".repeat(32));
        assert_eq!(bar(50, 10, 32), "█".repeat(32));
        assert!(bar(1, 10, 32).chars().all(|c| c == '█'));
    }

    #[test]
    fn hit_rate_is_a_dash_without_cache_data() {
        assert_eq!(hit_rate(&Totals::default()), "—");
        assert_eq!(
            hit_rate(&Totals {
                input_tokens: 50,
                cache_read_tokens: 50,
                ..Totals::default()
            }),
            "50.0%"
        );
        // A vendor that counted no cache at all leaves an empty denominator.
        assert_eq!(
            hit_rate(&Totals {
                input_tokens: 10,
                cache_read_tokens: 10,
                cache_unknown_tokens: 20,
                ..Totals::default()
            }),
            "—"
        );
    }
}
