//! Turning a user-facing period into concrete UTC bounds.
//!
//! The CLI, the desktop app, and the web UI all ask for "the last 7 days" or
//! "this day". Resolving that string in each caller would let their windows
//! drift apart, so the conversion lives here and every caller shares it.
//!
//! Every bound is a UTC ISO-8601 string with literal `Z` and second precision,
//! the same shape as the stored `timestamp` column, so string comparison in
//! SQLite means time comparison.

use chrono::{DateTime, Days, Duration, NaiveDate, NaiveTime, SecondsFormat, Utc};

/// A resolved half-open UTC range: `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub from: String,
    pub to: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PeriodError {
    #[error("unknown period: {0}")]
    Unknown(String),
    #[error("invalid range: {0}")]
    InvalidRange(String),
}

/// The widest accepted `<n>d` window: ten years, comfortably past any retention.
const MAX_PRESET_DAYS: i64 = 3650;

/// Resolves a preset, a rolling window, or a picked day range into UTC bounds.
///
/// Mirrored by `resolvePeriod` in `apps/web/src/app/api/log-store.ts`.
///
/// - `today`  : this UTC day, midnight to midnight
/// - `<n>d`   : the last `n` days, ending now, for any `n` from 1 to 3650
///   (so `1d`, `3d`, `7d`, and `30d` are the same kind of window)
/// - `all`    : from 1970-01-01T00:00:00Z to now
/// - `last:24h`, `last:90m`, `last:2d` : that long before now
/// - `2026-01-01..2026-01-31` : both days included, so `to` is the midnight after the last
pub fn resolve(period: &str, now: DateTime<Utc>) -> Result<Range, PeriodError> {
    let (from, to) = match period {
        "today" => {
            let start = now.date_naive().and_time(NaiveTime::MIN).and_utc();
            let end = start
                .checked_add_signed(Duration::days(1))
                .ok_or_else(|| invalid(period))?;
            (start, end)
        }
        "all" => (DateTime::<Utc>::UNIX_EPOCH, now),
        other if other.starts_with("last:") => {
            let duration = parse_last(other)?;
            (span_before(now, duration, period)?, now)
        }
        other if other.contains("..") => resolve_days(other)?,
        other => match parse_day_preset(other)? {
            Some(days) => (span_before(now, Duration::days(days), period)?, now),
            None => return Err(PeriodError::Unknown(other.to_string())),
        },
    };

    Ok(Range {
        from: format_bound(from),
        to: format_bound(to),
    })
}

/// Parses `<n>d` as a whole number of days in `1..=MAX_PRESET_DAYS`.
///
/// `None` means the period does not look like a day preset at all; a period that
/// does (`0d`, `4000d`) but falls outside the range is an error.
fn parse_day_preset(period: &str) -> Result<Option<i64>, PeriodError> {
    let Some(number) = period.strip_suffix('d') else {
        return Ok(None);
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let days: i64 = number.parse().map_err(|_| invalid(period))?;
    if !(1..=MAX_PRESET_DAYS).contains(&days) {
        return Err(invalid(period));
    }
    Ok(Some(days))
}

/// Parses the part after `last:` as a positive number with an `m`, `h`, or `d` unit.
fn parse_last(period: &str) -> Result<Duration, PeriodError> {
    let spec = period
        .strip_prefix("last:")
        .ok_or_else(|| invalid(period))?;
    let (number, unit) = match spec.char_indices().next_back() {
        Some((index, unit)) => (&spec[..index], unit),
        None => return Err(invalid(period)),
    };
    let amount: i64 = number.parse().map_err(|_| invalid(period))?;
    if amount <= 0 {
        return Err(invalid(period));
    }
    match unit {
        'm' => Duration::try_minutes(amount),
        'h' => Duration::try_hours(amount),
        'd' => Duration::try_days(amount),
        _ => None,
    }
    .ok_or_else(|| invalid(period))
}

/// Parses `first..last` as inclusive calendar days, ending at the next midnight.
fn resolve_days(period: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), PeriodError> {
    let (first, last) = period.split_once("..").ok_or_else(|| invalid(period))?;
    let first = NaiveDate::parse_from_str(first, "%Y-%m-%d").map_err(|_| invalid(period))?;
    let last = NaiveDate::parse_from_str(last, "%Y-%m-%d").map_err(|_| invalid(period))?;
    if first > last {
        return Err(invalid(period));
    }
    let from = first.and_time(NaiveTime::MIN).and_utc();
    let to = last
        .checked_add_days(Days::new(1))
        .ok_or_else(|| invalid(period))?
        .and_time(NaiveTime::MIN)
        .and_utc();
    Ok((from, to))
}

/// `now` moved back by `duration`, refusing a move off the representable calendar.
fn span_before(
    now: DateTime<Utc>,
    duration: Duration,
    period: &str,
) -> Result<DateTime<Utc>, PeriodError> {
    now.checked_sub_signed(duration)
        .ok_or_else(|| invalid(period))
}

fn invalid(period: &str) -> PeriodError {
    PeriodError::InvalidRange(period.to_string())
}

fn format_bound(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 16, 13, 45, 12).unwrap()
    }

    fn width_days(period: &str) -> i64 {
        let range = resolve(period, now()).unwrap();
        let from = DateTime::parse_from_rfc3339(&range.from).unwrap();
        let to = DateTime::parse_from_rfc3339(&range.to).unwrap();
        assert_eq!(to.to_utc(), now());
        (to - from).num_days()
    }

    #[test]
    fn period_any_day_preset_is_that_many_days_wide_ending_now() {
        assert_eq!(width_days("1d"), 1);
        assert_eq!(width_days("3d"), 3);
        assert_eq!(width_days("14d"), 14);
        assert_eq!(width_days("3650d"), 3650);
    }

    #[test]
    fn period_7d_still_starts_seven_days_earlier() {
        let range = resolve("7d", now()).unwrap();

        assert_eq!(range.from, "2026-07-09T13:45:12Z");
        assert_eq!(range.to, "2026-07-16T13:45:12Z");
    }

    #[test]
    fn period_zero_and_oversized_day_presets_are_errors() {
        assert_eq!(
            resolve("0d", now()),
            Err(PeriodError::InvalidRange("0d".to_string()))
        );
        assert_eq!(
            resolve("3651d", now()),
            Err(PeriodError::InvalidRange("3651d".to_string()))
        );
        assert_eq!(
            resolve("nonsense", now()),
            Err(PeriodError::Unknown("nonsense".to_string()))
        );
    }
}
