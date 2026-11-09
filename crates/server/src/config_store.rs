//! The single writer for the authoritative `config.toml`.
//!
//! Configuration is deliberately kept as a human-editable TOML document at
//! rest. The desktop UI, CLI and HTTP management API must go through
//! [`ConfigStore`] instead of reading or writing that file independently.
//!
//! The store separates two concepts:
//!
//! - [`ConfigDocument`] is the serializable, human-editable form. It may
//!   contain secret *references*, but never resolved runtime credentials.
//! - [`ConfigDocument::config`] is the parsed `ServerConfig` snapshot used by
//!   callers to validate and construct runtime state.
//!
//! Writes use a same-directory temporary file, an optional last-good backup,
//! and a cross-process lock. A compare-and-swap revision check prevents two
//! management surfaces from silently overwriting each other's edits.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::{Document, Item, Table};

use crate::config::ServerConfig;
use crate::home::Home;
use crate::lockfile::{DEFAULT_ACQUIRE_TIMEOUT, InstanceLock, InstanceLockGuard};

const REDACTED: &str = "<redacted>";
const CONFIG_LOCK_SUFFIX: &str = ".lock";
const CONFIG_BACKUP_SUFFIX: &str = ".bak";

/// A content-addressed revision of the exact bytes on disk.
///
/// The value is deliberately opaque to clients. Treat it as an ETag: send it
/// back unchanged on the next write.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ConfigRevision(String);

impl ConfigRevision {
    /// The revision as a lowercase hexadecimal string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn for_source(source: &str) -> Self {
        let digest = Sha256::digest(source.as_bytes());
        let mut encoded = String::with_capacity(digest.len() * 2);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in digest {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Self(encoded)
    }
}

impl fmt::Debug for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigRevision")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ConfigRevision {
    type Err = ConfigRevisionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ConfigRevisionError);
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for ConfigRevision {
    type Error = ConfigRevisionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl TryFrom<&str> for ConfigRevision {
    type Error = ConfigRevisionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Serialize for ConfigRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ConfigRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("config revision must be exactly 64 lowercase hexadecimal characters")]
pub struct ConfigRevisionError;

/// A validated TOML snapshot.
///
/// The raw source and the parsed config are kept together so a caller cannot
/// accidentally validate one document and persist another. Mutating the
/// config requires producing a new TOML document and going through
/// [`ConfigStore::write_if_revision`].
#[derive(Clone)]
pub struct ConfigDocument {
    source: String,
    document: Document,
    config: ServerConfig,
    revision: ConfigRevision,
}

impl fmt::Debug for ConfigDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigDocument")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl ConfigDocument {
    /// Parse a TOML document without touching the filesystem.
    ///
    /// Structural TOML errors are returned immediately. Semantic validation
    /// is available through [`Self::validate`] and is enforced on writes.
    pub fn parse(source: impl Into<String>) -> Result<Self, ConfigStoreError> {
        let source = source.into();
        let document = source.parse::<Document>()?;
        if contains_redacted_secret(&document) {
            return Err(ConfigStoreError::RedactedSecret);
        }
        let config = ServerConfig::from_toml(&source)?;
        let revision = ConfigRevision::for_source(&source);
        Ok(Self {
            source,
            document,
            config,
            revision,
        })
    }

    /// The exact TOML source represented by this snapshot.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn into_toml_document(self) -> Document {
        self.document
    }

    /// The parsed, immutable configuration snapshot.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The revision of [`Self::source`].
    pub fn revision(&self) -> &ConfigRevision {
        &self.revision
    }

    /// Semantic validation errors, if any.
    pub fn validate(&self) -> Vec<String> {
        self.config.validate()
    }

    /// The management-key reference configured under `[management]`.
    pub fn management_key_ref(&self) -> Option<String> {
        document_string(&self.document, &["management", "api_key"])
    }

    /// A read-only view safe enough to return from a browser or desktop UI.
    ///
    /// Secret values are removed from the embedded TOML. The view is
    /// intentionally not round-trippable: an update must be based on an
    /// authorized source document, not on this redacted representation.
    pub fn public_sanitized_view(&self) -> SanitizedConfigView {
        let mut document = self.document.clone();
        redact_document(&mut document);
        SanitizedConfigView {
            revision: self.revision.clone(),
            document: document.to_string(),
            server_api_key_configured: self
                .config
                .server
                .api_key
                .as_deref()
                .is_some_and(|key| !key.is_empty()),
        }
    }
}

