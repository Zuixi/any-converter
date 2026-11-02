//! Writing rotated tokens back to the `auth.json` a Codex sign-in lives in
//! (P11b).
//!
//! Every write carries magpie's three protections (`codexStore.write`,
//! `account.go`), in full:
//!
//! 1. **Never overwrite what could not be read.** A missing `auth.json` is
//!    never created, and a file that exists but does not parse is refused
//!    ("sign-ins that don't read are not written over") — a bad file is
//!    never treated as an empty one and wiped.
//! 2. **Last-good backup.** The pre-write bytes are copied to
//!    `auth.json.bak` (magpie's lastgood generations), then the new
//!    contents go through a `0600` temp file and a same-directory rename,
//!    so a failure never leaves half a file.
//! 3. **Cross-process lock.** The whole read-modify-write runs under a
//!    [`crate::lockfile::FileLock`] at `auth.json.any-converter.lock`
//!    (magpie's `logins.json.codex.lock`), because the server and the CLI
//!    are separate processes: two processes refreshing the same sign-in
//!    concurrently make the vendor answer `refresh_token_reused`, which
//!    signs the user out of Codex.
//!
//! Only the store Codex's own config names is written: `file` (the
//! default) means `auth.json`; `keyring`, `auto` and `ephemeral` are
//! refused with guidance, and a config that cannot be parsed is refused
//! too (codex-rs `login/src/auth/storage.rs`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::lockfile::{DEFAULT_ACQUIRE_TIMEOUT, FileLock};

use super::store;

/// Where Codex keeps its sign-in, as its own config says
/// (`cli_auth_credentials_store`, codex-rs login/src/auth/storage.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexStore {
    /// `auth.json` inside the codex home (the default `file` store).
    File(PathBuf),
    /// The store Codex is configured for cannot be written back; the
    /// message says why and what to do instead.
    Refused(String),
}

/// Resolve the store for `home` (a CODEX_HOME).
///
/// Reads `home/config.toml` for `cli_auth_credentials_store`; without the
/// file (or the key) Codex uses `auth.json`, so that is what this returns.
/// `file` resolves to `home/auth.json`; every other store — `keyring`,
/// `auto`, `ephemeral` — refuses with guidance, and so does a config that
/// exists but cannot be parsed: refusing is the safe side, never a guess.
pub fn codex_store(home: &Path) -> CodexStore {
    let config = home.join("config.toml");
    let text = match fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Codex's default: auth.json in the codex home.
            return CodexStore::File(home.join("auth.json"));
        }
        Err(error) => {
            return CodexStore::Refused(format!(
                "can't read {}; refusing to write Codex's auth.json: {error}",
                config.display()
            ));
        }
    };
    let parsed: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return CodexStore::Refused(format!(
                "{} can't be parsed ({error}); refusing to write Codex's auth.json",
                config.display()
            ));
        }
    };
    match parsed
        .get("cli_auth_credentials_store")
        .and_then(toml::Value::as_str)
    {
        None | Some("file") => CodexStore::File(home.join("auth.json")),
        Some(store) => CodexStore::Refused(format!(
            "Codex is configured to keep this sign-in in `{store}` \
             (cli_auth_credentials_store in {}), not in auth.json; magpie never \
             writes keychains — set cli_auth_credentials_store = \"file\" in that \
             file and sign in again in Codex, or leave the store alone",
            config.display()
        )),
    }
}

/// The rotated token fields [`write_tokens`] puts into `auth.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUpdate {
    pub access_token: String,
    pub refresh_token: String,
    /// Written only when `Some` and non-empty; `None` keeps the file's
    /// current value.
    pub id_token: Option<String>,
}

/// Why a write-back did not happen.
#[derive(Debug, Error)]
pub enum WriteBackError {
    /// keyring/auto/ephemeral store, unparsable config, or an `auth.json`
    /// that does not exist or cannot be read.
    #[error("{0}")]
    Refused(String),
    /// Filesystem or lock failure.
    #[error("{0}")]
    Io(String),
}

/// Write rotated tokens back where Codex reads them.
///
/// Keeps every other field of `auth.json`; refuses when the file can't be
/// read. The whole operation runs under the cross-process lock and leaves
/// a `.bak` last-good copy behind (see the module docs).
pub fn write_tokens(home: &Path, tokens: &TokenUpdate) -> Result<(), WriteBackError> {
    write_tokens_with_timeout(home, tokens, DEFAULT_ACQUIRE_TIMEOUT)
}

