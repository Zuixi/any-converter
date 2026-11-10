//! Versioned sqlite schema migrations, tracked in `pragma user_version`.

/// Bump when adding a new `apply` arm so existing databases get the new DDL.
pub(super) const SCHEMA_VERSION: i64 = 11;

/// Ensures incremental auto-vacuum, then every migration newer than `user_version`.
///
/// Connection pragmas (`busy_timeout`, `synchronous`, `foreign_keys`) live in
/// `build_pool`'s `with_init`, so every pooled connection carries them, not just the
/// one that happens to run this function. `journal_mode` is set by `open` only after
/// this returns, so the database is still in rollback-journal mode here.
pub(super) fn migrate(conn: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
    // Runs once per database, and is the ONLY `VACUUM` in this codebase. Without
    // `auto_vacuum = incremental`, `pragma incremental_vacuum` is a no-op and a pruned
    // log file never gives its space back. The rebuild cannot be skipped just because
    // the file has no tables yet: `auto_vacuum` must be persisted while the file is
    // still empty, and once WAL or user tables are in play SQLite may reject the
    // switch. On a new database that rebuild is one page — microseconds; on an
    // existing multi-GB log file it can take tens of seconds, which `open` pays once,
    // then never again.
    let av: i64 = conn.query_row("pragma auto_vacuum", [], |row| row.get(0))?;
    if av == 0 {
        conn.execute_batch("pragma auto_vacuum = incremental;")?;
        conn.execute_batch("vacuum;")?;
    }

    let current: i64 = conn.query_row("pragma user_version", [], |row| row.get(0))?;
    for version in (current + 1)..=SCHEMA_VERSION {
        // One transaction per step: the DDL and the version bump commit together, so a
        // failure leaves the database exactly at the previous version.
        let tx = conn.transaction()?;
        apply(&tx, version)?;
        // The pragma cannot be parameterized; `version` is an i64, not injectable.
        tx.execute_batch(&format!("pragma user_version = {version}"))?;
        tx.commit()?;
    }
    Ok(())
}