/// A redacted configuration view. It is safe for read-only display, but it
/// must not be sent back as a replacement document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedConfigView {
    revision: ConfigRevision,
    document: String,
    server_api_key_configured: bool,
}

impl SanitizedConfigView {
    pub fn revision(&self) -> &ConfigRevision {
        &self.revision
    }

    pub fn document(&self) -> &str {
        &self.document
    }

    pub fn server_api_key_configured(&self) -> bool {
        self.server_api_key_configured
    }
}

/// The authoritative config file store for one installation.
#[derive(Clone)]
pub struct ConfigStore {
    path: PathBuf,
    backup_path: PathBuf,
    lock: InstanceLock,
}

impl ConfigStore {
    /// Create a store for an explicit config path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            backup_path: append_suffix(&path, CONFIG_BACKUP_SUFFIX),
            lock: InstanceLock::new(append_suffix(&path, CONFIG_LOCK_SUFFIX)),
            path,
        }
    }

    /// Create a store using the canonical path derived from `home`.
    pub fn from_home(home: &Home) -> Self {
        Self::new(home.config_path())
    }

    /// The authoritative TOML path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last-good backup path (`.bak` beside the config).
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    /// The cross-process lock path.
    pub fn lock_path(&self) -> PathBuf {
        append_suffix(&self.path, CONFIG_LOCK_SUFFIX)
    }

    /// Load an existing config.
    pub fn load(&self) -> Result<ConfigDocument, ConfigStoreError> {
        self.load_if_exists()?
            .ok_or_else(|| ConfigStoreError::NotFound(self.path.clone()))
    }

    /// Load a config, returning `Ok(None)` when the file does not exist.
    pub fn load_if_exists(&self) -> Result<Option<ConfigDocument>, ConfigStoreError> {
        // Readers deliberately do not take the writer lock. Atomic replacement
        // means a reader observes either the old complete file or the new
        // complete file, never a torn write. A read lock would only make
        // status requests wait behind an editor.
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let source = String::from_utf8(bytes).map_err(|_| ConfigStoreError::NotUtf8 {
            path: self.path.clone(),
        })?;
        ConfigDocument::parse(source).map(Some)
    }

    /// Load the last-good backup without modifying the primary file.
    ///
    /// Recovery is explicit: silently replacing a corrupt primary could hide
    /// an operator's manual edit.
    pub fn load_backup(&self) -> Result<ConfigDocument, ConfigStoreError> {
        let bytes = match fs::read(&self.backup_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ConfigStoreError::BackupNotFound(self.backup_path.clone()));
            }
            Err(error) => return Err(error.into()),
        };
        let source = String::from_utf8(bytes).map_err(|_| ConfigStoreError::NotUtf8 {
            path: self.backup_path.clone(),
        })?;
        ConfigDocument::parse(source)
    }

    /// Atomically replace the config if [`ConfigRevision`] still matches.
    ///
    /// `expected = None` means "the file must not exist yet". This is
    /// intentionally stricter than an unconditional write: every mutating
    /// surface must either create a new document or pass the revision it
    /// read. A mismatch returns [`ConfigStoreError::Conflict`] without
    /// changing the file.
    pub fn write_if_revision(
        &self,
        expected: Option<&ConfigRevision>,
        source: &str,
    ) -> Result<ConfigDocument, ConfigStoreError> {
        let _guard = self.lock.acquire_timeout(DEFAULT_ACQUIRE_TIMEOUT)?;
        self.write_if_revision_locked(&_guard, expected, source)
    }

    pub(crate) fn write_if_revision_locked(
        &self,
        _guard: &InstanceLockGuard,
        expected: Option<&ConfigRevision>,
        source: &str,
    ) -> Result<ConfigDocument, ConfigStoreError> {
        let current = self.load_if_exists()?;
        let actual = current.as_ref().map(|document| document.revision.clone());
        if actual.as_ref() != expected {
            return Err(ConfigStoreError::Conflict {
                expected: expected.cloned(),
                actual,
            });
        }

        let next = ConfigDocument::parse(source.to_owned())?;
        let errors = next.validate();
        if !errors.is_empty() {
            return Err(ConfigStoreError::Validation(errors.join("; ")));
        }

        self.atomic_replace(next.source().as_bytes())?;
        Ok(next)
    }

    /// Apply a typed document mutation under the same lock and CAS checks as
    /// [`Self::write_if_revision`].
    ///
    /// Typed management operations use this path. Editing the existing
    /// document preserves comments and unrelated TOML keys.
    pub(crate) fn mutate_if_revision(
        &self,
        expected: Option<&ConfigRevision>,
        mutate: impl FnOnce(&mut Document) -> Result<(), ConfigStoreError>,
    ) -> Result<ConfigDocument, ConfigStoreError> {
        let _guard = self.lock.acquire_timeout(DEFAULT_ACQUIRE_TIMEOUT)?;
        self.mutate_if_revision_locked(&_guard, expected, mutate)
    }

    pub(crate) fn mutate_if_revision_locked(
        &self,
        _guard: &InstanceLockGuard,
        expected: Option<&ConfigRevision>,
        mutate: impl FnOnce(&mut Document) -> Result<(), ConfigStoreError>,
    ) -> Result<ConfigDocument, ConfigStoreError> {
        let current = self.load_if_exists()?;
        let actual = current.as_ref().map(|document| document.revision.clone());
        if actual.as_ref() != expected {
            return Err(ConfigStoreError::Conflict {
                expected: expected.cloned(),
                actual,
            });
        }

        let mut document = current
            .as_ref()
            .map(|document| document.document.clone())
            .unwrap_or_default();
        mutate(&mut document)?;
        let next = ConfigDocument::parse(document.to_string())?;
        let errors = next.validate();
        if !errors.is_empty() {
            return Err(ConfigStoreError::Validation(errors.join("; ")));
        }
        self.atomic_replace(next.source().as_bytes())?;
        Ok(next)
    }

    pub(crate) fn acquire_write_lock(&self) -> Result<InstanceLockGuard, ConfigStoreError> {
        if let Some(parent) = self.lock_path().parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(self.lock.acquire_timeout(DEFAULT_ACQUIRE_TIMEOUT)?)
    }

    fn atomic_replace(&self, bytes: &[u8]) -> Result<(), ConfigStoreError> {
        let parent = self.path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            )
        })?;
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }

        self.write_temp_and_commit(bytes)
    }

    fn write_temp_and_commit(&self, bytes: &[u8]) -> Result<(), ConfigStoreError> {
        if self.path.exists() {
            fs::copy(&self.path, &self.backup_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.backup_path, fs::Permissions::from_mode(0o600))?;
            }
            sync_file(&self.backup_path)?;
        }

        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        // `atomicwrites` uses MoveFileEx with REPLACE_EXISTING on Windows.
        // A bare `std::fs::rename` is not a replacement there and would fail
        // once the config exists.
        AtomicFile::new(&self.path, AllowOverwrite)
            .write_with_options(
                |file| {
                    file.write_all(bytes)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(fs::Permissions::from_mode(0o600))?;
                    }
                    Ok::<_, io::Error>(())
                },
                options,
            )
            .map_err(atomic_write_error)?;
        sync_parent(&self.path)?;
        Ok(())
    }
}

