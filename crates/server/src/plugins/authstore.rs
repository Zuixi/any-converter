//! OpenCode-shaped plugin auth storage (`plugin-auth.json`).
//!
//! Entries are keyed by `<provider>:<account>` and shaped exactly like
//! OpenCode's auth file (see magpie host.js `save()`/`setAuth`):
//!
//! ```json
//! {
//!   "github:acct": { "type": "oauth", "refresh": "...", "access": "...", "expires": 123 },
//!   "openai:acct": { "type": "api", "key": "...", "metadata": {} }
//! }
//! ```
//!
//! Unknown fields inside an entry are preserved on round-trip. Writes use the
//! atomic pattern proven in magpie: `0600` temp file → fsync → rename, with a
//! last-good `.bak` of the previous contents and a `<path>.lock` lock file
//! (`0600`, `O_CREAT | O_EXCL`, stale takeover).

use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Pause between lock attempts while another holder keeps the lock fresh.
const LOCK_POLL: Duration = Duration::from_millis(10);

/// Failures surfaced by [`PluginAuthStore`] operations.
#[derive(Debug, Error)]
pub enum AuthStoreError {
    /// Filesystem failure (create/write/rename/copy).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The on-disk payload could not be (de)serialized.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Neither the primary file nor the backup could be parsed.
    #[error("auth store corrupted and no usable backup at {0}")]
    Corrupted(PathBuf),
}

