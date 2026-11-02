//! Encrypted-at-rest secret storage backends.
//!
//! [`FileSecretStore`] persists credentials as a JSON map under the caller
//! supplied path (e.g. `<config_dir>/secrets.json`) using the atomic write
//! pattern proven in magpie's `writePrivate` (steady.WriteFile): serialize in
//! full → write a `0600` temp file → rename over the target, keeping a
//! `path.bak` copy of the previous contents for recovery when the main file
//! is corrupted.

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

use super::Credential;

/// Errors surfaced by [`SecretStore`] implementations.
#[derive(Debug, Error)]
pub enum SecretStoreError {
    /// Filesystem failure (create/write/rename/copy).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The on-disk payload could not be (de)serialized.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The store payload exists but is not a JSON object map.
    #[error("store file is not a JSON object map: {0}")]
    NotAMap(String),
    /// Neither the primary file nor the backup could be read.
    #[error("credential store corrupted and no usable backup at {0}")]
    NotFound(PathBuf),
}

/// Storage backend for provider credentials, keyed by account id.
pub trait SecretStore: Send + Sync {
    /// Insert or overwrite the credential stored under `account_id`.
    fn put(&self, account_id: &str, cred: &Credential) -> Result<(), SecretStoreError>;
    /// Fetch the credential stored under `account_id`, if present.
    fn get(&self, account_id: &str) -> Result<Option<Credential>, SecretStoreError>;
    /// Remove the credential stored under `account_id`.
    fn delete(&self, account_id: &str) -> Result<(), SecretStoreError>;
    /// List all stored account ids (unspecified order).
    fn list(&self) -> Result<Vec<String>, SecretStoreError>;
}

/// On-disk representation: a flat JSON object keyed by account id.
type DiskMap = BTreeMap<String, Credential>;

/// File-backed [`SecretStore`] with atomic writes and last-good backup.
pub struct FileSecretStore {
    path: PathBuf,
    /// Serializes the read-modify-write cycle; there are no await points,
    /// so a `std` mutex suffices.
    lock: Arc<Mutex<()>>,
}

