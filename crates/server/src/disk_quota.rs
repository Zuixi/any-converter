use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::storage::{RetentionPolicy, SqliteStorage};

const TICK: Duration = Duration::from_secs(300);
/// `pragma optimize` runs every 12th tick, i.e. roughly once per hour.
const OPTIMIZE_EVERY_TICKS: u64 = 12;
/// Pages released per tick. Bounded so the write lock is never held long; the
/// freelist is drained over many ticks instead of in one blocking sweep.
const VACUUM_PAGES_PER_TICK: u32 = 2_000;

/// Spawn a background task that periodically enforces disk quota on the log directory.
///
/// Every 5 minutes it scans all files in `dir`, sums their sizes, and removes the
/// oldest files (by modification time) until total usage is below `max_bytes`.
/// When `storage` is set, the same tick also releases a bounded slice of sqlite
/// freelist pages and, once an hour, applies the tiered `policy` retention and
/// refreshes the query planner statistics.
///
/// All sqlite work runs on a blocking thread. Errors are logged but never crash
/// the server.
pub fn spawn_disk_quota_manager(
    dir: PathBuf,
    max_bytes: u64,
    storage: Option<SqliteStorage>,
    policy: RetentionPolicy,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        let mut ticks: u64 = 0;
        loop {
            interval.tick().await;
            ticks += 1;
            let hourly = ticks % OPTIMIZE_EVERY_TICKS == 0;
            sweep(&dir, max_bytes, storage.clone(), policy, hourly).await;
        }
    })
}

/// One maintenance pass: file quota always, then sqlite maintenance (if any).
///
/// Split out of the loop so tests can run a single pass without sleeping. Every
/// failure is logged here and never propagates.
async fn sweep(
    dir: &Path,
    max_bytes: u64,
    storage: Option<SqliteStorage>,
    policy: RetentionPolicy,
    hourly: bool,
) {
    if let Err(e) = enforce_quota(dir, max_bytes) {
        log::warn!(
            "disk quota enforcement failed dir={} error={}",
            dir.display(),
            e
        );
    }

    let Some(storage) = storage else {
        return;
    };

    // Every sqlite call here blocks, so none may run on a worker thread.
    let task = tokio::task::spawn_blocking(move || sweep_storage(&storage, &policy, hourly));
    if let Err(e) = task.await {
        log::warn!("background sqlite maintenance failed: {e}");
    }
}

/// The blocking half of [`sweep`]: a small vacuum always, retention and
/// `pragma optimize` only on the hourly tick.
fn sweep_storage(storage: &SqliteStorage, policy: &RetentionPolicy, hourly: bool) {
    match storage.incremental_vacuum(VACUUM_PAGES_PER_TICK) {
        Ok(0) => {}
        Ok(released) => log::debug!("incremental vacuum released {released} pages"),
        Err(e) => log::warn!("incremental vacuum failed: {e}"),
    }

    if !hourly {
        return;
    }

    match storage.apply_retention(policy, chrono::Utc::now()) {
        Ok(stats) => log::info!(
            "retention removed {} request logs, {} bodies, {} rollup rows",
            stats.logs_deleted,
            stats.bodies_deleted,
            stats.rollup_deleted
        ),
        Err(e) => log::warn!("request log retention failed: {e}"),
    }

    if let Err(e) = storage.optimize() {
        log::warn!("sqlite optimize failed: {e}");
    }
}

fn enforce_quota(dir: &Path, max_bytes: u64) -> std::io::Result<()> {
    if max_bytes == 0 {
        return Ok(());
    }

    let mut entries = collect_log_files(dir)?;
    let total: u64 = entries.iter().map(|e| e.size).sum();

    if total <= max_bytes {
        return Ok(());
    }

    // Sort oldest first (lowest mtime)
    entries.sort_by_key(|e| e.modified);

    let mut freed: u64 = 0;
    let overshoot = total - max_bytes;

    for entry in &entries {
        if freed >= overshoot {
            break;
        }
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                log::info!(
                    "removed old log file for disk quota path={} size={}",
                    entry.path.display(),
                    entry.size
                );
                freed += entry.size;
            }
            Err(e) => {
                log::warn!(
                    "failed to remove old log file path={} error={}",
                    entry.path.display(),
                    e
                );
            }
        }
    }

    Ok(())
}

