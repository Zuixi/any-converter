use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OpenFlags, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::request_log::RequestLogRecord;

mod ledger;
mod maintenance;
mod query;
mod schema;
mod write;

use ledger::{TOTALS_AGGREGATES, totals_at};

pub use ledger::{LedgerBy, LedgerPage, LedgerRow, Share};
pub use maintenance::{RetentionPolicy, RetentionStats};
pub use query::{
    Bucket, Granularity, Group, HEATMAP_WEEKS, HeatDay, Heatmap, LedgerFilter, SERIES_KEEP,
    SplitDim, Totals, UsageQuery, UsageSummary,
};

/// The rollup's aggregate columns after `calls`, in [`Totals`] order, so
/// `totals_at` reads a rollup row exactly like a detail row.
const ROLLUP_AGGREGATES: &str = "\
    sum(errors),
    sum(input_tokens), sum(output_tokens),
    sum(cache_read_tokens), sum(cache_write_tokens),
    sum(cache_write_1h), sum(cache_unknown_tokens),
    sum(reasoning_tokens),
    sum(latency_sum_ms), max(latency_max_ms),
    sum(ttft_sum_ms), sum(ttft_calls),
    sum(cost_usd), sum(priced_calls),
    sum(retried_calls), sum(loops)";

/// The decode aggregates appended after `ROLLUP_AGGREGATES` in the summary's
/// grouped select, so one row carries a speed as well as its token sums.
const ROLLUP_DECODE_AGGREGATES: &str = "sum(decode_ms), sum(decode_out), sum(decode_n)";

/// The same three from the detail table, where `decode_n` is not a column: it is
/// the count `upsert_rollup` folds, kept identical so a detail read and a rollup
/// read of the same rows report the same speed.
const TOTALS_DECODE_AGGREGATES: &str = "\
    sum(decode_ms), sum(decode_out), \
    sum(case when decode_out > 0 and decode_ms > 0 then 1 else 0 end)";

/// Index of the first decode aggregate in a summary row: bucket, provider, model,
/// agent, `calls`, then the 16 aggregates above.
const DECODE_OFFSET: usize = 21;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlyUsage {
    pub timestamp: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
    pub status: u16,
    pub latency_ms: u64,
    pub avg_latency_ms: u64,
    pub max_latency_ms: u64,
    pub error_count: u64,
    pub provider: String,
    pub client_model: String,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite connection pool error: {0}")]
    Pool(#[from] r2d2::Error),
}

impl StorageError {
    /// Whether the failure is only that this database has no request log table
    /// yet, which a reader opened before the proxy served anything meets. Any
    /// other error (a corrupt file, a failing disk) must still be reported: a
    /// silent empty list makes "why are my logs gone" impossible to diagnose.
    pub fn is_missing_table(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no such table")
        )
    }
}

#[derive(Clone)]
pub struct SqliteStorage {
    pub(super) pool: Pool<SqliteConnectionManager>,
}