impl FileSecretStore {
    /// Create a store backed by `path` (e.g. `<config_dir>/secrets.json`).
    /// Does not touch the filesystem; the parent directory and file are
    /// created lazily on the first write.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn backup_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(".bak");
        PathBuf::from(p)
    }

    fn tmp_path(&self) -> PathBuf {
        let mut p = self.path.clone().into_os_string();
        p.push(format!(".tmp-{}", std::process::id()));
        PathBuf::from(p)
    }

    /// Parse a store file; `Ok(None)` when the file does not exist (fresh
    /// store is not an error).
    fn read_file(path: &PathBuf) -> Result<Option<DiskMap>, SecretStoreError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let parsed: DiskMap = serde_json::from_slice(&bytes)?;
        Ok(Some(parsed))
    }

    /// Load the current map, falling back to `.bak` when the primary file is
    /// corrupted. A missing primary file is a fresh (empty) store, not an
    /// error; a corrupted primary with no usable backup is `Err`.
    fn load(&self) -> Result<DiskMap, SecretStoreError> {
        match Self::read_file(&self.path) {
            Ok(Some(map)) => Ok(map),
            Ok(None) => Ok(DiskMap::new()),
            Err(_) => match Self::read_file(&self.backup_path()) {
                Ok(Some(map)) => Ok(map),
                Ok(None) => Err(SecretStoreError::NotFound(self.backup_path())),
                Err(e) => Err(e),
            },
        }
    }

    /// Persist `map` atomically: `0600` temp file → back up current → rename.
    fn save(&self, map: &DiskMap) -> Result<(), SecretStoreError> {
        let bytes = serde_json::to_vec_pretty(map)?;

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

        let tmp = self.tmp_path();
        fs::write(&tmp, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }

        // Best-effort last-good backup: the committed state, taken after the
        // rename succeeds so `.bak` always mirrors a fully written file.
        // (Atomic on POSIX via same-directory rename; on Windows `rename`
        // fails when the destination exists, so remove first.)
        #[cfg(windows)]
        let _ = fs::remove_file(&self.path);
        fs::rename(&tmp, &self.path)?;
        if self.path.exists() {
            let _ = fs::copy(&self.path, self.backup_path());
        }
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn put(&self, account_id: &str, cred: &Credential) -> Result<(), SecretStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load()?;
        map.insert(account_id.to_string(), cred.clone());
        self.save(&map)
    }

    fn get(&self, account_id: &str) -> Result<Option<Credential>, SecretStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.get(account_id).cloned())
    }

    fn delete(&self, account_id: &str) -> Result<(), SecretStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load()?;
        if map.remove(account_id).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.into_keys().collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Unique temp dir per test; removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "any-converter-secret-store-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn api_key() -> Credential {
        Credential::ApiKey {
            key: "sk-test-123".into(),
        }
    }

    fn oauth() -> Credential {
        Credential::OAuth {
            provider: "gemini".into(),
            account: "me@example.com".into(),
            access_token: "ya29.test-access".into(),
            refresh_token: Some("1//test-refresh".into()),
            expires_at_unix: Some(1_900_000_000),
        }
    }

    #[test]
    fn round_trip_api_key_and_oauth() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("secrets.json"));

        store.put("api", &api_key()).unwrap();
        store.put("oauth", &oauth()).unwrap();

        assert_eq!(store.get("api").unwrap(), Some(api_key()));
        assert_eq!(store.get("oauth").unwrap(), Some(oauth()));
        assert_eq!(store.get("missing").unwrap(), None);
    }

    #[test]
    fn oauth_round_trip_preserves_refresh_fields() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("secrets.json"));

        store.put("gemini:me@example.com", &oauth()).unwrap();

        let loaded = store.get("gemini:me@example.com").unwrap().unwrap();
        assert_eq!(loaded, oauth());
        match loaded {
            Credential::OAuth {
                provider,
                account,
                access_token,
                refresh_token,
                expires_at_unix,
            } => {
                assert_eq!(provider, "gemini");
                assert_eq!(account, "me@example.com");
                assert_eq!(access_token, "ya29.test-access");
                assert_eq!(refresh_token.as_deref(), Some("1//test-refresh"));
                assert_eq!(expires_at_unix, Some(1_900_000_000));
            }
            other => panic!("expected OAuth credential, got {other:?}"),
        }
    }

    #[test]
    fn put_overwrites_same_id() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("secrets.json"));

        store.put("a", &api_key()).unwrap();
        store.put("a", &oauth()).unwrap();

        assert_eq!(store.get("a").unwrap(), Some(oauth()));
    }

    #[test]
    fn delete_then_get_none() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("secrets.json"));

        store.put("a", &api_key()).unwrap();
        store.delete("a").unwrap();

        assert_eq!(store.get("a").unwrap(), None);
        // Deleting a non-existent id is not an error.
        store.delete("never-existed").unwrap();
    }

    #[test]
    fn list_returns_all_ids() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("secrets.json"));

        store.put("b", &api_key()).unwrap();
        store.put("a", &oauth()).unwrap();
        store.put("c", &api_key()).unwrap();

        let mut ids = store.list().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let path = dir.path("secrets.json");
        let store = FileSecretStore::new(path.clone());
        store.put("a", &api_key()).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "secrets file must be 0600");

        let residue: Vec<_> = glob_children(&dir.0)
            .into_iter()
            .filter(|p| {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string());
                matches!(name.as_deref(), Some(n) if n.contains(".tmp-"))
            })
            .collect();
        assert!(
            residue.is_empty(),
            "no tmp files may remain after put: {residue:?}"
        );
    }

    #[test]
    fn no_tmp_residue_after_puts() {
        let dir = TempDir::new();
        let path = dir.path("secrets.json");
        let store = FileSecretStore::new(path);

        store.put("a", &api_key()).unwrap();
        store.put("a", &oauth()).unwrap();
        store.delete("a").unwrap();

        let residue: Vec<_> = glob_children(&dir.0)
            .into_iter()
            .filter(|p| {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string());
                matches!(name.as_deref(), Some(n) if n.contains(".tmp-"))
            })
            .collect();
        assert!(residue.is_empty(), "tmp residue: {residue:?}");
    }

    #[test]
    fn bak_recovers_corrupted_main_file() {
        let dir = TempDir::new();
        let path = dir.path("secrets.json");
        let store = FileSecretStore::new(path.clone());

        store.put("a", &api_key()).unwrap();
        store.put("a", &oauth()).unwrap();

        // Corrupt the primary file; `.bak` holds the last-good state.
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(store.get("a").unwrap(), Some(oauth()));
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = TempDir::new();
        let store = FileSecretStore::new(dir.path("nested/sub/secrets.json"));

        assert_eq!(store.list().unwrap(), Vec::<String>::new());
        assert_eq!(store.get("x").unwrap(), None);
        store.delete("x").unwrap();
    }

    #[test]
    fn corrupted_main_and_bak_is_error() {
        let dir = TempDir::new();
        let path = dir.path("secrets.json");
        let store = FileSecretStore::new(path.clone());

        store.put("a", &api_key()).unwrap();
        fs::write(&path, "{ broken").unwrap();
        fs::write(dir.path("secrets.json.bak"), "}{ broken too").unwrap();

        assert!(store.get("a").is_err());
    }

    #[test]
    fn serde_tag_is_kind() {
        let dir = TempDir::new();
        let path = dir.path("secrets.json");
        let store = FileSecretStore::new(path.clone());
        store.put("a", &api_key()).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"kind\": \"api_key\""), "raw: {raw}");
    }

    fn glob_children(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                out.push(entry.path());
            }
        }
        out
    }
}
