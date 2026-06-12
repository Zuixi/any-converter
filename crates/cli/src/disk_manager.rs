use std::path::{Path, PathBuf};
use std::time::Duration;

/// Spawn a background task that periodically enforces disk quota on the log directory.
///
/// Scans all files in `dir`, sums their sizes, and removes the oldest files
/// (by modification time) until total usage is below `max_bytes`.
///
/// Runs every 5 minutes. Errors are logged but never crash the server.
pub fn spawn_disk_manager(dir: PathBuf, max_bytes: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            interval.tick().await;
            if let Err(e) = enforce_quota(&dir, max_bytes) {
                log::warn!(
                    "disk quota enforcement failed dir={} error={}",
                    dir.display(),
                    e
                );
            }
        }
    })
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
    use super::*;

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
}