/// [`write_tokens`] with an explicit lock wait (test seam: the production
/// path waits [`DEFAULT_ACQUIRE_TIMEOUT`]).
#[doc(hidden)]
pub fn write_tokens_with_timeout(
    home: &Path,
    tokens: &TokenUpdate,
    lock_timeout: Duration,
) -> Result<(), WriteBackError> {
    let auth_path = match codex_store(home) {
        CodexStore::File(path) => path,
        CodexStore::Refused(reason) => return Err(WriteBackError::Refused(reason)),
    };

    // The whole read-modify-write is under the cross-process lock: two
    // processes must never refresh (and write back) the same sign-in
    // concurrently — see the module docs.
    let lock = FileLock::new(lock_path(&auth_path));
    let _guard = lock
        .acquire_timeout(lock_timeout)
        .map_err(|error| WriteBackError::Io(error.to_string()))?;

    // Never create a sign-in, and never write over what could not be read.
    let bytes = match fs::read(&auth_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(WriteBackError::Refused(format!(
                "no sign-in to update: {} does not exist",
                auth_path.display()
            )));
        }
        Err(error) => return Err(WriteBackError::Io(error.to_string())),
    };
    let mut auth: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        WriteBackError::Refused(format!("auth.json can't be read; left as it is ({error})"))
    })?;
    let auth_obj = auth.as_object_mut().ok_or_else(|| {
        WriteBackError::Refused("auth.json is not a JSON object; left as it is".to_string())
    })?;
    let tokens_obj = auth_obj
        .get_mut("tokens")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            WriteBackError::Refused("auth.json has no `tokens` object; left as it is".to_string())
        })?;
    tokens_obj.insert(
        "access_token".to_string(),
        serde_json::Value::String(tokens.access_token.clone()),
    );
    tokens_obj.insert(
        "refresh_token".to_string(),
        serde_json::Value::String(tokens.refresh_token.clone()),
    );
    if let Some(id_token) = tokens
        .id_token
        .as_deref()
        .filter(|id_token| !id_token.is_empty())
    {
        tokens_obj.insert(
            "id_token".to_string(),
            serde_json::Value::String(id_token.to_string()),
        );
    }
    // The fourth field this module owns: when the sign-in was last
    // refreshed, stamped the way Codex stamps it.
    auth_obj.insert(
        "last_refresh".to_string(),
        serde_json::Value::String(
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        ),
    );

    let out =
        serde_json::to_vec_pretty(&auth).map_err(|error| WriteBackError::Io(error.to_string()))?;

    // Last-good backup: the bytes that were there before this write
    // (magpie's lastgood generation), then the atomic 0600 temp +
    // rename commit.
    fs::copy(&auth_path, backup_path(&auth_path))
        .map_err(|error| WriteBackError::Io(error.to_string()))?;
    store::write_private(&auth_path, &out)
        .map_err(|error| WriteBackError::Io(error.to_string()))?;
    Ok(())
}

/// The cross-process lock path: `auth.json.any-converter.lock`
/// (magpie's `logins.json.codex.lock`).
fn lock_path(auth_path: &Path) -> PathBuf {
    let mut path = auth_path.as_os_str().to_owned();
    path.push(".any-converter.lock");
    PathBuf::from(path)
}

/// The last-good backup path: `auth.json.bak`.
fn backup_path(auth_path: &Path) -> PathBuf {
    let mut path = auth_path.as_os_str().to_owned();
    path.push(".bak");
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Unique temp codex home per test; removed on drop.
    struct TempCodexHome {
        _dir: tempfile::TempDir,
        home: PathBuf,
    }

    impl TempCodexHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join(".codex");
            fs::create_dir_all(&home).unwrap();
            Self { _dir: dir, home }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.home.join(name)
        }
    }

    /// A fresh home distinct from any other test's (unit tests run in
    /// parallel).
    fn fresh_home() -> TempCodexHome {
        let _ = SEQ.fetch_add(1, Ordering::SeqCst);
        TempCodexHome::new()
    }

    #[test]
    fn no_config_toml_defaults_to_auth_json() {
        let home = fresh_home();
        assert_eq!(
            codex_store(&home.home),
            CodexStore::File(home.path("auth.json"))
        );
    }

    #[test]
    fn file_store_is_auth_json() {
        let home = fresh_home();
        fs::write(
            home.path("config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();
        assert_eq!(
            codex_store(&home.home),
            CodexStore::File(home.path("auth.json"))
        );
    }

    #[test]
    fn keyring_store_refuses_with_guidance() {
        let home = fresh_home();
        fs::write(
            home.path("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        let CodexStore::Refused(reason) = codex_store(&home.home) else {
            panic!("keyring store must refuse");
        };
        assert!(reason.contains("keyring"), "reason: {reason}");
        assert!(
            reason.contains("cli_auth_credentials_store = \"file\""),
            "reason: {reason}"
        );
    }

    #[test]
    fn auto_and_ephemeral_stores_refuse() {
        for store in ["auto", "ephemeral"] {
            let home = fresh_home();
            fs::write(
                home.path("config.toml"),
                format!("cli_auth_credentials_store = \"{store}\"\n"),
            )
            .unwrap();
            assert!(
                matches!(codex_store(&home.home), CodexStore::Refused(_)),
                "{store} store must refuse"
            );
        }
    }

    #[test]
    fn unparsable_config_refuses() {
        let home = fresh_home();
        fs::write(home.path("config.toml"), "not [ valid toml").unwrap();
        assert!(matches!(codex_store(&home.home), CodexStore::Refused(_)));
    }

    #[test]
    fn missing_key_defaults_to_auth_json() {
        let home = fresh_home();
        fs::write(home.path("config.toml"), "model = \"gpt-5-codex\"\n").unwrap();
        assert_eq!(
            codex_store(&home.home),
            CodexStore::File(home.path("auth.json"))
        );
    }
}