struct LogFileEntry {
    path: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
}

fn collect_log_files(dir: &Path) -> std::io::Result<Vec<LogFileEntry>> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut entries = Vec::new();
    for result in read_dir {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("any-converter.sqlite3"))
        {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        entries.push(LogFileEntry {
            path: entry.path(),
            size: meta.len(),
            modified,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::request_log::{RequestLogRecord, ResponseBodyKind};
    use any_converter_core::ir::Usage;

    #[test]
    fn test_enforce_quota_noop_when_under_limit() {
        let dir = std::env::temp_dir().join("any_conv_test_quota_noop");
        let _ = std::fs::create_dir_all(&dir);

        let file_path = dir.join("test.jsonl");
        std::fs::write(&file_path, "small data").ok();

        let result = enforce_quota(&dir, 1_000_000);
        assert!(result.is_ok());
        assert!(file_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_enforce_quota_removes_oldest() {
        let dir = std::env::temp_dir().join("any_conv_test_quota_remove");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let old_file = dir.join("old.jsonl");
        let new_file = dir.join("new.jsonl");
        std::fs::write(&old_file, "A".repeat(100)).ok();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&new_file, "B".repeat(100)).ok();

        let result = enforce_quota(&dir, 150);
        assert!(result.is_ok());
        assert!(!old_file.exists(), "old file should be removed");
        assert!(new_file.exists(), "new file should be kept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_enforce_quota_zero_max_is_noop() {
        let dir = std::env::temp_dir().join("any_conv_test_quota_zero");
        let result = enforce_quota(&dir, 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_collect_log_files_missing_dir() {
        let entries = collect_log_files(Path::new("/tmp/nonexistent_any_conv_dir_12345"));
        assert!(entries.is_ok());
        assert!(entries.ok().is_some_and(|e| e.is_empty()));
    }

    #[test]
    fn test_enforce_quota_keeps_sqlite_files() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("any-converter.sqlite3");
        let wal = dir.path().join("any-converter.sqlite3-wal");
        let shm = dir.path().join("any-converter.sqlite3-shm");
        std::fs::write(&database, "D".repeat(100)).unwrap();
        std::fs::write(&wal, "W".repeat(100)).unwrap();
        std::fs::write(&shm, "S".repeat(100)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let log = dir.path().join("requests.2026-07-22.jsonl");
        std::fs::write(&log, "L".repeat(100)).unwrap();

        enforce_quota(dir.path(), 50).unwrap();

        assert!(database.exists());
        assert!(wal.exists());
        assert!(shm.exists());
        assert!(!log.exists());
    }

    #[tokio::test]
    async fn test_optimize_fresh_storage_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SqliteStorage::open_in_log_dir(dir.path()).unwrap();

        assert!(storage.optimize().is_ok());
    }

    #[tokio::test]
    async fn test_sweep_prunes_old_rows_and_keeps_recent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SqliteStorage::open_in_log_dir(dir.path()).unwrap();
        let now = chrono::Utc::now();
        storage
            .insert_request_log(&record_at("req-old", now, 40))
            .unwrap();
        storage
            .insert_request_log(&record_at("req-recent", now, 1))
            .unwrap();

        // max_bytes = 0 disables file quota; the default log window keeps the recent row.
        sweep(
            dir.path(),
            0,
            Some(storage.clone()),
            RetentionPolicy::default(),
            true,
        )
        .await;

        let remaining = storage.recent_request_logs(10).unwrap();
        assert_eq!(remaining.len(), 1, "only the recent row must survive");
        assert_eq!(remaining[0].request_id, "req-recent");
    }

    #[tokio::test]
    async fn test_sweep_zero_retention_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SqliteStorage::open_in_log_dir(dir.path()).unwrap();
        storage
            .insert_request_log(&record_at("req-ancient", chrono::Utc::now(), 4000))
            .unwrap();

        let policy = RetentionPolicy {
            rollup_days: 0,
            log_days: 0,
            body_days: 0,
        };
        sweep(dir.path(), 0, Some(storage.clone()), policy, true).await;

        assert_eq!(storage.recent_request_logs(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_sweep_default_policy_ages_bodies_before_logs() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SqliteStorage::open_in_log_dir(dir.path()).unwrap();
        let now = chrono::Utc::now();
        for (request_id, days_ago) in [("req-40d", 40), ("req-10d", 10), ("req-1d", 1)] {
            storage
                .insert_request_log(&record_at(request_id, now, days_ago))
                .unwrap();
        }
        assert_eq!(storage.body_row_count().unwrap(), 3);

        sweep(
            dir.path(),
            0,
            Some(storage.clone()),
            RetentionPolicy::default(),
            true,
        )
        .await;

        // The 40-day log is past the 30-day log window and cascades its body away.
        // The 10-day log is still inside 30 days but past the 7-day body window, so
        // only its body goes. The 1-day request keeps both.
        let filter = crate::storage::LedgerFilter {
            from: "0000-01-01T00:00:00Z".to_string(),
            to: "9999-12-31T23:59:59Z".to_string(),
            ..Default::default()
        };
        let page = storage.ledger_page(&filter, None, 10).unwrap();
        assert_eq!(
            page.total, 2,
            "the 40-day log must be gone, the others kept"
        );
        assert_eq!(storage.body_row_count().unwrap(), 1);

        let remaining = storage.recent_request_logs(10).unwrap();
        assert_eq!(remaining.len(), 1, "only the 1-day body must survive");
        assert_eq!(remaining[0].request_id, "req-1d");
    }

    #[tokio::test]
    async fn test_sweep_zero_body_days_keeps_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SqliteStorage::open_in_log_dir(dir.path()).unwrap();
        storage
            .insert_request_log(&record_at("req-10d", chrono::Utc::now(), 10))
            .unwrap();

        let policy = RetentionPolicy {
            body_days: 0,
            ..RetentionPolicy::default()
        };
        sweep(dir.path(), 0, Some(storage.clone()), policy, true).await;

        assert_eq!(storage.body_row_count().unwrap(), 1);
        assert_eq!(storage.recent_request_logs(10).unwrap().len(), 1);
    }

    #[test]
    fn test_max_capture_bytes_defaults_to_one_mib() {
        assert_eq!(
            crate::config::RequestLogConfig::default().max_capture_bytes,
            1024 * 1024
        );
    }

    fn record_at(
        request_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        days_ago: i64,
    ) -> RequestLogRecord {
        RequestLogRecord {
            request_id: request_id.to_string(),
            timestamp: (now - chrono::Duration::days(days_ago))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
            client_format: "openai_responses".to_string(),
            client_id: None,
            session_id: None,
            provider_account: None,
            served_model: None,
            ttft_ms: 0,
            cache_unknown: false,
            cost_usd: None,
            price_version: None,
            decode_ms: 0,
            decode_out: 0,
            own_ms: 0,
            sent_ms: 0,
            retried_ms: 0,
            queued_ms: 0,
            masking_ms: 0,
            flow_ms: 0,
            upstream: None,
            effort: None,
            tier_asked: None,
            tier_sent: None,
            tier_served: None,
            subagent: None,
            parent_agent: None,
            kind: None,
            attempt: 0,
            handed_on: false,
            error: None,
            err_type: None,
            provider: "kimi".to_string(),
            client_model: "gpt-5.4".to_string(),
            upstream_model: "moonshot-v1".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            request_body: None,
            upstream_request_body: None,
            response_status: 200,
            response_body: ResponseBodyKind::Json {
                text: "{}".to_string(),
            },
            latency_ms: 12,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_write_1h: None,
                reasoning_tokens: None,
            },
            trace: None,
            truncated: false,
        }
    }
}