/// Errors from config loading, parsing, validation, or replacement.
#[derive(Debug, Error)]
pub enum ConfigStoreError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),
    #[error("config backup not found: {0}")]
    BackupNotFound(PathBuf),
    #[error("config file is not valid UTF-8: {path}")]
    NotUtf8 { path: PathBuf },
    #[error("invalid TOML document: {0}")]
    InvalidDocument(#[from] toml_edit::TomlError),
    #[error("invalid server config: {0}")]
    InvalidConfig(#[from] toml::de::Error),
    #[error(
        "config contains <redacted> in a secret field; a sanitized read view cannot be written back"
    )]
    RedactedSecret,
    #[error("config validation failed: {0}")]
    Validation(String),
    #[error("config mutation failed: {0}")]
    Mutation(String),
    #[error("config revision conflict: expected {expected:?}, found {actual:?}")]
    Conflict {
        expected: Option<ConfigRevision>,
        actual: Option<ConfigRevision>,
    },
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn document_string(document: &Document, path: &[&str]) -> Option<String> {
    let (first, rest) = path.split_first()?;
    let mut item = document.get(first)?;
    for key in rest {
        item = item.get(key)?;
    }
    item.as_str().map(str::to_owned)
}

fn contains_redacted_secret(document: &Document) -> bool {
    let server_secret = document
        .get("server")
        .and_then(Item::as_table)
        .and_then(|server| server.get("api_key"))
        .and_then(Item::as_str)
        == Some(REDACTED);
    let management_secret = document
        .get("management")
        .and_then(Item::as_table)
        .and_then(|management| management.get("api_key"))
        .and_then(Item::as_str)
        == Some(REDACTED);
    let provider_secret = document
        .get("providers")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|providers| {
            providers
                .iter()
                .filter_map(|provider| provider.get("api_key"))
                .filter_map(Item::as_str)
                .any(|value| value == REDACTED)
        });

    server_secret || management_secret || provider_secret
}

