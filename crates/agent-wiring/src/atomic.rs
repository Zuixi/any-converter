//! Durable, atomic file replacement helpers.
//!
//! Writes go to a hidden temporary file in the destination directory, are
//! flushed to disk, and are then renamed over the target. A crash therefore
//! leaves either the old file or the new one, never a partially written file.
//!
//! Writes that would not change the file are skipped entirely so callers do not
//! churn modification times (catalog sync uses this to stay quiet).

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TEMP_SUFFIX: &str = ".tmp";
const MAX_TEMP_AGE: Duration = Duration::from_secs(10 * 60);
const MAX_TEMP_ATTEMPTS: usize = 100;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads a file, returning `None` when it does not exist.
pub fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Atomically writes `data` to `path`.
///
/// The parent directory is created when missing. An existing file's permission
/// bits are preserved. When the file already contains exactly `data`, the call
/// is a no-op and the modification time is left untouched. Stale temporary
/// files from older writes are best-effort removed after a successful write.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    if read(path)?.as_deref() == Some(data) {
        return Ok(());
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no file name: {}", path.display()),
        )
    })?;
    let permissions = fs::metadata(path).ok().map(|meta| meta.permissions());

    let (temp_path, mut temp) = create_temp(parent, file_name)?;
    let written = (|| -> io::Result<()> {
        temp.write_all(data)?;
        if let Some(permissions) = permissions {
            temp.set_permissions(permissions)?;
        }
        temp.sync_all()
    })();
    drop(temp);

    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    sync_dir(parent);
    remove_stale_temps(parent, file_name);
    Ok(())
}

fn create_temp(parent: &Path, file_name: &OsStr) -> io::Result<(PathBuf, File)> {
    let display_name = file_name.to_string_lossy();
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let name = format!(
            ".{display_name}.{}.{nanos}.{counter}{TEMP_SUFFIX}",
            std::process::id()
        );
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not create a temporary file for {}", parent.display()),
    ))
}

fn sync_dir(parent: &Path) {
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

fn remove_stale_temps(parent: &Path, file_name: &OsStr) {
    let prefix = format!(".{}.", file_name.to_string_lossy());
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(TEMP_SUFFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let stale = SystemTime::now()
            .duration_since(modified)
            .map(|age| age > MAX_TEMP_AGE)
            .unwrap_or(false);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}