impl SqliteStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let pool = build_pool(SqliteConnectionManager::file(path), PoolMode::ReadWrite)?;
        let mut conn = pool.get()?;
        schema::migrate(&mut conn)?;
        drop(conn);
        Ok(Self { pool })
    }

    pub fn open_in_log_dir(log_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::open(log_dir.as_ref().join("any-converter.sqlite3"))
    }

    /// Open an existing log database for concurrent reads while the server may be writing.
    ///
    /// Skips schema migration/`journal_mode` changes so it does not fight the writer lock.
    pub fn open_readonly_in_log_dir(log_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = log_dir.as_ref().join("any-converter.sqlite3");
        let manager =
            SqliteConnectionManager::file(path).with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY);
        Ok(Self {
            pool: build_pool(manager, PoolMode::ReadOnly)?,
        })
    }

    /// Current schema version of the database (`pragma user_version`).
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        let conn = self.pool.get()?;
        Ok(conn.query_row("pragma user_version", [], |row| row.get(0))?)
    }
    pub fn recent_request_logs(&self, limit: u64) -> Result<Vec<RequestLogRecord>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            r#"
            select b.record_json
            from request_logs l
            join request_bodies b on b.request_id = l.request_id
            order by l.timestamp desc, l.id desc
            limit ?1
            "#,
        )?;
        let rows = stmt.query_map([to_i64(limit)], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    pub fn hourly_usage_from_request_logs(
        &self,
        limit: u64,
    ) -> Result<Vec<HourlyUsage>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            r#"
            select
                hour,
                input_tokens,
                output_tokens,
                total_tokens,
                request_count,
                status,
                avg_latency_ms,
                max_latency_ms,
                error_count,
                provider,
                client_model
            from (
                select
                    strftime('%Y-%m-%dT%H:00:00Z', timestamp) as hour,
                    sum(input_tokens) as input_tokens,
                    sum(output_tokens) as output_tokens,
                    sum(total_tokens) as total_tokens,
                    count(*) as request_count,
                    max(response_status) as status,
                    cast(round(avg(latency_ms)) as integer) as avg_latency_ms,
                    max(latency_ms) as max_latency_ms,
                    sum(case when response_status >= 400 then 1 else 0 end) as error_count,
                    min(provider) as provider,
                    min(client_model) as client_model
                from request_logs
                group by hour
                order by hour desc
                limit ?1
            )
            order by hour asc
            "#,
        )?;
        let rows = stmt.query_map([to_i64(limit)], |row| {
            let avg_latency_ms = row.get::<_, u64>(6)?;
            Ok(HourlyUsage {
                timestamp: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
                status: row.get(5)?,
                latency_ms: avg_latency_ms,
                avg_latency_ms,
                max_latency_ms: row.get(7)?,
                error_count: row.get(8)?,
                provider: row.get(9)?,
                client_model: row.get(10)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Aggregates `rollup_hourly` over `q` with ONE grouped query, folded in Rust.
    ///
    /// The grouping key keeps provider, model, and agent apart, so a busy hour
    /// never attributes one provider's tokens to another. The rollup is a cache
    /// maintained on insert; [`SqliteStorage::rebuild_rollup`] is the way back to
    /// the detail table when it drifts.
    ///
    /// `rollup_hourly` has no account dimension, so an account filter reads the
    /// detail rows instead: folding a whole rollup row would include every
    /// account that shared the bucket.
    pub fn usage_summary(&self, q: &UsageQuery) -> Result<UsageSummary, StorageError> {
        let conn = self.pool.get()?;

        let mut totals = Totals::default();
        // Buckets are ordered by their ISO timestamp key, which the map sorts for us.
        let mut series: BTreeMap<String, Totals> = BTreeMap::new();
        let mut by_provider: HashMap<String, GroupTotals> = HashMap::new();
        let mut by_model: HashMap<String, GroupTotals> = HashMap::new();
        let mut by_agent: HashMap<String, GroupTotals> = HashMap::new();

        if q.account.is_some() {
            // The detail table has the account column the rollup lacks.
            let sql = format!(
                r#"
                select
                    {} as bucket, provider, client_model, coalesce(client_id,'') as agent,
                    count(*), {TOTALS_AGGREGATES}, {TOTALS_DECODE_AGGREGATES}
                from request_logs
                where timestamp >= ?1 and timestamp < ?2
                  and (?3 is null or provider = ?3)
                  and (?4 is null or client_model = ?4)
                  and (?5 is null or coalesce(client_id,'') = ?5)
                  and provider_account = ?6
                group by 1, 2, 3, 4
                "#,
                q.granularity.bucket_expr("timestamp")
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(params![
                q.from,
                q.to,
                q.provider.as_deref(),
                q.model.as_deref(),
                q.agent.as_deref(),
                q.account.as_deref(),
            ])?;
            while let Some(row) = rows.next()? {
                fold_summary_row(
                    row,
                    &mut totals,
                    &mut series,
                    &mut by_provider,
                    &mut by_model,
                    &mut by_agent,
                )?;
            }
        } else {
            let sql = format!(
                r#"
                select
                    {} as bucket, provider, client_model, agent,
                    sum(calls), {ROLLUP_AGGREGATES}, {ROLLUP_DECODE_AGGREGATES}
                from rollup_hourly
                where bucket >= ?1 and bucket < ?2
                  and (?3 is null or provider = ?3)
                  and (?4 is null or client_model = ?4)
                  and (?5 is null or agent = ?5)
                group by 1, 2, 3, 4
                "#,
                q.granularity.bucket_expr("bucket")
            );
            let mut stmt = conn.prepare(&sql)?;
            // A rollup row covers a whole hour, so a `from` inside an hour must round
            // down to that hour or the first partial one would be dropped.
            let from = q.granularity.hour_floor(&q.from);
            let mut rows = stmt.query(params![
                from,
                q.to,
                q.provider.as_deref(),
                q.model.as_deref(),
                q.agent.as_deref(),
            ])?;
            while let Some(row) = rows.next()? {
                fold_summary_row(
                    row,
                    &mut totals,
                    &mut series,
                    &mut by_provider,
                    &mut by_model,
                    &mut by_agent,
                )?;
            }
        }

        // Only a caller that asked for a stack pays for the second grouping.
        let split = match q.split_by {
            Some(dim) => series_split(&conn, q, dim)?,
            None => None,
        };

        let series = series
            .into_iter()
            .map(|(timestamp, totals)| Bucket {
                label: timestamp.clone(),
                // A kept value with no calls in this bucket is simply absent;
                // an empty map is a point whose values all fell outside the keep.
                by: split
                    .as_ref()
                    .map(|split| split.get(&timestamp).cloned().unwrap_or_default()),
                timestamp,
                totals,
            })
            .collect();

        Ok(UsageSummary {
            from: q.from.clone(),
            to: q.to.clone(),
            granularity: q.granularity,
            totals,
            series,
            by_provider: sorted_groups(by_provider),
            by_model: sorted_groups(by_model),
            by_agent: sorted_groups(by_agent),
            by_account: account_groups(&conn, q)?,
            by_session: session_groups(&conn, q)?,
        })
    }

    /// The last 53 weeks of calls the filter keeps, a day each. Read from
    /// `rollup_hourly`: one cell is one `substr(bucket,1,10)` group.
    ///
    /// The span is the heatmap's own — `q.from`/`q.to` are ignored, because a
    /// calendar whose first column moved with the ledger's range would stop being
    /// a calendar. The provider/model/agent filters still narrow the cells. `q.account`
    /// has no column in the rollup: honouring it would mean re-reading
    /// `request_logs`, which this view is specifically not.
    pub fn heatmap(&self, q: &UsageQuery) -> Result<Heatmap, StorageError> {
        let conn = self.pool.get()?;
        // One instant for both ends, so a clock crossing midnight cannot make the
        // window end on a day its own start never covered.
        let now = chrono::Utc::now();
        let today = now.date_naive();
        // 52 whole weeks behind this one's Monday, so the current week keeps its
        // own column and the calendar is exactly `HEATMAP_WEEKS` columns wide.
        let from = monday_of(today) - chrono::Duration::weeks(i64::from(HEATMAP_WEEKS - 1));
        let from = from.to_string();
        // `bucket` is `YYYY-MM-DDTHH:00:00Z`, so it sorts against a second-precision
        // RFC 3339 instant; the hour in progress is included, later rows are not.
        let until = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let mut stmt = conn.prepare(
            r#"
            select
                substr(bucket,1,10) as day,
                sum(calls),
                sum(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens),
                sum(cost_usd),
                sum(calls - priced_calls)
            from rollup_hourly
            where bucket >= ?1 and bucket <= ?2
              and (?3 is null or provider = ?3)
              and (?4 is null or client_model = ?4)
              and (?5 is null or agent = ?5)
            group by 1
            having sum(calls) > 0
            order by 1 asc
            "#,
        )?;
        let mut rows = stmt.query(params![
            from,
            until,
            q.provider.as_deref(),
            q.model.as_deref(),
            q.agent.as_deref(),
        ])?;
        let mut days = Vec::new();
        while let Some(row) = rows.next()? {
            // Every column summed here is NOT NULL on the rollup, so a group's
            // aggregate is never NULL and can be read as a plain integer.
            days.push(HeatDay {
                date: row.get(0)?,
                calls: row.get::<_, i64>(1)?.max(0) as u64,
                tokens: row.get::<_, i64>(2)?.max(0) as u64,
                cost_usd: row.get(3)?,
                unpriced: row.get::<_, i64>(4)?.max(0) as u64,
            });
        }

        Ok(Heatmap {
            from,
            to: today.to_string(),
            weeks: HEATMAP_WEEKS,
            days,
        })
    }
}

/// The per-point split of the series: bucket → value → that value's totals.
///
/// `None` when the dimension cannot be split at all (`Account` is not on the
/// rollup) or when no value survived the empty check, so the caller leaves every
/// [`Bucket::by`] at `None` instead of sending a pile of empty maps.
fn series_split(
    conn: &rusqlite::Connection,
    q: &UsageQuery,
    dim: SplitDim,
) -> Result<Option<BTreeMap<String, BTreeMap<String, Totals>>>, StorageError> {
    // `rollup_hourly` is keyed by (bucket, provider, client_model, agent), so a
    // split by provider, agent, or model is that same grouping with one more
    // column. An account is not pre-aggregated: the rollup has no
    // `provider_account` column, and adding one would turn an hourly cache into a
    // per-subscription table, so the account split is left out here.
    let Some(column) = dim.rollup_column() else {
        return Ok(None);
    };

    // An account filter reads `request_logs` in `usage_summary` (the rollup has no
    // account column); the split must read the same rows or it would hand the
    // selected account every other account's traffic in the same bucket.
    let (sql, from) = if q.account.is_some() {
        (
            format!(
                r#"
                select {column} as value, {} as bucket, count(*), {TOTALS_AGGREGATES}
                from request_logs
                where timestamp >= ?1 and timestamp < ?2
                  and (?3 is null or provider = ?3)
                  and (?4 is null or client_model = ?4)
                  and (?5 is null or coalesce(client_id,'') = ?5)
                  and provider_account = ?6
                group by 1, 2
                "#,
                q.granularity.bucket_expr("timestamp")
            ),
            q.from.clone(),
        )
    } else {
        (
            format!(
                r#"
                select {column} as value, {} as bucket, sum(calls), {ROLLUP_AGGREGATES}
                from rollup_hourly
                where bucket >= ?1 and bucket < ?2
                  and (?3 is null or provider = ?3)
                  and (?4 is null or client_model = ?4)
                  and (?5 is null or agent = ?5)
                group by 1, 2
                "#,
                q.granularity.bucket_expr("bucket")
            ),
            // The same hour floor the main rollup query applies, or a partial
            // first hour would be in `totals` but missing from `by`.
            q.granularity.hour_floor(&q.from),
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&from, &q.to, &q.provider, &q.model, &q.agent];
    // Only the detail query has the `?6` account predicate.
    if q.account.is_some() {
        bind.push(&q.account);
    }
    let mut rows = stmt.query(bind.as_slice())?;

    let mut split: BTreeMap<String, BTreeMap<String, Totals>> = BTreeMap::new();
    // Tokens over the whole range decide which values survive `SERIES_KEEP`.
    let mut weight: HashMap<String, u64> = HashMap::new();
    while let Some(row) = rows.next()? {
        let value: String = row.get(0)?;
        // A value with no name has no line to draw: it stays in the point totals.
        if value.is_empty() {
            continue;
        }
        let bucket: String = row.get(1)?;
        let calls: u64 = row.get(2)?;
        let row_totals = totals_at(row, 3, calls)?;
        *weight.entry(value.clone()).or_default() +=
            row_totals.input_tokens + row_totals.output_tokens;
        add_totals(
            split.entry(bucket).or_default().entry(value).or_default(),
            &row_totals,
        );
    }

    // Rank once over the whole range: a value that is busy in one bucket only is
    // still one line, so it must not outrank a steady one by a single spike.
    let mut ranked: Vec<(String, u64)> = weight.into_iter().collect();
    ranked.sort_by(|(value_a, weight_a), (value_b, weight_b)| {
        weight_b.cmp(weight_a).then_with(|| value_a.cmp(value_b))
    });
    ranked.truncate(SERIES_KEEP);
    let kept: HashSet<String> = ranked.into_iter().map(|(value, _)| value).collect();
    if kept.is_empty() {
        return Ok(None);
    }
    split.retain(|_, values| {
        values.retain(|value, _| kept.contains(value));
        !values.is_empty()
    });
    Ok(Some(split))
}

/// The Monday that starts `d`'s week, in UTC. A Sunday belongs to the week that
/// began six days before it, which is why `weekday 1` alone is not enough.
fn monday_of(d: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    d - chrono::Duration::days(i64::from(d.weekday().num_days_from_monday()))
}

/// Folds one grouped row shaped like the rollup or detail select into the summary.
fn fold_summary_row(
    row: &rusqlite::Row<'_>,
    totals: &mut Totals,
    series: &mut BTreeMap<String, Totals>,
    by_provider: &mut HashMap<String, GroupTotals>,
    by_model: &mut HashMap<String, GroupTotals>,
    by_agent: &mut HashMap<String, GroupTotals>,
) -> rusqlite::Result<()> {
    let bucket: String = row.get(0)?;
    let provider: String = row.get(1)?;
    let model: String = row.get(2)?;
    let agent: String = row.get(3)?;
    let calls: u64 = row.get(4)?;
    let mut row_totals = totals_at(row, 5, calls)?;
    // The shared aggregates stop before the decode columns; the three appended
    // after them are what let `speed()` answer from a summary instead of the ledger.
    row_totals.decode_ms = u64_column(row, DECODE_OFFSET)?;
    row_totals.decode_out = u64_column(row, DECODE_OFFSET + 1)?;
    row_totals.decode_n = u64_column(row, DECODE_OFFSET + 2)?;

    add_totals(totals, &row_totals);
    add_totals(series.entry(bucket).or_default(), &row_totals);

    let provider_group = by_provider
        .entry(provider.clone())
        .or_insert_with(|| GroupTotals {
            provider: Some(provider.clone()),
            ..GroupTotals::default()
        });
    add_totals(&mut provider_group.totals, &row_totals);

    match by_model.entry(model.clone()) {
        Entry::Vacant(slot) => {
            let group = slot.insert(GroupTotals {
                provider: Some(provider),
                model: Some(model),
                totals: Totals::default(),
            });
            add_totals(&mut group.totals, &row_totals);
        }
        Entry::Occupied(mut slot) => {
            let group = slot.get_mut();
            // A model served by several providers has no single provider to name.
            if group.provider.as_deref() != Some(provider.as_str()) {
                group.provider = None;
            }
            add_totals(&mut group.totals, &row_totals);
        }
    }

    add_totals(&mut by_agent.entry(agent).or_default().totals, &row_totals);
    Ok(())
}

/// Most calls a detail-table dimension returns; the tail is not worth the round trip.
const MAX_DIMENSION_ROWS: usize = 50;

/// Accounts rank from `request_logs`: the rollup has no account dimension.
fn account_groups(conn: &rusqlite::Connection, q: &UsageQuery) -> Result<Vec<Group>, StorageError> {
    let sql = format!(
        r#"
        select coalesce(provider_account,''), provider, count(*), {TOTALS_AGGREGATES}
        from request_logs
        where timestamp >= ?1 and timestamp < ?2
          and (?3 is null or provider = ?3)
          and (?4 is null or client_model = ?4)
          and (?5 is null or coalesce(client_id,'') = ?5)
          and provider_account is not null and provider_account <> ''
          and (?6 is null or provider_account = ?6)
        group by 1, 2
        order by sum(input_tokens + output_tokens) desc, 1 asc, 2 asc
        limit {MAX_DIMENSION_ROWS}
        "#
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![
        q.from,
        q.to,
        q.provider.as_deref(),
        q.model.as_deref(),
        q.agent.as_deref(),
        q.account.as_deref(),
    ])?;
    let mut groups = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let provider: String = row.get(1)?;
        let calls: u64 = row.get(2)?;
        groups.push(Group {
            id,
            provider: Some(provider),
            model: None,
            totals: totals_at(row, 3, calls)?,
        });
    }
    Ok(groups)
}

/// Sessions rank from `request_logs`; they are far too many for the rollup.
fn session_groups(conn: &rusqlite::Connection, q: &UsageQuery) -> Result<Vec<Group>, StorageError> {
    let sql = format!(
        r#"
        select session_id, coalesce(client_id,''), count(*), {TOTALS_AGGREGATES}
        from request_logs
        where timestamp >= ?1 and timestamp < ?2
          and (?3 is null or provider = ?3)
          and (?4 is null or client_model = ?4)
          and (?5 is null or coalesce(client_id,'') = ?5)
          and session_id is not null and session_id <> ''
        group by 1, 2
        order by sum(input_tokens + output_tokens) desc, 1 asc, 2 asc
        limit {MAX_DIMENSION_ROWS}
        "#
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![
        q.from,
        q.to,
        q.provider.as_deref(),
        q.model.as_deref(),
        q.agent.as_deref(),
    ])?;
    let mut groups = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let agent: String = row.get(1)?;
        let calls: u64 = row.get(2)?;
        groups.push(Group {
            id,
            provider: None,
            model: Some(agent),
            totals: totals_at(row, 3, calls)?,
        });
    }
    Ok(groups)
}

#[derive(Default)]
struct GroupTotals {
    provider: Option<String>,
    model: Option<String>,
    totals: Totals,
}

fn add_totals(into: &mut Totals, from: &Totals) {
    into.calls += from.calls;
    into.errors += from.errors;
    into.input_tokens += from.input_tokens;
    into.output_tokens += from.output_tokens;
    into.cache_read_tokens += from.cache_read_tokens;
    into.cache_write_tokens += from.cache_write_tokens;
    into.cache_write_1h += from.cache_write_1h;
    into.cache_unknown_tokens += from.cache_unknown_tokens;
    into.reasoning_tokens += from.reasoning_tokens;
    into.latency_sum_ms += from.latency_sum_ms;
    into.latency_max_ms = into.latency_max_ms.max(from.latency_max_ms);
    into.ttft_sum_ms += from.ttft_sum_ms;
    into.ttft_calls += from.ttft_calls;
    into.cost_usd += from.cost_usd;
    into.unpriced += from.unpriced;
    into.retried += from.retried;
    into.loops += from.loops;
    into.decode_ms += from.decode_ms;
    into.decode_out += from.decode_out;
    into.decode_n += from.decode_n;
}

/// A nullable integer column as a non-negative `u64`; `sum()` over no rows is NULL.
fn u64_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, Option<i64>>(index)?.unwrap_or(0).max(0) as u64)
}

/// Busiest groups first; `id` breaks ties so the order is stable across runs.
fn sorted_groups(groups: HashMap<String, GroupTotals>) -> Vec<Group> {
    let mut groups = groups
        .into_iter()
        .map(|(id, group)| Group {
            id,
            provider: group.provider,
            model: group.model,
            totals: group.totals,
        })
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        b.totals
            .calls
            .cmp(&a.totals.calls)
            .then_with(|| a.id.cmp(&b.id))
    });
    groups
}