fn redact_document(document: &mut Document) {
    if let Some(management) = document.get_mut("management").and_then(Item::as_table_mut) {
        redact_key(management, "api_key");
    }
    if let Some(server) = document.get_mut("server").and_then(Item::as_table_mut) {
        redact_key(server, "api_key");
        redact_key(server, "proxy");
    }

    if let Some(providers) = document
        .get_mut("providers")
        .and_then(Item::as_array_of_tables_mut)
    {
        for provider in providers.iter_mut() {
            redact_key(provider, "api_key");
            redact_key(provider, "proxy");
            if let Some(auth) = provider.get_mut("auth").and_then(Item::as_table_mut) {
                if let Some(headers) = auth.get_mut("headers").and_then(Item::as_table_mut) {
                    for (_, header) in headers.iter_mut() {
                        redact_item(header);
                    }
                }
            }
        }
    }
}

fn atomic_write_error(error: atomicwrites::Error<io::Error>) -> ConfigStoreError {
    match error {
        atomicwrites::Error::Internal(error) | atomicwrites::Error::User(error) => {
            ConfigStoreError::Io(error)
        }
    }
}

fn redact_key(table: &mut Table, key: &str) {
    if let Some(item) = table.get_mut(key) {
        redact_item(item);
    }
}

fn redact_item(item: &mut Item) {
    let should_redact = match item.as_value() {
        Some(toml_edit::Value::String(value)) => !value.value().is_empty(),
        Some(_) => true,
        None => false,
    };
    if should_redact {
        *item = toml_edit::value(REDACTED);
    }
}

#[cfg(unix)]
fn sync_file(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "any-converter-config-store-{}-{}",
                std::process::id(),
                TEST_DIR_SEQUENCE.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const INITIAL: &str = r#"
# keep this comment
[server]
host = "127.0.0.1"
port = 1228
api_key = "server-secret"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "provider-secret"

[providers.auth.headers]
Authorization = "Bearer header-secret"

[[model_routes]]
pattern = "*"
provider = "openai"
"#;

    fn config_with_port(port: u16) -> String {
        INITIAL.replace("port = 1228", &format!("port = {port}"))
    }

    #[test]
    fn revision_is_stable_for_exact_bytes() {
        let first = ConfigDocument::parse(INITIAL).unwrap();
        let second = ConfigDocument::parse(INITIAL).unwrap();
        let changed = ConfigDocument::parse(config_with_port(1229)).unwrap();

        assert_eq!(first.revision(), second.revision());
        assert_ne!(first.revision(), changed.revision());
        assert_eq!(first.revision().as_str().len(), 64);
    }

    #[test]
    fn revision_parser_requires_exact_lowercase_hex() {
        let document = ConfigDocument::parse(INITIAL).unwrap();
        let encoded = document.revision().as_str();

        assert_eq!(
            encoded.parse::<ConfigRevision>().unwrap(),
            *document.revision()
        );
        assert!("abc".parse::<ConfigRevision>().is_err());
        assert!(
            encoded
                .to_ascii_uppercase()
                .parse::<ConfigRevision>()
                .is_err()
        );
        assert!(format!("{encoded}0").parse::<ConfigRevision>().is_err());
        assert!(
            encoded
                .chars()
                .enumerate()
                .map(|(index, ch)| if index == 0 { 'g' } else { ch })
                .collect::<String>()
                .parse::<ConfigRevision>()
                .is_err()
        );
    }

    #[test]
    fn debug_output_never_contains_source_secrets() {
        let document = ConfigDocument::parse(INITIAL).unwrap();
        let rendered = format!("{document:?}");

        assert!(rendered.contains("ConfigDocument"));
        assert!(!rendered.contains("server-secret"));
        assert!(!rendered.contains("provider-secret"));
        assert!(!rendered.contains("header-secret"));
    }

    #[test]
    fn write_requires_matching_revision_and_creates_backup() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        let first = store.write_if_revision(None, INITIAL).unwrap();

        let conflict = store.write_if_revision(None, INITIAL).unwrap_err();
        assert!(matches!(
            conflict,
            ConfigStoreError::Conflict {
                expected: None,
                actual: Some(_)
            }
        ));

        let second_source = config_with_port(1229);
        let second = store
            .write_if_revision(Some(first.revision()), &second_source)
            .unwrap();
        assert_ne!(first.revision(), second.revision());
        assert_eq!(fs::read_to_string(store.path()).unwrap(), second_source);
        assert_eq!(fs::read_to_string(store.backup_path()).unwrap(), INITIAL);
        assert!(
            fs::read_dir(&dir.0)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    !name.contains(".tmp-") && !name.contains(".atomicwrite")
                }),
            "atomic writes must not leave temp files behind"
        );
    }

    #[test]
    fn validation_failure_does_not_replace_the_document() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        let first = store.write_if_revision(None, INITIAL).unwrap();

        let invalid = r#"
