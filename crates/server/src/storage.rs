use std::path::{Path, PathBuf};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::request_log::{RequestLogRecord, ResponseBodyKind};
use crate::usage::UsageRecord;

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

#[derive(Clone)]
pub struct SqliteStorage {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let pool = build_pool(SqliteConnectionManager::file(path))?;
        let conn = pool.get()?;
        initialize_schema(&conn)?;
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
            pool: build_pool(manager)?,
        })
    }

    pub fn insert_request_log(&self, record: &RequestLogRecord) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        let response_body_json = serde_json::to_string(&record.response_body)?;
        let request_body_json = serde_json::to_string(&record.request_body)?;
        let upstream_request_body_json = serde_json::to_string(&record.upstream_request_body)?;
        let trace_json = serde_json::to_string(&record.trace)?;
        let record_json = serde_json::to_string(record)?;
        let response_body_kind = match &record.response_body {
            ResponseBodyKind::Json { .. } => "json",
            ResponseBodyKind::SseLines { .. } => "sse",
        };
        let total_tokens = record.usage.input_tokens + record.usage.output_tokens;

        conn.execute(
            r#"
            insert into request_logs (
                request_id, timestamp, client_format, provider, client_model,
                upstream_model, streaming, method, path, request_body_json,
                upstream_request_body_json, response_status, response_body_kind,
                response_body_json, latency_ms, input_tokens, output_tokens,
                total_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
                trace_json, truncated, record_json
            ) values (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13,
                ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21,
                ?22, ?23, ?24
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
                request_body_json = excluded.request_body_json,
                upstream_request_body_json = excluded.upstream_request_body_json,
                response_status = excluded.response_status,
                response_body_kind = excluded.response_body_kind,
                response_body_json = excluded.response_body_json,
                latency_ms = excluded.latency_ms,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                total_tokens = excluded.total_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                cache_write_tokens = excluded.cache_write_tokens,
                reasoning_tokens = excluded.reasoning_tokens,
                trace_json = excluded.trace_json,
                truncated = excluded.truncated,
                record_json = excluded.record_json
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
                request_body_json,
                upstream_request_body_json,
                record.response_status,
                response_body_kind,
                response_body_json,
                to_i64(record.latency_ms),
                to_i64(record.usage.input_tokens),
                to_i64(record.usage.output_tokens),
                to_i64(total_tokens),
                record.usage.cache_read_tokens.map(to_i64),
                record.usage.cache_write_tokens.map(to_i64),
                record.usage.reasoning_tokens.map(to_i64),
                trace_json,
                record.truncated,
                record_json,
            ],
        )?;
        Ok(())
    }

    pub fn insert_usage_record(&self, record: &UsageRecord) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        let record_json = serde_json::to_string(record)?;
        conn.execute(
            r#"
            insert into usage_logs (
                request_id, timestamp, client_format, provider, client_model,
                upstream_model, input_tokens, output_tokens, total_tokens,
                latency_ms, status, streaming, record_json
            ) values (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13
            )
            on conflict(request_id) do update set
                timestamp = excluded.timestamp,
                client_format = excluded.client_format,
                provider = excluded.provider,
                client_model = excluded.client_model,
                upstream_model = excluded.upstream_model,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                total_tokens = excluded.total_tokens,
                latency_ms = excluded.latency_ms,
                status = excluded.status,
                streaming = excluded.streaming,
                record_json = excluded.record_json
            "#,
            params![
                record.request_id,
                record.timestamp,
                record.client_format,
                record.provider,
                record.client_model,
                record.upstream_model,
                to_i64(record.input_tokens),
                to_i64(record.output_tokens),
                to_i64(record.total_tokens),
                to_i64(record.latency_ms),
                record.status,
                record.streaming,
                record_json,
            ],
        )?;
        Ok(())
    }

    pub fn recent_request_logs(&self, limit: u64) -> Result<Vec<RequestLogRecord>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            r#"
            select record_json
            from request_logs
            order by timestamp desc, id desc
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
}

fn build_pool(
    manager: SqliteConnectionManager,
) -> Result<Pool<SqliteConnectionManager>, r2d2::Error> {
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

fn initialize_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        r#"
        pragma journal_mode = wal;
        pragma synchronous = normal;
        pragma foreign_keys = on;

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
    )
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