/// Which pragmas a freshly opened pooled connection receives.
///
/// Pragmas are per-connection state, not database state, so a pragma set once on
/// the connection that ran the migrations does not reach the connections the pool
/// opens later — and `foreign_keys = off` there silently disables the
/// `request_bodies` cascade for every prune.
#[derive(Clone, Copy)]
enum PoolMode {
    /// The single writer.
    ReadWrite,
    /// A `SQLITE_OPEN_READ_ONLY` connection: `journal_mode` may not be changed
    /// from one, so only the pragmas that are legal on a reader go here.
    ReadOnly,
}

impl PoolMode {
    fn pragmas(self) -> &'static str {
        match self {
            Self::ReadWrite => {
                r#"
                pragma journal_mode = wal;
                pragma synchronous = normal;
                pragma foreign_keys = on;
                pragma busy_timeout = 5000;
                "#
            }
            Self::ReadOnly => {
                r#"
                pragma foreign_keys = on;
                pragma busy_timeout = 5000;
                "#
            }
        }
    }
}

fn build_pool(
    manager: SqliteConnectionManager,
    mode: PoolMode,
) -> Result<Pool<SqliteConnectionManager>, r2d2::Error> {
    // `with_init` runs on every connection this pool creates, not just the first.
    let manager = manager.with_init(move |conn| conn.execute_batch(mode.pragmas()));
    Pool::builder().max_size(4).build(manager)
}

pub fn open_sqlite_storage_for_log_dir(log_dir: Option<&str>) -> Option<SqliteStorage> {
    let dir = log_dir?;
    let path = PathBuf::from(dir);
    match SqliteStorage::open_in_log_dir(path) {
        Ok(storage) => Some(storage),
        Err(e) => {
            log::error!("failed to initialize sqlite log storage: {e}");
            None
        }
    }
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
