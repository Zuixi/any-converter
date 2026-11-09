//! Server-owned API-key references and system-keychain access.
//!
//! `config.toml` may contain references, but never resolved provider,
//! inference, or management credentials. All surfaces use this module so the
//! namespace and fail-closed behavior cannot drift.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use keyring::Entry;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::home::Home;

pub const KEYCHAIN_SERVICE: &str = "any-converter";
pub const KEYCHAIN_PREFIX: &str = "keychain:any-converter:";

const PROVIDER_ACCOUNT_PREFIX: &str = "provider:";
const INFERENCE_ACCOUNT: &str = "server:inference";
const MANAGEMENT_ACCOUNT: &str = "server:management";

/// A typed reference to a credential in the operating-system keychain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    Provider(String),
    Inference,
    Management,
    /// The pre-`provider:` namespace. Read and delete only; new writes use
    /// [`SecretRef::Provider`].
    LegacyProvider(String),
}

impl SecretRef {
    /// Parse a keychain reference.
    ///
    /// A literal API key returns `Ok(None)`. Anything that starts with
    /// `keychain:` but does not match a supported shape is an error rather
    /// than a credential that might accidentally be sent upstream.
    pub fn parse(value: &str) -> Result<Option<Self>, SecretRefError> {
        if !value.starts_with("keychain:") {
            return Ok(None);
        }
        let account = value
            .strip_prefix(KEYCHAIN_PREFIX)
            .ok_or_else(|| SecretRefError::Unsupported(value.to_owned()))?;
        if account == MANAGEMENT_ACCOUNT {
            return Ok(Some(Self::Management));
        }
        if account == INFERENCE_ACCOUNT {
            return Ok(Some(Self::Inference));
        }
        if let Some(provider) = account.strip_prefix(PROVIDER_ACCOUNT_PREFIX) {
            return validate_name(provider, value).map(Self::Provider).map(Some);
        }
        validate_name(account, value)
            .map(Self::LegacyProvider)
            .map(Some)
    }

    /// The canonical reference written to `config.toml`.
    pub fn canonical(&self) -> String {
        match self {
            Self::Provider(provider) => {
                format!("{KEYCHAIN_PREFIX}{PROVIDER_ACCOUNT_PREFIX}{provider}")
            }
            Self::Inference => format!("{KEYCHAIN_PREFIX}{INFERENCE_ACCOUNT}"),
            Self::Management => format!("{KEYCHAIN_PREFIX}{MANAGEMENT_ACCOUNT}"),
            Self::LegacyProvider(provider) => format!("{KEYCHAIN_PREFIX}{provider}"),
        }
    }

    fn primary_account(&self) -> &str {
        match self {
            Self::Provider(provider) => provider,
            Self::Inference => INFERENCE_ACCOUNT,
            Self::Management => MANAGEMENT_ACCOUNT,
            Self::LegacyProvider(provider) => provider,
        }
    }

    fn uses_provider_namespace(&self) -> bool {
        matches!(self, Self::Provider(_))
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical())
    }
}

fn validate_name(name: &str, original: &str) -> Result<String, SecretRefError> {
    if name.is_empty() || name.contains(':') || name.contains('/') {
        return Err(SecretRefError::Unsupported(original.to_owned()));
    }
    Ok(name.to_owned())
}

/// Reference syntax failure. The value is not itself secret material.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SecretRefError {
    #[error(
        "unsupported keychain reference `{0}`; expected keychain:any-converter:provider:<name>, keychain:any-converter:server:inference, or keychain:any-converter:server:management"
    )]
    Unsupported(String),
}

/// Minimal keychain backend. The production backend is [`SystemKeychain`];
/// tests and preview shells can use [`MemoryKeychain`].
pub trait KeychainBackend: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError>;
}

/// Operating-system keychain backed by the `keyring` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemKeychain;

impl KeychainBackend for SystemKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
        let entry = Entry::new(service, account).map_err(|source| SecretError::Keychain {
            operation: "open",
            source,
        })?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Keychain {
                operation: "read",
                source,
            }),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), SecretError> {
        let entry = Entry::new(service, account).map_err(|source| SecretError::Keychain {
            operation: "open",
            source,
        })?;
        entry
            .set_password(value)
            .map_err(|source| SecretError::Keychain {
                operation: "write",
                source,
            })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
        let entry = Entry::new(service, account).map_err(|source| SecretError::Keychain {
            operation: "open",
            source,
        })?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(SecretError::Keychain {
                operation: "delete",
                source,
            }),
        }
    }
}