/// One auth entry in the OpenCode auth file.
///
/// The `type` tag mirrors OpenCode exactly: `"oauth"` / `"api"`. Fields we do
/// not model are captured in `extra` and written back out unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PluginAuth {
    /// OAuth credential: token pair plus epoch expiry.
    #[serde(rename = "oauth")]
    OAuth {
        refresh: Option<String>,
        access: Option<String>,
        expires: Option<i64>,
        /// Unknown fields, preserved for round-trips.
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    /// Static API key entry.
    #[serde(rename = "api")]
    ApiKey {
        key: String,
        /// Unknown fields (e.g. `metadata`), preserved for round-trips.
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
}

/// File-backed auth store keyed by `<provider>:<account>` account keys.
pub struct PluginAuthStore {
    path: PathBuf,
    /// A lock file older than this is considered abandoned and may be taken
    /// over (magpie's stale-lock judgment).
    stale_after: Duration,
}

impl PluginAuthStore {
    /// Create a store backed by `path` with the default 60s stale threshold.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_stale_after(path, Duration::from_secs(60))
    }

    /// Create a store backed by `path` with a custom stale-lock threshold.
    pub fn with_stale_after(path: impl Into<PathBuf>, stale_after: Duration) -> Self {
        Self {
            path: path.into(),
            stale_after,
        }
    }

    /// The file path this store persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The provider part of an `<provider>:<account>` account key
    /// (everything before the first `:`; keys without a colon map to
    /// themselves, mirroring OpenCode's `providerOf`).
    pub fn provider_of(account_key: &str) -> &str {
        account_key.split(':').next().unwrap_or(account_key)
    }

    fn bak_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(".bak");
        PathBuf::from(p)
    }

    fn lock_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(".lock");
        PathBuf::from(p)
    }

    fn tmp_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(format!(".tmp-{}", std::process::id()));
        PathBuf::from(p)
    }

    /// Parse a store file; `Ok(None)` when the file does not exist (a fresh
    /// store is not an error).
    fn read_file(path: &Path) -> Result<Option<BTreeMap<String, PluginAuth>>, AuthStoreError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Load the current map, falling back to `.bak` when the primary file is
    /// corrupted. A missing primary file is a fresh (empty) store, not an
    /// error; a corrupted primary with no usable backup is `Err`.
    pub fn load(&self) -> Result<BTreeMap<String, PluginAuth>, AuthStoreError> {
        match Self::read_file(&self.path) {
            Ok(Some(map)) => Ok(map),
            Ok(None) => Ok(BTreeMap::new()),
            Err(_) => match Self::read_file(&self.bak_path()) {
                Ok(Some(map)) => Ok(map),
                Ok(None) => Err(AuthStoreError::Corrupted(self.path.clone())),
                Err(e) => Err(e),
            },
        }
    }

    /// Persist `map` atomically: ensure a `0700` parent directory, write the
    /// payload to a `0600` temp file, fsync, back up the current file to
    /// `.bak`, then rename over the target.
    pub fn save(&self, map: &BTreeMap<String, PluginAuth>) -> Result<(), AuthStoreError> {
        let bytes = serde_json::to_vec_pretty(map)?;

        self.ensure_parent()?;

        let tmp = self.tmp_path();
        {
            use std::io::Write;
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(&bytes)?;
            f.sync_all()?;
        }

        // Last-good backup of the current contents, taken before the rename
        // replaces them (`.bak` always mirrors the previous committed state).
        if self.path.exists() {
            fs::copy(&self.path, self.bak_path())?;
        }
        // Atomic on POSIX via same-directory rename; on Windows `rename`
        // fails when the destination exists, so remove first.
        #[cfg(windows)]
        let _ = fs::remove_file(&self.path);
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Create the parent directory (`0700`) if needed.
    fn ensure_parent(&self) -> Result<(), AuthStoreError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(parent)?.permissions();
                    if perms.mode() & 0o777 != 0o700 {
                        perms.set_mode(0o700);
                        fs::set_permissions(parent, perms)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Insert or overwrite the entry stored under `account_key`.
    pub fn set(&self, account_key: &str, auth: PluginAuth) -> Result<(), AuthStoreError> {
        self.with_lock(|| {
            let mut map = self.load()?;
            map.insert(account_key.to_string(), auth);
            self.save(&map)
        })?
    }

    /// Remove the entry stored under `account_key`; returns whether it
    /// existed.
    pub fn remove(&self, account_key: &str) -> Result<bool, AuthStoreError> {
        self.with_lock(|| -> Result<bool, AuthStoreError> {
            let mut map = self.load()?;
            if map.remove(account_key).is_some() {
                self.save(&map)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })?
    }

    /// All account keys whose provider part matches `provider`, sorted.
    pub fn accounts_of(&self, provider: &str) -> Result<Vec<String>, AuthStoreError> {
        let prefix = format!("{provider}:");
        Ok(self
            .load()?
            .into_keys()
            .filter(|k| k.starts_with(&prefix))
            .collect())
    }

    /// Run `f` while holding the `<path>.lock` lock file.
    ///
    /// The lock is created with `O_CREAT | O_EXCL` (mode `0600`). When it
    /// already exists and is older than the stale threshold, it is considered
    /// abandoned and taken over (delete + recreate); a fresh lock is polled
    /// until released. The lock file is removed after `f` returns. If `f`
    /// panics the lock lingers and will be reclaimed as stale.
    ///
    /// Not reentrant: calling this again inside `f` deadlocks until the
    /// stale threshold passes.
    pub fn with_lock<T>(&self, f: impl FnOnce() -> T) -> Result<T, AuthStoreError> {
        self.ensure_parent()?;
        let lock = self.lock_path();
        loop {
            match create_lock_file(&lock) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .map(|at| at.elapsed().unwrap_or(Duration::ZERO) > self.stale_after)
                        .unwrap_or(true);
                    if stale {
                        // Abandoned lock: take it over. If a contender recreates
                        // it first, the loop simply retries.
                        let _ = fs::remove_file(&lock);
                        continue;
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(e) => return Err(e.into()),
            }
        }
        let out = f();
        // Best-effort release: a failure here only delays the next holder
        // until the lock goes stale.
        let _ = fs::remove_file(&lock);
        Ok(out)
    }
}

/// Create the lock file with `O_CREAT | O_EXCL` and mode `0600` (unix).
fn create_lock_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map(|_| ())
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use serde_json::json;

    use super::*;

    fn dir() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    fn oauth() -> PluginAuth {
        PluginAuth::OAuth {
            refresh: Some("r-token".into()),
            access: Some("a-token".into()),
            expires: Some(1_700_000_000_000),
            extra: BTreeMap::new(),
        }
    }

    fn api_key() -> PluginAuth {
        PluginAuth::ApiKey {
            key: "sk-test".into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn load_missing_file_is_empty() {
        let store = PluginAuthStore::new(dir().join("auth.json"));
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn round_trip_preserves_extra_fields() {
        let root = dir();
        let path = root.join("auth.json");
        let raw = r#"{"github:u1":{"type":"oauth","refresh":"r1","access":"a1","expires":1700000000000,"scopes":["repo"],"project":"p"},
                      "openai:u2":{"type":"api","key":"sk-9","metadata":{"org":"acme"}}}"#;
        fs::write(&path, raw).unwrap();

        let store = PluginAuthStore::new(&path);
        let map = store.load().unwrap();
        assert_eq!(
            map.get("github:u1"),
            Some(&PluginAuth::OAuth {
                refresh: Some("r1".into()),
                access: Some("a1".into()),
                expires: Some(1_700_000_000_000),
                extra: BTreeMap::from([
                    ("scopes".to_string(), json!(["repo"])),
                    ("project".to_string(), json!("p")),
                ]),
            })
        );
        assert_eq!(
            map.get("openai:u2"),
            Some(&PluginAuth::ApiKey {
                key: "sk-9".into(),
                extra: BTreeMap::from([("metadata".to_string(), json!({"org": "acme"}))]),
            })
        );

        // Full round-trip through save keeps the unknown fields byte-equal in
        // value space.
        store.save(&map).unwrap();
        assert_eq!(store.load().unwrap(), map);
    }

    #[test]
    fn set_remove_and_accounts_of() {
        let root = dir();
        let store = PluginAuthStore::new(root.join("auth.json"));

        store.set("github:a", oauth()).unwrap();
        store.set("github:b", api_key()).unwrap();
        store.set("other:c", oauth()).unwrap();

        assert_eq!(
            store.accounts_of("github").unwrap(),
            vec!["github:a".to_string(), "github:b".to_string()]
        );
        assert!(store.accounts_of("missing").unwrap().is_empty());

        assert!(store.remove("github:a").unwrap());
        assert!(!store.remove("github:a").unwrap());
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn provider_of_takes_prefix_before_colon() {
        assert_eq!(PluginAuthStore::provider_of("github:acct"), "github");
        assert_eq!(PluginAuthStore::provider_of("a:b:c"), "a");
        assert_eq!(PluginAuthStore::provider_of("nocolon"), "nocolon");
    }

    #[test]
    fn corrupted_main_falls_back_to_bak() {
        let root = dir();
        let path = root.join("auth.json");
        let store = PluginAuthStore::new(&path);

        store.set("p:a", oauth()).unwrap();
        // Second save backs up the first committed state to `.bak`.
        store.set("p:b", oauth()).unwrap();

        fs::write(&path, "corrupted{{{").unwrap();
        let map = store.load().unwrap();
        assert!(map.contains_key("p:a"));
        assert!(!map.contains_key("p:b"));

        // Both corrupted → hard error.
        fs::write(store.bak_path(), "also bad").unwrap();
        assert!(store.load().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_restricted() {
        use std::os::unix::fs::PermissionsExt;

        let root = dir();
        let store = PluginAuthStore::new(root.join("auth").join("auth.json"));
        store.set("github:a", oauth()).unwrap();

        let file_mode = fs::metadata(root.join("auth").join("auth.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600);
        let dir_mode = fs::metadata(root.join("auth"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        // Lock is released after the mutation.
        assert!(!root.join("auth").join("auth.json.lock").exists());
    }

    #[test]
    fn lock_is_mutually_exclusive() {
        let root = dir();
        let store = Arc::new(PluginAuthStore::new(root.join("auth.json")));

        let entered = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = Arc::clone(&store);
                let entered = Arc::clone(&entered);
                let max_concurrent = Arc::clone(&max_concurrent);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .with_lock(|| {
                            let n = entered.fetch_add(1, Ordering::SeqCst) + 1;
                            max_concurrent.fetch_max(n, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(50));
                            entered.fetch_sub(1, Ordering::SeqCst);
                        })
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
        assert!(!root.join("auth.json.lock").exists());
    }

    #[test]
    fn stale_lock_is_taken_over() {
        let root = dir();
        let lock = root.join("auth.json.lock");
        let f = fs::File::create(&lock).unwrap();
        // Make the lock look 120s old, past the 60s threshold.
        f.set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        drop(f);

        let store =
            PluginAuthStore::with_stale_after(root.join("auth.json"), Duration::from_secs(60));
        assert_eq!(store.with_lock(|| 7).unwrap(), 7);
        assert!(!lock.exists());

        // A fresh lock is NOT stolen.
        let f = fs::File::create(&lock).unwrap();
        f.set_modified(SystemTime::now()).unwrap();
        drop(f);
        let store =
            PluginAuthStore::with_stale_after(root.join("auth.json"), Duration::from_secs(60));
        let result = thread::spawn(move || {
            let _ = store.with_lock(|| 7);
        });
        thread::sleep(Duration::from_millis(100));
        // Fresh lock still in place while the other thread waits.
        assert!(lock.exists());
        fs::remove_file(&lock).unwrap(); // release so the waiter can finish
        result.join().unwrap();
    }
}
