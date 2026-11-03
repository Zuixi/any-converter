//! Durable recovery stores for agent configuration wiring.
//!
//! The stores are deliberately plain files next to the configuration they
//! protect. Each is written through [`crate::atomic`], so a crash never leaves a
//! half-written recovery record behind.

pub mod applied;
pub mod snapshot;
pub mod stash;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use applied::Applied;
pub use snapshot::Snapshot;
pub use stash::Stash;

/// Errors produced by the recovery stores.
#[derive(Debug)]
pub enum BackupError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// Stored recovery data exists but cannot be used.
    Corrupt(String),
    /// The requested recovery data does not exist.
    Missing(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Corrupt(message) => write!(f, "corrupt recovery data: {message}"),
            Self::Missing(message) => write!(f, "missing recovery data: {message}"),
        }
    }
}

impl std::error::Error for BackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Corrupt(_) | Self::Missing(_) => None,
        }
    }
}

impl From<io::Error> for BackupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Appends a literal suffix to the path's final component.
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

/// Current wall-clock time as whole Unix seconds.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Chooses a bad-copy path that does not exist yet.
///
/// The convention is `<path>.bad-<unix_ts>` with `-2`, `-3`, ... appended when
/// several copies are preserved within the same second. Existing bad copies are
/// never reused.
pub(crate) fn bad_copy_path(path: &Path) -> PathBuf {
    let base = with_suffix(path, &format!(".bad-{}", now_unix()));
    let mut candidate = base.clone();
    let mut attempt = 2;
    while candidate.exists() {
        candidate = with_suffix(&base, &format!("-{attempt}"));
        attempt += 1;
    }
    candidate
}

/// Moves `path` aside to a fresh bad-copy path, if it exists.
pub(crate) fn move_aside(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let target = bad_copy_path(path);
    fs::rename(path, &target)?;
    Ok(Some(target))
}