/// In-memory backend for deterministic tests.
#[derive(Debug, Default)]
pub struct MemoryKeychain {
    values: Mutex<HashMap<(String, String), String>>,
}

impl MemoryKeychain {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), String>> {
        self.values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl KeychainBackend for MemoryKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self
            .lock()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), SecretError> {
        self.lock()
            .insert((service.to_owned(), account.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
        self.lock()
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// The server's API-key authority.
#[derive(Clone)]
pub struct ApiKeyStore {
    service: String,
    backend: Arc<dyn KeychainBackend>,
}

impl ApiKeyStore {
    pub fn system() -> Self {
        Self::with_backend(KEYCHAIN_SERVICE, Arc::new(SystemKeychain))
    }

    pub fn for_home(home: &Home) -> Self {
        Self::for_home_with_backend(home, Arc::new(SystemKeychain))
    }

    pub fn for_home_with_backend(home: &Home, backend: Arc<dyn KeychainBackend>) -> Self {
        let service = if home.uses_default_root() {
            KEYCHAIN_SERVICE.to_owned()
        } else {
            custom_home_keychain_service(home.root())
        };
        Self::with_backend(service, backend)
    }

    pub fn with_backend(service: impl Into<String>, backend: Arc<dyn KeychainBackend>) -> Self {
        Self {
            service: service.into(),
            backend,
        }
    }

    /// Read a reference without accepting an empty value as a credential.
    pub fn get(&self, reference: &SecretRef) -> Result<String, SecretError> {
        self.try_get(reference)?
            .ok_or_else(|| SecretError::Missing(reference.clone()))
    }

    /// Read a reference, returning `None` when the entry is absent or empty.
    pub fn try_get(&self, reference: &SecretRef) -> Result<Option<String>, SecretError> {
        if reference.uses_provider_namespace() {
            let canonical = self.backend.get(
                &self.service,
                &format!("{PROVIDER_ACCOUNT_PREFIX}{}", reference.primary_account()),
            )?;
            if let Some(value) = canonical.and_then(non_empty) {
                return Ok(Some(value));
            }
        }

        Ok(self
            .backend
            .get(&self.service, reference.primary_account())?
            .and_then(non_empty))
    }

    /// Resolve a TOML value. Literal keys are returned unchanged for legacy
    /// configurations; references fail closed when the entry is absent.
    pub fn resolve(&self, raw: &str) -> Result<Option<String>, SecretError> {
        match SecretRef::parse(raw)? {
            Some(reference) => self.get(&reference).map(Some),
            None => Ok(Some(raw.to_owned())),
        }
    }

    pub fn set_provider(&self, provider: &str, api_key: &str) -> Result<SecretRef, SecretError> {
        let reference = SecretRef::Provider(validate_provider_name(provider)?.to_owned());
        self.backend.set(
            &self.service,
            &format!("{PROVIDER_ACCOUNT_PREFIX}{provider}"),
            require_non_empty(api_key)?,
        )?;
        Ok(reference)
    }

    pub fn get_provider(&self, provider: &str) -> Result<String, SecretError> {
        self.get(&SecretRef::Provider(provider.to_owned()))
    }

    pub fn try_get_provider(&self, provider: &str) -> Result<Option<String>, SecretError> {
        self.try_get(&SecretRef::Provider(provider.to_owned()))
    }

    pub fn delete_provider(&self, provider: &str) -> Result<(), SecretError> {
        self.backend.delete(
            &self.service,
            &format!("{PROVIDER_ACCOUNT_PREFIX}{provider}"),
        )?;
        self.backend.delete(&self.service, provider)
    }

    pub fn set_inference(&self, api_key: &str) -> Result<SecretRef, SecretError> {
        self.backend.set(
            &self.service,
            INFERENCE_ACCOUNT,
            require_non_empty(api_key)?,
        )?;
        Ok(SecretRef::Inference)
    }

    pub fn get_inference(&self) -> Result<String, SecretError> {
        self.get(&SecretRef::Inference)
    }

    pub fn delete_inference(&self) -> Result<(), SecretError> {
        self.backend.delete(&self.service, INFERENCE_ACCOUNT)
    }

    pub fn set_management(&self, value: &str) -> Result<SecretRef, SecretError> {
        self.backend
            .set(&self.service, MANAGEMENT_ACCOUNT, require_non_empty(value)?)?;
        Ok(SecretRef::Management)
    }

    pub fn rotate_management(&self) -> Result<(SecretRef, String), SecretError> {
        let generated = generate_management_key();
        let reference = self.set_management(&generated)?;
        Ok((reference, generated))
    }

    pub fn get_management(&self) -> Result<String, SecretError> {
        self.get(&SecretRef::Management)
    }

    pub fn delete_management(&self) -> Result<(), SecretError> {
        self.backend.delete(&self.service, MANAGEMENT_ACCOUNT)
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn custom_home_keychain_service(root: &Path) -> String {
    let normalized = normalize_absolute_path(root);
    let text = normalized.to_string_lossy();
    #[cfg(windows)]
    let text = text.to_lowercase();
    let digest = Sha256::digest(text.as_bytes());
    let fingerprint = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{KEYCHAIN_SERVICE}-home-{fingerprint}")
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn require_non_empty(value: &str) -> Result<&str, SecretError> {
    if value.is_empty() {
        return Err(SecretError::Empty);
    }
    Ok(value)
}

fn validate_provider_name(name: &str) -> Result<&str, SecretError> {
    if name.is_empty() || name.contains(':') || name.contains('/') {
        return Err(SecretError::InvalidProviderName(name.to_owned()));
    }
    Ok(name)
}

fn generate_management_key() -> String {
    let first = Uuid::new_v4().simple();
    let second = Uuid::new_v4().simple();
    format!("acmgmt_{first}{second}")
}

/// Details gathered while resolving a runtime configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretResolutionReport {
    pub literal_provider_keys: Vec<String>,
    pub legacy_provider_refs: Vec<String>,
    pub literal_inference_key: bool,
}

/// Replace configured keychain references with their values.
///
/// Resolution is atomic with respect to the supplied config: on any missing
/// reference the function returns an error and the caller must not start.
pub fn resolve_runtime_secrets(
    config: &mut ServerConfig,
    keys: &ApiKeyStore,
) -> Result<SecretResolutionReport, SecretError> {
    let mut report = SecretResolutionReport::default();

    for provider in &mut config.providers {
        match SecretRef::parse(&provider.api_key)? {
            Some(SecretRef::Provider(_) | SecretRef::LegacyProvider(_)) => {
                if matches!(
                    SecretRef::parse(&provider.api_key)?,
                    Some(SecretRef::LegacyProvider(_))
                ) {
                    report.legacy_provider_refs.push(provider.name.clone());
                }
                provider.api_key = keys.resolve(&provider.api_key)?.ok_or_else(|| {
                    SecretError::Missing(SecretRef::Provider(provider.name.clone()))
                })?;
            }
            Some(SecretRef::Inference | SecretRef::Management) => {
                return Err(SecretRefError::Unsupported(provider.api_key.clone()).into());
            }
            None if !provider.api_key.is_empty() => {
                report.literal_provider_keys.push(provider.name.clone());
            }
            None => {}
        }
    }

    if let Some(api_key) = config.server.api_key.as_mut() {
        match SecretRef::parse(api_key)? {
            Some(SecretRef::Inference) => {
                *api_key = keys.get_inference()?;
            }
            Some(reference) => {
                return Err(SecretRefError::Unsupported(reference.canonical()).into());
            }
            None if !api_key.is_empty() => report.literal_inference_key = true,
            None => {}
        }
    }

    Ok(report)
}

#[derive(Debug, Error)]
pub enum SecretError {
    #[error(transparent)]
    Reference(#[from] SecretRefError),
    #[error(
        "no readable keychain entry for {0}; re-enter the credential or restore the OS keychain entry"
    )]
    Missing(SecretRef),
    #[error("refusing to store an empty API key")]
    Empty,
    #[error(
        "keychain entry for {0} already exists with a different value; resolve the conflict explicitly"
    )]
    Conflict(SecretRef),
    #[error("invalid provider name for a keychain reference: `{0}`")]
    InvalidProviderName(String),
    #[error("system keychain {operation} failed: {source}")]
    Keychain {
        operation: &'static str,
        #[source]
        source: keyring::Error,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::ServerConfig;

    fn store() -> (ApiKeyStore, Arc<MemoryKeychain>) {
        let backend = Arc::new(MemoryKeychain::new());
        (
            ApiKeyStore::with_backend(KEYCHAIN_SERVICE, backend.clone()),
            backend,
        )
    }

    #[test]
    fn custom_homes_use_isolated_keychain_namespaces() {
        let backend: Arc<dyn KeychainBackend> = Arc::new(MemoryKeychain::new());
        let first_home = Home::from_root(std::env::temp_dir().join("any-converter-home-a"));
        let second_home = Home::from_root(std::env::temp_dir().join("any-converter-home-b"));
        let first = ApiKeyStore::for_home_with_backend(&first_home, Arc::clone(&backend));
        let second = ApiKeyStore::for_home_with_backend(&second_home, backend);

        first.set_provider("openai", "sk-first").unwrap();

        assert_eq!(first.get_provider("openai").unwrap(), "sk-first");
        assert!(second.try_get_provider("openai").unwrap().is_none());
    }

    fn config(source: &str) -> ServerConfig {
        ServerConfig::from_toml(source).unwrap()
    }

    #[test]
    fn parses_canonical_and_legacy_references() {
        assert_eq!(
            SecretRef::parse("keychain:any-converter:provider:openai").unwrap(),
            Some(SecretRef::Provider("openai".into()))
        );
        assert_eq!(
            SecretRef::parse("keychain:any-converter:server:inference").unwrap(),
            Some(SecretRef::Inference)
        );
        assert_eq!(
            SecretRef::parse("keychain:any-converter:server:management").unwrap(),
            Some(SecretRef::Management)
        );
        assert_eq!(
            SecretRef::parse("keychain:any-converter:legacy").unwrap(),
            Some(SecretRef::LegacyProvider("legacy".into()))
        );
        assert_eq!(SecretRef::parse("sk-literal").unwrap(), None);
    }

    #[test]
    fn rejects_malformed_keychain_references_instead_of_sending_them_upstream() {
        for value in [
            "keychain:other:provider:openai",
            "keychain:any-converter:provider:",
            "keychain:any-converter:provider:a:b",
            "keychain:any-converter:",
        ] {
            assert!(SecretRef::parse(value).is_err(), "value={value}");
        }
    }

    #[test]
    fn provider_round_trip_and_legacy_fallback() {
        let (keys, backend) = store();
        let reference = keys.set_provider("openai", "sk-new").unwrap();
        assert_eq!(
            reference.canonical(),
            "keychain:any-converter:provider:openai"
        );
        assert_eq!(keys.get_provider("openai").unwrap(), "sk-new");

        backend.set(KEYCHAIN_SERVICE, "legacy", "sk-old").unwrap();
        assert_eq!(
            keys.get(&SecretRef::LegacyProvider("legacy".into()))
                .unwrap(),
            "sk-old"
        );
    }

    #[test]
    fn missing_or_empty_entry_fails_closed() {
        let (keys, backend) = store();
        let error = keys.get_provider("missing").unwrap_err();
        assert!(matches!(
            error,
            SecretError::Missing(SecretRef::Provider(ref name)) if name == "missing"
        ));

        backend.set(KEYCHAIN_SERVICE, "provider:empty", "").unwrap();
        assert!(matches!(
            keys.get_provider("empty").unwrap_err(),
            SecretError::Missing(_)
        ));
    }

    #[test]
    fn resolves_all_runtime_credentials_and_reports_literal_legacy_values() {
        let (keys, _) = store();
        keys.set_provider("openai", "sk-upstream").unwrap();
        keys.set_inference("sk-client").unwrap();
        let mut runtime = config(
            r#"
[server]
host = "127.0.0.1"
port = 1228
api_key = "keychain:any-converter:server:inference"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "keychain:any-converter:provider:openai"

[[providers]]
name = "legacy"
format = "openai_chat"
base_url = "https://legacy.example.test"
api_key = "sk-literal"
"#,
        );

        let report = resolve_runtime_secrets(&mut runtime, &keys).unwrap();
        assert_eq!(runtime.server.api_key.as_deref(), Some("sk-client"));
        assert_eq!(runtime.providers[0].api_key, "sk-upstream");
        assert_eq!(runtime.providers[1].api_key, "sk-literal");
        assert_eq!(report.literal_provider_keys, vec!["legacy".to_string()]);
        assert!(!report.literal_inference_key);
    }

    #[test]
    fn unresolved_reference_aborts_without_leaving_a_reference_as_a_key() {
        let (keys, _) = store();
        let mut runtime = config(
            r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "keychain:any-converter:provider:openai"
"#,
        );

        let error = resolve_runtime_secrets(&mut runtime, &keys).unwrap_err();
        assert!(matches!(error, SecretError::Missing(_)));
        assert_eq!(
            runtime.providers[0].api_key,
            "keychain:any-converter:provider:openai"
        );
    }

    #[test]
    fn management_key_is_never_accepted_as_a_provider_key() {
        let (keys, _) = store();
        let mut runtime = config(
            r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "keychain:any-converter:server:management"
"#,
        );

        let error = resolve_runtime_secrets(&mut runtime, &keys).unwrap_err();
        assert!(matches!(error, SecretError::Reference(_)));
    }
}
