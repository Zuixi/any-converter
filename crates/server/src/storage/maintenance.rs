//! Maintenance: pruning by age, retention, vacuuming, checkpointing, and
//! rebuilding the rollup cache from the detail table.

use chrono::{Duration, SecondsFormat};

use super::{SqliteStorage, StorageError};

impl SqliteStorage {
    /// Number of `request_bodies` rows, for tests that prove the cascade ran.
    pub fn body_row_count(&self) -> Result<u64, StorageError> {
        let conn = self.pool.get()?;
        Ok(conn.query_row("select count(*) from request_bodies", [], |row| row.get(0))?)
    }

    /// Deletes request logs older than the given UTC ISO-8601 timestamp, with
    /// their bodies (foreign key cascade). Returns how many log rows went.
    pub fn prune_before(&self, cutoff: &str) -> Result<u64, StorageError> {
        let conn = self.pool.get()?;
        conn.execute("delete from request_logs where timestamp < ?1", [cutoff])?;
        Ok(conn.changes())
    }

    /// Keeps `retention_days` of history. A day count of 0 keeps everything.
    pub fn prune_older_than(
        &self,
        retention_days: u64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, StorageError> {
        if retention_days == 0 {
            return Ok(0);
        }
        self.prune_before(&age_cutoff(now, retention_days))
    }

    /// Deletes each layer's expired rows. Bodies go first so their pages are
    /// free before the cascade touches them again. `now` is explicit so the
    /// policy is testable.
    pub fn apply_retention(
        &self,
        policy: &RetentionPolicy,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<RetentionStats, StorageError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        let mut stats = RetentionStats::default();
        if policy.body_days != 0 {
            // A body with no timestamp (written before the column existed and never
            // backfilled) has no age to compare, so it is left alone.
            tx.execute(
                "delete from request_bodies where timestamp is not null and timestamp < ?1",
                [age_cutoff(now, policy.body_days)],
            )?;
            stats.bodies_deleted = tx.changes();
        }
        if policy.log_days != 0 {
            tx.execute(
                "delete from request_logs where timestamp < ?1",
                [age_cutoff(now, policy.log_days)],
            )?;
            stats.logs_deleted = tx.changes();
        }
        if policy.rollup_days != 0 {
            // A rollup row covers a whole hour, so the cutoff floors to its hour: an
            // hour that still contains a live instant is not expired yet.
            tx.execute(
                "delete from rollup_hourly where bucket < ?1",
                [hour_floor_cutoff(now, policy.rollup_days)],
            )?;
            stats.rollup_deleted = tx.changes();
        }
        tx.commit()?;
        Ok(stats)
    }

    /// Refreshes the query planner's statistics (`pragma optimize`). Run it
    /// periodically, not per query.
    pub fn optimize(&self) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        // `optimize` may return rows on some builds; `execute_batch` ignores them.
        conn.execute_batch("pragma optimize")?;
        Ok(())
    }

    /// Runs `pragma incremental_vacuum` in bounded steps so it never holds the
    /// write lock for long. Returns how many pages were released.
    ///
    /// Only does anything on a database opened after the `auto_vacuum` conversion
    /// in `schema::migrate`; on the old `auto_vacuum = none` files this is a no-op
    /// until that one-off `VACUUM` has run.
    pub fn incremental_vacuum(&self, max_pages: u32) -> Result<u64, StorageError> {
        let conn = self.pool.get()?;
        let freelist_before: u64 = conn.query_row("pragma freelist_count", [], |row| row.get(0))?;
        // `execute_batch` stops reading a statement at its first row, and this pragma
        // yields one row per released page, so it must be stepped to completion.
        let mut stmt = conn.prepare(&format!("pragma incremental_vacuum({max_pages})"))?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        let freelist_after: u64 = conn.query_row("pragma freelist_count", [], |row| row.get(0))?;
        Ok(freelist_before.saturating_sub(freelist_after))
    }

    /// Folds the write-ahead log back into the database file and truncates it, so
    /// the on-disk size reflects what is actually stored. Callers that measure or
    /// ship the file (a backup, an operator checking disk use) need this; the
    /// background sweep calls it after a retention pass.
    pub fn checkpoint(&self) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("pragma wal_checkpoint(truncate)", [], |row| {
            row.get::<_, i64>(0)
        })?;
        Ok(())
    }

    /// Recomputes `rollup_hourly` from `request_logs` in one transaction.
    /// The rollup is a cache; this is the only way back to the truth.
    pub fn rebuild_rollup(&self) -> Result<u64, StorageError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;
        tx.execute("delete from rollup_hourly", [])?;
        // Same column expressions as `upsert_rollup`, so a rebuilt row is
        // indistinguishable from one the write path folded.
        tx.execute(
            r#"
            insert into rollup_hourly (
                bucket, provider, client_model, agent,
                calls, errors, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, cache_write_1h,
                cache_unknown_tokens, reasoning_tokens,
                latency_sum_ms, latency_max_ms, ttft_sum_ms, ttft_calls, cost_usd,
                priced_calls, retried_calls, loops,
                decode_ms, decode_out, decode_n
            )
            select
                substr(timestamp,1,13) || ':00:00Z', provider, client_model, coalesce(client_id,''),
                count(*),
                sum(case when response_status >= 400 then 1 else 0 end),
                sum(input_tokens), sum(output_tokens),
                sum(coalesce(cache_read_tokens,0)), sum(coalesce(cache_write_tokens,0)),
                sum(cache_write_1h),
                sum(case when cache_unknown = 1
                         then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
                         else 0 end),
                sum(coalesce(reasoning_tokens,0)),
                sum(latency_ms), max(latency_ms),
                sum(ttft_ms), sum(case when ttft_ms > 0 then 1 else 0 end),
                sum(coalesce(cost_usd,0)),
                sum(case when cost_usd is not null then 1 else 0 end),
                sum(case when attempt > 0 and response_status < 400 then 1 else 0 end),
                sum(case when err_type = 'loop' then 1 else 0 end),
                -- `speed()` divides these two sums, so every millisecond of a
                -- decode window adds to the numerator the same way it adds to the
                -- denominator; `decode_n` only counts the rows with both sides.
                sum(decode_ms), sum(decode_out),
                sum(case when decode_out > 0 and decode_ms > 0 then 1 else 0 end)
            from request_logs
            group by 1, 2, 3, 4
            "#,
            [],
        )?;
        let written = tx.changes();
        tx.commit()?;
        Ok(written)
    }
}

/// How long each layer keeps history. `0` keeps that layer forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub rollup_days: u64,
    pub log_days: u64,
    pub body_days: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            rollup_days: 0,
            log_days: 30,
            body_days: 7,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionStats {
    pub bodies_deleted: u64,
    pub logs_deleted: u64,
    pub rollup_deleted: u64,
}

/// The ISO-8601 instant `days` before `now`, in the second-precision form the
/// `timestamp` columns are written in, so `<` compares as text.
fn age_cutoff(now: chrono::DateTime<chrono::Utc>, days: u64) -> String {
    (now - Duration::days(days as i64)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Same cutoff, truncated to its hour: `rollup_hourly.bucket` is `...T{HH}:00:00Z`.
fn hour_floor_cutoff(now: chrono::DateTime<chrono::Utc>, days: u64) -> String {
    (now - Duration::days(days as i64))
        .format("%Y-%m-%dT%H:00:00Z")
        .to_string()
}