fn apply(conn: &rusqlite::Connection, version: i64) -> Result<(), rusqlite::Error> {
    match version {
        1 => conn.execute_batch(
            r#"
            create table if not exists request_logs (
                id integer primary key autoincrement,
                request_id text not null unique,
                timestamp text not null,
                client_format text not null,
                provider text not null,
                client_model text not null,
                upstream_model text not null,
                streaming integer not null,
                method text not null,
                path text not null,
                request_body_json text not null,
                upstream_request_body_json text not null,
                response_status integer not null,
                response_body_kind text not null,
                response_body_json text not null,
                latency_ms integer not null,
                input_tokens integer not null,
                output_tokens integer not null,
                total_tokens integer not null,
                cache_read_tokens integer,
                cache_write_tokens integer,
                reasoning_tokens integer,
                trace_json text not null,
                truncated integer not null,
                record_json text not null,
                created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );

            create index if not exists idx_request_logs_timestamp on request_logs(timestamp);
            create index if not exists idx_request_logs_provider_model on request_logs(provider, client_model);
            create index if not exists idx_request_logs_status on request_logs(response_status);
            create index if not exists idx_request_logs_path on request_logs(path);

            create table if not exists usage_logs (
                id integer primary key autoincrement,
                request_id text not null unique,
                timestamp text not null,
                client_format text not null,
                provider text not null,
                client_model text not null,
                upstream_model text not null,
                input_tokens integer not null,
                output_tokens integer not null,
                total_tokens integer not null,
                latency_ms integer not null,
                status integer not null,
                streaming integer not null,
                record_json text not null,
                created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );

            create index if not exists idx_usage_logs_timestamp on usage_logs(timestamp);
            create index if not exists idx_usage_logs_provider_model on usage_logs(provider, client_model);
            create index if not exists idx_usage_logs_status on usage_logs(status);
            "#,
        ),
        2 => conn.execute_batch(
            r#"
            create table if not exists request_bodies (
                request_id text primary key references request_logs(request_id) on delete cascade,
                record_json text not null
            ) without rowid;

            insert or ignore into request_bodies (request_id, record_json)
                select request_id, record_json from request_logs;

            alter table request_logs drop column request_body_json;
            alter table request_logs drop column upstream_request_body_json;
            alter table request_logs drop column response_body_kind;
            alter table request_logs drop column response_body_json;
            alter table request_logs drop column trace_json;
            alter table request_logs drop column record_json;
            "#,
        ),
        3 => conn.execute_batch(
            r#"
            drop table if exists usage_logs;
            "#,
        ),
        4 => conn.execute_batch(
            r#"
            alter table request_logs add column client_id text;
            alter table request_logs add column session_id text;
            alter table request_logs add column provider_account text;
            alter table request_logs add column served_model text;
            alter table request_logs add column ttft_ms integer not null default 0;
            alter table request_logs add column cache_unknown integer not null default 0;
            alter table request_logs add column cache_write_1h integer not null default 0;
            alter table request_logs add column decode_ms integer not null default 0;
            alter table request_logs add column decode_out integer not null default 0;
            alter table request_logs add column cost_usd real;
            alter table request_logs add column price_version text;

            create index if not exists idx_request_logs_client_ts on request_logs(client_id, timestamp);
            create index if not exists idx_request_logs_session on request_logs(session_id);
            create index if not exists idx_request_logs_account on request_logs(provider, provider_account);
            "#,
        ),
        // The pre-aggregate the usage views read instead of grouping over every log
        // row: one row per (hour, provider, model, agent), maintained by the write path.
        5 => conn.execute_batch(
            r#"
            create table if not exists rollup_hourly (
                bucket text not null,
                provider text not null,
                client_model text not null,
                agent text not null default '',
                calls integer not null default 0,
                errors integer not null default 0,
                input_tokens integer not null default 0,
                output_tokens integer not null default 0,
                cache_read_tokens integer not null default 0,
                cache_write_tokens integer not null default 0,
                cache_write_1h integer not null default 0,
                cache_unknown_tokens integer not null default 0,
                reasoning_tokens integer not null default 0,
                latency_sum_ms integer not null default 0,
                latency_max_ms integer not null default 0,
                ttft_sum_ms integer not null default 0,
                ttft_calls integer not null default 0,
                cost_usd real not null default 0,
                primary key (bucket, provider, client_model, agent)
            ) without rowid;
            "#,
        ),
        // `request_bodies` is the wide payload (three bodies plus the trace) and must
        // expire long before the narrow metadata, so it carries its own timestamp
        // instead of being aged by a join back to `request_logs`.
        6 => {
            // Re-runnable: a previous attempt may have added the column before
            // failing, and a database may have been edited by hand.
            if !has_column(conn, "request_bodies", "timestamp")? {
                conn.execute_batch("alter table request_bodies add column timestamp text;")?;
            }
            conn.execute_batch(
                r#"
                create index if not exists idx_request_bodies_timestamp on request_bodies(timestamp);
                update request_bodies
                   set timestamp = (select l.timestamp from request_logs l where l.request_id = request_bodies.request_id)
                   where timestamp is null;
                "#,
            )
        }
        // How many of a bucket's `calls` carried a known price. `cost_usd` sums only
        // those, so without this counter a summary cannot tell "100 calls worth $1"
        // from "10 priced calls worth $1 and 90 the pricing table never covered".
        7 => {
            if !has_column(conn, "rollup_hourly", "priced_calls")? {
                conn.execute_batch(
                    "alter table rollup_hourly add column priced_calls integer not null default 0;",
                )?;
            }
            conn.execute_batch("update rollup_hourly set priced_calls = calls where cost_usd > 0;")
        }
        // Failover visibility. `attempt` is the zero-based index of the try that
        // answered, so one row says "routing needed N+1 providers". `handed_on`
        // reserves the shape for a row per abandoned try (magpie's `HandedOn`);
        // `error`/`err_type` describe that abandoned try and stay null on the
        // answering row.
        8 => {
            if !has_column(conn, "request_logs", "attempt")? {
                conn.execute_batch(
                    "alter table request_logs add column attempt integer not null default 0;",
                )?;
            }
            if !has_column(conn, "request_logs", "handed_on")? {
                conn.execute_batch(
                    "alter table request_logs add column handed_on integer not null default 0;",
                )?;
            }
            if !has_column(conn, "request_logs", "error")? {
                conn.execute_batch("alter table request_logs add column error text;")?;
            }
            if !has_column(conn, "request_logs", "err_type")? {
                conn.execute_batch("alter table request_logs add column err_type text;")?;
            }
            Ok(())
        }
        // Failover and loop totals, kept beside `calls`/`errors` so a summary never
        // has to group over the detail table for them.
        9 => {
            if !has_column(conn, "rollup_hourly", "retried_calls")? {
                conn.execute_batch(
                    "alter table rollup_hourly add column retried_calls integer not null default 0;",
                )?;
            }
            if !has_column(conn, "rollup_hourly", "loops")? {
                conn.execute_batch(
                    "alter table rollup_hourly add column loops integer not null default 0;",
                )?;
            }
            Ok(())
        }
        // Where the request's time went, and how fast the vendor wrote. One
        // `latency_ms` cannot answer "did the vendor hold the try, or did we",
        // so the row now carries the split. The rest label the call: which
        // upstream/model tier was asked for, sent, and served, at which
        // reasoning effort, and which subagent issued it.
        10 => {
            const COLUMNS: &[(&str, &str)] = &[
                ("own_ms", "integer not null default 0"),
                ("sent_ms", "integer not null default 0"),
                ("retried_ms", "integer not null default 0"),
                ("queued_ms", "integer not null default 0"),
                ("masking_ms", "integer not null default 0"),
                ("flow_ms", "integer not null default 0"),
                ("upstream", "text"),
                ("effort", "text"),
                ("tier_asked", "text"),
                ("tier_sent", "text"),
                ("tier_served", "text"),
                ("subagent", "text"),
                ("parent_agent", "text"),
                ("kind", "text"),
            ];
            for (column, declaration) in COLUMNS {
                if !has_column(conn, "request_logs", column)? {
                    // `column` and `declaration` are literals, not injectable.
                    conn.execute_batch(&format!(
                        "alter table request_logs add column {column} {declaration};"
                    ))?;
                }
            }
            Ok(())
        }
        // The decode window, so a summary can report tokens per second instead of
        // re-reading every log row. Nothing is backfilled: an existing bucket has
        // already lost the per-row split it would need, so `rebuild_rollup` is the
        // way to give old traffic a speed.
        11 => {
            const COLUMNS: &[&str] = &["decode_ms", "decode_out", "decode_n"];
            for column in COLUMNS {
                if !has_column(conn, "rollup_hourly", column)? {
                    // `column` is a literal, not injectable.
                    conn.execute_batch(&format!(
                        "alter table rollup_hourly add column {column} integer not null default 0;"
                    ))?;
                }
            }
            Ok(())
        }
        _ => unreachable!("no migration defined for schema version {version}"),
    }
}

/// Whether `table` already has `column`. Column additions must be re-runnable:
/// a previous attempt may have succeeded before the migration failed, and a
/// user's database may have been edited by hand.
fn has_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
