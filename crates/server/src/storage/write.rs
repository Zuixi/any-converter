//! The write path: one request log record into its narrow row, its body payload,
//! and the hourly rollup it folds into.

use rusqlite::{TransactionBehavior, params};

use crate::request_log::RequestLogRecord;

use super::{SqliteStorage, StorageError, to_i64};

impl SqliteStorage {
    pub fn insert_request_log(&self, record: &RequestLogRecord) -> Result<(), StorageError> {
        self.insert_request_logs(std::slice::from_ref(record))
    }

    /// Inserts many records in ONE transaction. A record that fails is logged and
    /// skipped; its neighbours still commit.
    pub fn insert_request_logs(&self, records: &[RequestLogRecord]) -> Result<(), StorageError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.get()?;
        // `immediate` takes the write lock at BEGIN, where `busy_timeout` covers it.
        // A deferred transaction would instead read first and then fail its upgrade
        // with SQLITE_BUSY the moment another pooled connection commits, dropping
        // every record in the batch.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for record in records {
            // One malformed row must not roll back the neighbours sharing this transaction.
            if let Err(error) = insert_request_log_row(&tx, record) {
                log::error!(
                    "failed to insert request log record {}: {error}",
                    record.request_id
                );
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Writes one record's narrow row and wide payload on `tx`. The narrow row and the
/// wide payload must land together.
fn insert_request_log_row(
    tx: &rusqlite::Transaction<'_>,
    record: &RequestLogRecord,
) -> Result<(), StorageError> {
    let record_json = serde_json::to_string(record)?;
    let total_tokens = record.usage.input_tokens + record.usage.output_tokens;
    // A replay of the same `request_id` must not double-add its tokens to
    // `rollup_hourly`. `changes()` cannot tell an upsert's insert branch from its
    // update branch (both report 1), so probe for the row first. Trade-off: the
    // replay still refreshes the detail row (its tokens may have been corrected)
    // but leaves the previous rollup contribution in place, which can go stale.
    // `rebuild_rollup` is the way to reconcile that.
    let existed: bool = tx.query_row(
        "select exists(select 1 from request_logs where request_id = ?1)",
        [&record.request_id],
        |row| row.get(0),
    )?;
    tx.execute(
        r#"
            insert into request_logs (
                request_id, timestamp, client_format, provider, client_model,
                upstream_model, streaming, method, path,
                response_status, latency_ms, input_tokens, output_tokens,
                total_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
                truncated, client_id, session_id, provider_account, served_model,
                ttft_ms, cache_unknown, cache_write_1h, decode_ms, decode_out,
                cost_usd, price_version,
                attempt, handed_on, error, err_type,
                own_ms, sent_ms, retried_ms, queued_ms, masking_ms, flow_ms,
                upstream, effort, tier_asked, tier_sent, tier_served,
                subagent, parent_agent, kind
            ) values (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25,
                ?26, ?27, ?28, ?29,
                ?30, ?31, ?32, ?33,
                ?34, ?35, ?36, ?37,
                ?38, ?39, ?40, ?41,
                ?42, ?43, ?44, ?45,
                ?46, ?47
            )
            on conflict(request_id) do update set
                timestamp = excluded.timestamp,
                client_format = excluded.client_format,
                provider = excluded.provider,
                client_model = excluded.client_model,
                upstream_model = excluded.upstream_model,
                streaming = excluded.streaming,
                method = excluded.method,
                path = excluded.path,
                response_status = excluded.response_status,
                latency_ms = excluded.latency_ms,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                total_tokens = excluded.total_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                cache_write_tokens = excluded.cache_write_tokens,
                reasoning_tokens = excluded.reasoning_tokens,
                truncated = excluded.truncated,
                client_id = excluded.client_id,
                session_id = excluded.session_id,
                provider_account = excluded.provider_account,
                served_model = excluded.served_model,
                ttft_ms = excluded.ttft_ms,
                cache_unknown = excluded.cache_unknown,
                cache_write_1h = excluded.cache_write_1h,
                decode_ms = excluded.decode_ms,
                decode_out = excluded.decode_out,
                cost_usd = excluded.cost_usd,
                price_version = excluded.price_version,
                attempt = excluded.attempt,
                handed_on = excluded.handed_on,
                error = excluded.error,
                err_type = excluded.err_type,
                own_ms = excluded.own_ms,
                sent_ms = excluded.sent_ms,
                retried_ms = excluded.retried_ms,
                queued_ms = excluded.queued_ms,
                masking_ms = excluded.masking_ms,
                flow_ms = excluded.flow_ms,
                upstream = excluded.upstream,
                effort = excluded.effort,
                tier_asked = excluded.tier_asked,
                tier_sent = excluded.tier_sent,
                tier_served = excluded.tier_served,
                subagent = excluded.subagent,
                parent_agent = excluded.parent_agent,
                kind = excluded.kind
            "#,
        params![
            record.request_id,
            record.timestamp,
            record.client_format,
            record.provider,
            record.client_model,
            record.upstream_model,
            record.streaming,
            record.method,
            record.path,
            record.response_status,
            to_i64(record.latency_ms),
            to_i64(record.usage.input_tokens),
            to_i64(record.usage.output_tokens),
            to_i64(total_tokens),
            record.usage.cache_read_tokens.map(to_i64),
            record.usage.cache_write_tokens.map(to_i64),
            record.usage.reasoning_tokens.map(to_i64),
            record.truncated,
            record.client_id,
            record.session_id,
            record.provider_account,
            record.served_model,
            to_i64(record.ttft_ms),
            record.cache_unknown as i64,
            to_i64(record.usage.cache_write_1h.unwrap_or(0)),
            to_i64(record.decode_ms),
            to_i64(record.decode_out),
            record.cost_usd,
            record.price_version,
            record.attempt,
            record.handed_on,
            record.error,
            record.err_type,
            to_i64(record.own_ms),
            to_i64(record.sent_ms),
            to_i64(record.retried_ms),
            to_i64(record.queued_ms),
            to_i64(record.masking_ms),
            to_i64(record.flow_ms),
            record.upstream,
            record.effort,
            record.tier_asked,
            record.tier_sent,
            record.tier_served,
            record.subagent,
            record.parent_agent,
            record.kind,
        ],
    )?;
    // The body carries its own copy of the timestamp: bodies age on a shorter
    // window than logs, and a delete by age must not join the narrow table.
    tx.execute(
        r#"
            insert into request_bodies (request_id, record_json, timestamp)
            values (?1, ?2, ?3)
            on conflict(request_id) do update set
                record_json = excluded.record_json,
                timestamp = excluded.timestamp
            "#,
        params![record.request_id, record_json, record.timestamp],
    )?;
    // Same transaction: a log row and its pre-aggregate land together or not at all.
    // A replay keeps the detail row updated but leaves the rollup alone, since the
    // rollup cannot un-add what it already counted for this `request_id`.
    if !existed {
        upsert_rollup(tx, record)?;
    }
    Ok(())
}

/// Folds one record into its `rollup_hourly` bucket so usage views never group over
/// the whole log table. Every numeric column accumulates; the latency high-water mark
/// keeps the larger side.
fn upsert_rollup(
    tx: &rusqlite::Transaction<'_>,
    record: &RequestLogRecord,
) -> Result<(), StorageError> {
    // `get` also declines a slice that is not a char boundary, so a mangled timestamp
    // becomes its own bucket instead of panicking mid-batch.
    let bucket = match record.timestamp.get(..13) {
        Some(prefix) => format!("{prefix}:00:00Z"),
        None => record.timestamp.clone(),
    };
    let agent = record.client_id.clone().unwrap_or_default();
    let cache_read_tokens = record.usage.cache_read_tokens.unwrap_or(0);
    let cache_write_tokens = record.usage.cache_write_tokens.unwrap_or(0);
    let cache_unknown_tokens = if record.cache_unknown {
        record.usage.input_tokens + cache_read_tokens + cache_write_tokens
    } else {
        0
    };
    // Whether this call had a known price, as the integer the rollup counts. A
    // bucket can mix the two: `cost_usd` then covers `priced_calls` of `calls`.
    let priced_calls: i64 = i64::from(record.cost_usd.is_some());
    // A decode window counts only when both sides exist: `speed()` divides the
    // summed tokens by the summed milliseconds, so a row with no output would
    // otherwise inflate the denominator without contributing a reply.
    let decode_n: i64 = i64::from(record.decode_out > 0 && record.decode_ms > 0);
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
            ) values (
                ?1, ?2, ?3, ?4,
                1, ?5, ?6, ?7,
                ?8, ?9, ?10,
                ?11, ?12,
                ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20,
                ?21, ?22, ?23
            )
            on conflict(bucket, provider, client_model, agent) do update set
                calls = calls + excluded.calls,
                errors = errors + excluded.errors,
                input_tokens = input_tokens + excluded.input_tokens,
                output_tokens = output_tokens + excluded.output_tokens,
                cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
                cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens,
                cache_write_1h = cache_write_1h + excluded.cache_write_1h,
                cache_unknown_tokens = cache_unknown_tokens + excluded.cache_unknown_tokens,
                reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
                latency_sum_ms = latency_sum_ms + excluded.latency_sum_ms,
                latency_max_ms = max(latency_max_ms, excluded.latency_max_ms),
                ttft_sum_ms = ttft_sum_ms + excluded.ttft_sum_ms,
                ttft_calls = ttft_calls + excluded.ttft_calls,
                cost_usd = cost_usd + excluded.cost_usd,
                priced_calls = priced_calls + excluded.priced_calls,
                retried_calls = retried_calls + excluded.retried_calls,
                loops = loops + excluded.loops,
                decode_ms = decode_ms + excluded.decode_ms,
                decode_out = decode_out + excluded.decode_out,
                decode_n = decode_n + excluded.decode_n
            "#,
        params![
            bucket,
            record.provider,
            record.client_model,
            agent,
            i64::from(record.response_status >= 400),
            to_i64(record.usage.input_tokens),
            to_i64(record.usage.output_tokens),
            to_i64(cache_read_tokens),
            to_i64(cache_write_tokens),
            to_i64(record.usage.cache_write_1h.unwrap_or(0)),
            to_i64(cache_unknown_tokens),
            to_i64(record.usage.reasoning_tokens.unwrap_or(0)),
            to_i64(record.latency_ms),
            to_i64(record.latency_ms),
            to_i64(record.ttft_ms),
            i64::from(record.ttft_ms > 0),
            record.cost_usd.unwrap_or(0.0),
            priced_calls,
            // magpie's `Retried`: answered after a try failed first.
            i64::from(record.attempt > 0 && record.response_status < 400),
            i64::from(record.err_type.as_deref() == Some("loop")),
            to_i64(record.decode_ms),
            to_i64(record.decode_out),
            decode_n,
        ],
    )?;
    Ok(())
}
