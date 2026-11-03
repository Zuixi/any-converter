//! Byte-faithful backups of agent files.
//!
//! A backup lives beside its agent file as `<file>.before-any-converter`. Taking
//! a snapshot is idempotent: an existing backup is never overwritten, so a
//! second connect cannot destroy the pristine pre-connect bytes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{BackupError, bad_copy_path, with_suffix};

const BACKUP_SUFFIX: &str = ".before-any-converter";
const ABSENT_SUFFIX: &str = ".absent";

/// The set of agent files covered by byte backups.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    files: Vec<PathBuf>,
}

impl Snapshot {
    /// Creates a snapshot over `files`.
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self { files }
    }

    /// Returns the files covered by this snapshot.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Captures pristine backups for every covered file.
    ///
    /// Files that already have a backup, or an absent marker, are skipped.
    /// Existing files are copied byte-for-byte; missing files get an empty
    /// `<file>.before-any-converter.absent` marker so disconnect can restore the
    /// absent state.
    pub fn take(&self) -> Result<(), BackupError> {
        for file in &self.files {
            let backup = Self::backup_path(file);
            let marker = with_suffix(&backup, ABSENT_SUFFIX);
            if backup.exists() || marker.exists() {
                continue;
            }
            match fs::read(file) {
                Ok(bytes) => crate::atomic::write_atomic(&backup, &bytes)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    crate::atomic::write_atomic(&marker, b"")?;
                }
                Err(error) => return Err(BackupError::Io(error)),
            }
        }
        Ok(())
    }

    /// Reports whether a byte backup exists for `file`.
    pub fn has_backup(&self, file: &Path) -> bool {
        Self::backup_path(file).exists()
    }

    /// Reads the byte backup for `file`.
    ///
    /// Returns `Ok(None)` when no backup exists. An existing but unreadable
    /// backup is reported as [`BackupError::Corrupt`].
    pub fn read_backup(&self, file: &Path) -> Result<Option<Vec<u8>>, BackupError> {
        match fs::read(Self::backup_path(file)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(BackupError::Corrupt(format!(
                "cannot read backup for {}: {error}",
                file.display()
            ))),
        }
    }

    /// Removes the backup and absent marker for one file.
    ///
    /// Used after a successful restore. Missing entries are ignored.
    pub fn clear(&self, file: &Path) {
        let backup = Self::backup_path(file);
        let marker = with_suffix(&backup, ABSENT_SUFFIX);
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&marker);
    }

    /// Moves an unreadable or corrupt backup aside.
    ///
    /// The new name is `<file>.before-any-converter.bad-<unix_ts>`, with a
    /// numeric suffix when that name already exists. The returned path is the
    /// chosen bad-copy path; failures are intentionally best-effort so the
    /// caller can continue with a degraded restore.
    pub fn preserve_bad(&self, file: &Path) -> PathBuf {
        let backup = Self::backup_path(file);
        let target = bad_copy_path(&backup);
        let _ = fs::rename(&backup, &target);
        target
    }

    fn backup_path(file: &Path) -> PathBuf {
        with_suffix(file, BACKUP_SUFFIX)
    }
}
