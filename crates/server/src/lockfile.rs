//! Cross-process advisory file locking.
//!
//! The CLI (`accounts add`) and the server are separate processes that can
//! both refresh the same OAuth credential. Vendors answer concurrent spends
//! of one refresh token with `refresh_token_reused` and revoke the
//! sign-in, so refresh must be serialized across processes, not just within
//! one tokio runtime. [`FileLock`] is magpie's `filelock.Mutex` equivalent
//! (`logins.json.codex.lock`): a `create_new` lock file whose holder writes
//! its pid and a timestamp.
//!
//! The lock is advisory: every participant must agree on the same path. A
//! lock file whose mtime is older than [`DEFAULT_STALE_AFTER`] is
//! considered abandoned (crashed holder) and is taken over by the next
//! contender.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use thiserror::Error;

/// How old a lock file must be before it counts as abandoned.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(30);
/// How long [`FileLock::acquire_timeout`] waits by default.
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Sleep between acquisition attempts.
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Failures of lock acquisition.
#[derive(Debug, Error)]
pub enum LockError {
    /// Filesystem failure while creating or inspecting the lock file.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The lock stayed held for the whole timeout.
    #[error("timed out waiting for lock {0}")]
    Timeout(PathBuf),
    /// The blocking task failed to start (shutting-down runtime).
    #[error("lock task failed: {0}")]
    Task(String),
}

/// Why one acquisition attempt failed.
enum TryAcquire {
    /// A live holder owns the file.
    Busy,
    /// An abandoned file was removed; try again.
    Stale,
    /// Real filesystem failure.
    Io(std::io::Error),
}

/// A path-based cross-process lock. Cheap to clone.
#[derive(Clone)]
pub struct FileLock {
    path: PathBuf,
    stale_after: Duration,
}

/// A cross-process kernel lock that is never reclaimed while its holder runs.
///
/// Unlike [`FileLock`], this lock does not use an age heuristic. The kernel
/// releases it when the process exits, so a slow startup can never be taken
/// over by a second gateway process.
#[derive(Clone, Debug)]
pub struct InstanceLock {
    path: PathBuf,
}

impl InstanceLock {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn acquire_timeout(&self, timeout: Duration) -> io::Result<InstanceLockGuard> {
        let deadline = Instant::now() + timeout;
        loop {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.path)?;
            match file.try_lock_exclusive() {
                Ok(true) => return Ok(InstanceLockGuard { file }),
                Ok(false) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "instance lock remained held",
                        ));
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn acquire(&self, timeout: Duration) -> io::Result<InstanceLockGuard> {
        let lock = self.clone();
        tokio::task::spawn_blocking(move || lock.acquire_timeout(timeout))
            .await
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?
    }
}

#[derive(Debug)]
pub struct InstanceLockGuard {
    file: fs::File,
}

impl Drop for InstanceLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl FileLock {
    /// Lock the file at `path` (the file is created by the acquire and
    /// removed by the returned guard; nothing else lives at this path).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stale_after: DEFAULT_STALE_AFTER,
        }
    }

    /// Override the abandoned-lock threshold (unit tests).
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }

    /// Acquire from a tokio context: runs the synchronous acquisition on
    /// the blocking pool.
    pub async fn acquire(&self, timeout: Duration) -> Result<FileLockGuard, LockError> {
        let lock = self.clone();
        tokio::task::spawn_blocking(move || lock.acquire_timeout(timeout))
            .await
            .map_err(|err| LockError::Task(err.to_string()))?
    }

    /// Synchronously wait up to `timeout` for the lock.
    pub fn acquire_timeout(&self, timeout: Duration) -> Result<FileLockGuard, LockError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.try_acquire() {
                Ok(guard) => return Ok(guard),
                // An abandoned lock was removed: retry immediately.
                Err(TryAcquire::Stale) => continue,
                Err(TryAcquire::Busy) => {
                    if Instant::now() >= deadline {
                        return Err(LockError::Timeout(self.path.clone()));
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(TryAcquire::Io(err)) => return Err(LockError::Io(err)),
            }
        }
    }

    /// One acquisition attempt.
    fn try_acquire(&self) -> Result<FileLockGuard, TryAcquire> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(TryAcquire::Io)?;
            }
        }

        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&self.path) {
            Ok(mut file) => {
                let payload = format!(
                    "pid={} at={}.{:09}\n",
                    std::process::id(),
                    unix_now(),
                    unix_subsec_nanos()
                );
                if let Err(err) = file.write_all(payload.as_bytes()) {
                    let _ = fs::remove_file(&self.path);
                    return Err(TryAcquire::Io(err));
                }
                Ok(FileLockGuard {
                    path: self.path.clone(),
                    payload,
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let abandoned = fs::metadata(&self.path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > self.stale_after);
                if abandoned {
                    // The holder crashed (or hung past `stale_after`):
                    // take the lock over.
                    let _ = fs::remove_file(&self.path);
                    return Err(TryAcquire::Stale);
                }
                Err(TryAcquire::Busy)
            }
            Err(err) => Err(TryAcquire::Io(err)),
        }
    }
}