[server]
host = "127.0.0.1"
port = 1228

[[model_routes]]
pattern = "*"
provider = "missing"
"#;
        let error = store
            .write_if_revision(Some(first.revision()), invalid)
            .unwrap_err();
        assert!(
            matches!(error, ConfigStoreError::Validation(_)),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(store.path()).unwrap(), INITIAL);
    }

    #[test]
    fn public_view_redacts_secrets_and_keeps_comments() {
        let document = ConfigDocument::parse(INITIAL).unwrap();
        let view = document.public_sanitized_view();

        assert!(view.document().contains("# keep this comment"));
        assert!(!view.document().contains("server-secret"));
        assert!(!view.document().contains("provider-secret"));
        assert!(!view.document().contains("header-secret"));
        assert!(view.server_api_key_configured());
        assert_eq!(view.revision(), document.revision());
    }

    #[test]
    fn sanitized_view_cannot_be_written_back() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        let first = store.write_if_revision(None, INITIAL).unwrap();
        let sanitized = first.public_sanitized_view();

        let error = store
            .write_if_revision(Some(first.revision()), sanitized.document())
            .unwrap_err();
        assert!(
            matches!(error, ConfigStoreError::RedactedSecret),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(store.path()).unwrap(), INITIAL);
    }

    #[test]
    fn invalid_utf8_is_reported_without_modifying_the_file() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        fs::write(store.path(), [0xff, 0xfe, 0xfd]).unwrap();

        let error = store.load().unwrap_err();
        assert!(
            matches!(error, ConfigStoreError::NotUtf8 { ref path } if path == store.path()),
            "{error:?}"
        );
        assert_eq!(fs::read(store.path()).unwrap(), vec![0xff, 0xfe, 0xfd]);
    }

    #[test]
    fn explicit_backup_load_does_not_replace_the_primary() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        let first = store.write_if_revision(None, INITIAL).unwrap();
        let second_source = config_with_port(1229);
        store
            .write_if_revision(Some(first.revision()), &second_source)
            .unwrap();
        fs::write(store.path(), "{ broken").unwrap();

        let backup = store.load_backup().unwrap();
        assert_eq!(backup.source(), INITIAL);
        assert_eq!(fs::read_to_string(store.path()).unwrap(), "{ broken");
    }

    #[test]
    fn concurrent_cas_writers_allow_exactly_one_revision_to_win() {
        let dir = TestDir::new();
        let store = ConfigStore::new(dir.config_path());
        let initial = store.write_if_revision(None, INITIAL).unwrap();
        let revision = initial.revision().clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        let mut handles = Vec::new();
        for port in [1230_u16, 1231_u16] {
            let store = store.clone();
            let revision = revision.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.write_if_revision(Some(&revision), &config_with_port(port))
            }));
        }
        barrier.wait();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one CAS writer must win: {results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ConfigStoreError::Conflict { .. })))
                .count(),
            1
        );
        let final_document = store.load().unwrap();
        assert_ne!(final_document.revision(), &revision);
    }

    #[test]
    fn missing_file_is_reported_and_does_not_create_directories() {
        let dir = TestDir::new();
        let path = dir.0.join("nested").join("config.toml");
        let store = ConfigStore::new(&path);

        assert!(matches!(
            store.load().unwrap_err(),
            ConfigStoreError::NotFound(_)
        ));
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn first_write_creates_a_missing_canonical_home() {
        let dir = TestDir::new();
        let path = dir.0.join("nested").join("config.toml");
        let store = ConfigStore::new(&path);

        let lock = store.acquire_write_lock().unwrap();
        assert!(path.parent().unwrap().exists());
        drop(lock);
        let document = store
            .write_if_revision(None, &config_with_port(1228))
            .unwrap();

        assert_eq!(document.config().server.port, 1228);
        assert!(path.exists());
    }
}