/// A held lock; the lock file is removed on drop.
#[derive(Debug)]
pub struct FileLockGuard {
    path: PathBuf,
    /// Exact bytes written when the lock was taken, used to verify that the
    /// file still belongs to this guard before removing it: a guard whose
    /// lock was taken over as stale must not delete its successor's file.
    payload: String,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|contents| contents == self.payload) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_subsec_nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_lock_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "any-converter-lockfile-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn instance_lock_is_not_reclaimed_while_the_holder_is_alive() {
        let path = temp_lock_path("instance-held");
        let holder = InstanceLock::new(path.clone())
            .acquire_timeout(Duration::from_secs(5))
            .unwrap();

        let error = InstanceLock::new(path.clone())
            .acquire_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);

        drop(holder);
        let _guard = InstanceLock::new(path.clone())
            .acquire_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(path.exists());
    }

    #[test]
    fn only_one_thread_holds_the_lock() {
        let path = temp_lock_path("single");
        let lock = FileLock::new(path);
        let in_critical = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(3));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let lock = lock.clone();
            let in_critical = Arc::clone(&in_critical);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..3 {
                    let guard = lock.acquire_timeout(Duration::from_secs(5)).unwrap();
                    let prev = in_critical.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(prev, 0, "two threads held the lock at once");
                    std::thread::sleep(Duration::from_millis(30));
                    in_critical.fetch_sub(1, Ordering::SeqCst);
                    drop(guard);
                }
            }));
        }
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(in_critical.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stale_lock_is_taken_over_without_deleting_the_successor() {
        let path = temp_lock_path("stale");
        let holder = FileLock::new(path.clone());
        let contender = FileLock::new(path.clone()).with_stale_after(Duration::from_millis(50));

        let first = holder.acquire_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(120));

        // A contender with a short stale threshold sees the lock as
        // abandoned and takes it over.
        let second = contender.acquire_timeout(Duration::from_secs(5)).unwrap();

        // The stale holder's guard must NOT delete the new owner's file.
        drop(first);
        assert!(path.exists(), "successor's lock file must survive");

        // And the new owner still holds it: a third contender times out.
        let err = FileLock::new(path.clone())
            .acquire_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert!(matches!(err, LockError::Timeout(_)), "{err:?}");

        drop(second);
        assert!(!path.exists(), "guard drop must remove the lock file");
    }

    #[test]
    fn timeout_is_reported_while_the_lock_is_held() {
        let path = temp_lock_path("timeout");
        let holder = FileLock::new(path.clone());
        let _first = holder.acquire_timeout(Duration::from_secs(5)).unwrap();

        let start = Instant::now();
        let err = FileLock::new(path.clone())
            .acquire_timeout(Duration::from_millis(200))
            .unwrap_err();
        assert!(matches!(err, LockError::Timeout(_)), "{err:?}");
        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "must wait the full timeout"
        );
    }

    #[test]
    fn guard_drop_removes_the_lock_file() {
        let path = temp_lock_path("drop");
        let guard = FileLock::new(path.clone())
            .acquire_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(path.exists(), "lock file must exist while held");
        drop(guard);
        assert!(!path.exists(), "guard drop must remove the lock file");
    }

    #[tokio::test]
    async fn async_acquire_runs_on_the_blocking_pool() {
        let path = temp_lock_path("async");
        let guard = FileLock::new(path.clone())
            .acquire(Duration::from_secs(5))
            .await
            .unwrap();
        assert!(path.exists(), "lock file must exist while held");
        drop(guard);
        assert!(!path.exists(), "guard drop must remove the lock file");
    }
}
