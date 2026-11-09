//! Server-owned control plane for configuration and API-key management.
//!
//! The HTTP admin handler is intentionally thin: this module owns bootstrap,
//! typed mutations, secret compensation, and the in-memory management
//! credential.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use thiserror::Error;
use toml_edit::{Array, ArrayOfTables, Document, Item, Table, Value};

use crate::config::{ModelRouteConfig, RouteStrategy, ServerConfig};
use crate::config_store::{ConfigDocument, ConfigRevision, ConfigStore, ConfigStoreError};
use crate::home::Home;
use crate::secrets::{
    ApiKeyStore, SecretError, SecretRef, SecretResolutionReport, resolve_runtime_secrets,
};

/// A prepared runtime configuration and its control plane.
pub struct PreparedServer {
    pub config: ServerConfig,
    pub control_plane: Arc<ControlPlane>,
    pub secret_report: SecretResolutionReport,
}

/// Server-owned control-plane state.
pub struct ControlPlane {
    home: Home,
    store: ConfigStore,
    keys: ApiKeyStore,
    state: RwLock<ControlPlaneState>,
    rotation_lock: std::sync::Mutex<()>,
}

struct ControlPlaneState {
    management_key: String,
    started_revision: ConfigRevision,
    restart_required: bool,
    ready_address: Option<std::net::SocketAddr>,
    started_at: Option<SystemTime>,
}

impl ControlPlane {
    /// Load, bootstrap, and resolve the configuration for this home.
    ///
    /// Callers that own the process lifecycle should hold the instance lock
    /// before invoking this function.
    pub fn prepare(home: Home, keys: ApiKeyStore) -> Result<PreparedServer, ControlPlaneError> {
        let store = ConfigStore::from_home(&home);
        let (document, management_key) = ensure_management_key(&store, &keys)?;
        let mut config = document.config().clone();
        let secret_report = resolve_runtime_secrets(&mut config, &keys)?;
        let errors = config.validate();
        if !errors.is_empty() {
            return Err(ControlPlaneError::Validation(errors.join("; ")));
        }

        let control_plane = Arc::new(Self {
            home,
            store,
            keys,
            state: RwLock::new(ControlPlaneState {
                management_key,
                started_revision: document.revision().clone(),
                restart_required: false,
                ready_address: None,
                started_at: None,
            }),
            rotation_lock: std::sync::Mutex::new(()),
        });
        Ok(PreparedServer {
            config,
            control_plane,
            secret_report,
        })
    }

    /// Initialize an empty canonical home from validated TOML.
    ///
    /// Literal and legacy provider keys are moved into their canonical
    /// keychain entries; the persisted document contains only references.
    /// This is the one-time import path used by the CLI. Once a home exists,
    /// updates must use `patch_config` under its revision.
    pub fn initialize_from_config(
        home: Home,
        keys: ApiKeyStore,
        source: &str,
    ) -> Result<PreparedServer, ControlPlaneError> {
        let store = ConfigStore::from_home(&home);
        let _guard = store.acquire_write_lock()?;
        if store.load_if_exists()?.is_some() {
            return Err(ControlPlaneError::AlreadyInitialized(
                home.config_path().to_path_buf(),
            ));
        }

        let source_document = ConfigDocument::parse(source)?;
        let errors = source_document.validate();
        if !errors.is_empty() {
            return Err(ControlPlaneError::Validation(errors.join("; ")));
        }

        let config = source_document.config().clone();
        let mut imported = ImportedSecrets::default();
        let import_result = (|| -> Result<(), ControlPlaneError> {
            import_provider_keys(&config, &keys, &mut imported)?;
            import_inference_key(&config, &keys, &mut imported)?;
            import_management_key(&keys, &mut imported)
        })();
        if let Err(error) = import_result {
            restore_imported_secrets(&keys, &imported);
            return Err(error);
        }

        let inference_reference = config
            .server
            .api_key
            .as_deref()
            .filter(|value| !value.is_empty())
            .and_then(|_| imported.inference_reference.clone());
        let management_reference = imported
            .management_reference
            .as_ref()
            .ok_or(ControlPlaneError::ManagementKeyMissing)?
            .canonical();
        let source_document = source_document.clone();
        let write = store.mutate_if_revision_locked(&_guard, None, |document| {
            *document = source_document.clone().into_toml_document();
            set_management_key_ref(document, &management_reference)?;
            for (provider, reference) in &imported.provider_references {
                set_provider_api_key(document, provider, &reference.canonical())?;
            }
            match &inference_reference {
                Some(reference) => set_server_api_key(document, Some(&reference.canonical()))?,
                None => set_server_api_key(document, None)?,
            }
            Ok(())
        });
        if let Err(error) = write {
            restore_imported_secrets(&keys, &imported);
            return Err(error.into());
        }
        drop(_guard);
        Self::prepare(home, keys)
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    pub fn config_store(&self) -> &ConfigStore {
        &self.store
    }

    pub fn api_keys(&self) -> &ApiKeyStore {
        &self.keys
    }

    pub fn management_key(&self) -> String {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .management_key
            .clone()
    }

    pub fn verify_management_key(&self, provided: &str) -> bool {
        let expected = self.management_key();
        provided.as_bytes().ct_eq(expected.as_bytes()).into()
    }

    pub fn mark_ready(&self, address: std::net::SocketAddr) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.ready_address = Some(address);
        state.started_at = Some(SystemTime::now());
    }

    pub fn status(&self) -> Result<ControlPlaneStatus, ControlPlaneError> {
        let document = self.store.load()?;
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(ControlPlaneStatus {
            revision: document.revision().clone(),
            restart_required: state.restart_required
                || document.revision() != &state.started_revision,
            management_key_configured: document.management_key_ref().is_some(),
            ready_address: state.ready_address,
            started_at: state.started_at,
        })
    }

    pub fn config_view(&self) -> Result<AdminConfigView, ControlPlaneError> {
        let document = self.store.load()?;
        Ok(config_view(&document, self.restart_required(&document)))
    }

    /// Apply a typed patch under an `If-Match` precondition.
    pub fn patch_config(
        &self,
        expected: &ConfigRevision,
        patch: ConfigPatch,
    ) -> Result<AdminConfigView, ControlPlaneError> {
        let _guard = self.store.acquire_write_lock()?;
        let current = self.store.load()?;
        if current.revision() != expected {
            return Err(ControlPlaneError::Precondition(current.revision().clone()));
        }

        let key_refs = self.provider_key_refs(&patch)?;
        let document =
            self.store
                .mutate_if_revision_locked(&_guard, Some(expected), |document| {
                    apply_patch(document, patch, key_refs)
                })?;
        self.mark_restart_required();
        Ok(config_view(&document, true))
    }

    /// Store a provider key and point the provider's config entry at its
    /// reference. If the provider does not exist yet, the key is created as
    /// an orphan so the caller can safely create the provider next.
    pub fn put_provider_key(
        &self,
        expected: &ConfigRevision,
        provider: &str,
        api_key: &str,
    ) -> Result<SecretMutationResult, ControlPlaneError> {
        validate_provider_name(provider)?;
        let _guard = self.store.acquire_write_lock()?;
        let current = self.store.load()?;
        if current.revision() != expected {
            return Err(ControlPlaneError::Precondition(current.revision().clone()));
        }
        let previous = self.keys.try_get_provider(provider)?;
        let reference = self.keys.set_provider(provider, api_key)?;
        let provider_exists = current.config().find_provider(provider).is_some();
        if !provider_exists {
            self.mark_restart_required();
            return Ok(SecretMutationResult {
                revision: current.revision().clone(),
                reference,
                restart_required: true,
            });
        }

        let reference_value = reference.canonical();
        let write = self
            .store
            .mutate_if_revision_locked(&_guard, Some(expected), |document| {
                set_provider_api_key(document, provider, &reference_value)
            });
        match write {
            Ok(document) => {
                self.mark_restart_required();
                Ok(SecretMutationResult {
                    revision: document.revision().clone(),
                    reference,
                    restart_required: true,
                })
            }
            Err(error) => {
                restore_provider_key(&self.keys, provider, previous)?;
                Err(error.into())
            }
        }
    }

    pub fn delete_provider_key(
        &self,
        expected: &ConfigRevision,
        provider: &str,
    ) -> Result<SecretMutationResult, ControlPlaneError> {
        validate_provider_name(provider)?;
        let _guard = self.store.acquire_write_lock()?;
        let current = self.store.load()?;
        if current.revision() != expected {
            return Err(ControlPlaneError::Precondition(current.revision().clone()));
        }
        let previous = self.keys.try_get_provider(provider)?;
        self.keys.delete_provider(provider)?;
        let write = self
            .store
            .mutate_if_revision_locked(&_guard, Some(expected), |document| {
                set_provider_api_key(document, provider, "")
            });
        match write {
            Ok(document) => {
                self.mark_restart_required();
                Ok(SecretMutationResult {
                    revision: document.revision().clone(),
                    reference: SecretRef::Provider(provider.to_owned()),
                    restart_required: true,
                })
            }
            Err(error) => {
                restore_provider_key(&self.keys, provider, previous)?;
                Err(error.into())
            }
        }
    }

    pub fn put_inference_key(
        &self,
        expected: &ConfigRevision,
        api_key: &str,
    ) -> Result<SecretMutationResult, ControlPlaneError> {
        let _guard = self.store.acquire_write_lock()?;
        let current = self.store.load()?;
        if current.revision() != expected {
            return Err(ControlPlaneError::Precondition(current.revision().clone()));
        }
        let previous = self.keys.try_get(&SecretRef::Inference)?;
        let reference = self.keys.set_inference(api_key)?;
        let reference_value = reference.canonical();
        let write = self
            .store
            .mutate_if_revision_locked(&_guard, Some(expected), |document| {
                set_server_api_key(document, Some(&reference_value))
            });
        match write {
            Ok(document) => {
                self.mark_restart_required();
                Ok(SecretMutationResult {
                    revision: document.revision().clone(),
                    reference,
                    restart_required: true,
                })
            }
            Err(error) => {
                restore_server_key(&self.keys, SecretRef::Inference, previous)?;
                Err(error.into())
            }
        }
    }

    pub fn delete_inference_key(
        &self,
        expected: &ConfigRevision,
    ) -> Result<SecretMutationResult, ControlPlaneError> {
        let _guard = self.store.acquire_write_lock()?;
        let current = self.store.load()?;
        if current.revision() != expected {
            return Err(ControlPlaneError::Precondition(current.revision().clone()));
        }
        let previous = self.keys.try_get(&SecretRef::Inference)?;
        self.keys.delete_inference()?;
        let write = self
            .store
            .mutate_if_revision_locked(&_guard, Some(expected), |document| {
                set_server_api_key(document, None)
            });
        match write {
            Ok(document) => {
                self.mark_restart_required();
                Ok(SecretMutationResult {
                    revision: document.revision().clone(),
                    reference: SecretRef::Inference,
                    restart_required: true,
                })
            }
            Err(error) => {
                restore_server_key(&self.keys, SecretRef::Inference, previous)?;
                Err(error.into())
            }
        }
    }

    /// Rotate the management key. The caller's current request remains
    /// authenticated; the new key applies to the next request.
    pub fn rotate_management_key(&self) -> Result<SecretMutationResult, ControlPlaneError> {
        let _rotation = self
            .rotation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = self.store.load()?;
        let existing_ref = current
            .management_key_ref()
            .ok_or(ControlPlaneError::ManagementKeyMissing)?;
        match SecretRef::parse(&existing_ref)? {
            Some(SecretRef::Management) => {}
            _ => {
                return Err(ControlPlaneError::ManagementKeyInvalidReference(
                    existing_ref,
                ));
            }
        }
        let (reference, generated) = self.keys.rotate_management()?;
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.management_key = generated;
            state.restart_required = true;
        }
        Ok(SecretMutationResult {
            revision: current.revision().clone(),
            reference,
            restart_required: true,
        })
    }

    fn restart_required(&self, document: &ConfigDocument) -> bool {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.restart_required || document.revision() != &state.started_revision
    }

    fn mark_restart_required(&self) {
        self.state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restart_required = true;
    }

    fn provider_key_refs(
        &self,
        patch: &ConfigPatch,
    ) -> Result<BTreeMap<String, Option<String>>, ControlPlaneError> {
        let mut refs = BTreeMap::new();
        for provider in &patch.upsert_providers {
            validate_provider_name(&provider.name)?;
            let explicit = provider
                .api_key_ref
                .as_deref()
                .map(SecretRef::parse)
                .transpose()?
                .flatten();
            if let Some(reference) = explicit {
                match reference {
                    SecretRef::Provider(ref name) if name == &provider.name => {}
                    _ => {
                        return Err(ControlPlaneError::InvalidPatch(format!(
                            "provider \"{}\" api_key_ref must be keychain:any-converter:provider:{}",
                            provider.name, provider.name
                        )));
                    }
                }
                refs.insert(provider.name.clone(), Some(reference.canonical()));
            } else if self.keys.try_get_provider(&provider.name)?.is_some() {
                refs.insert(
                    provider.name.clone(),
                    Some(SecretRef::Provider(provider.name.clone()).canonical()),
                );
            } else {
                refs.insert(provider.name.clone(), None);
            }
        }
        Ok(refs)
    }
}

/// Authentication authority shared by all management routes.
#[derive(Clone)]
pub enum ManagementAuthority {
    ControlPlane(Arc<ControlPlane>),
    Fixed(Option<String>),
    Disabled,
}

impl ManagementAuthority {
    pub fn from_legacy_key(key: Option<&str>) -> Self {
        match key {
            Some(key) if !key.is_empty() => Self::Fixed(Some(key.to_owned())),
            _ => Self::Disabled,
        }
    }

    pub fn verify(&self, provided: &str) -> bool {
        match self {
            Self::ControlPlane(control_plane) => control_plane.verify_management_key(provided),
            Self::Fixed(Some(expected)) => provided.as_bytes().ct_eq(expected.as_bytes()).into(),
            Self::Fixed(None) | Self::Disabled => false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlPlaneStatus {
    pub revision: ConfigRevision,
    pub restart_required: bool,
    pub management_key_configured: bool,
    pub ready_address: Option<std::net::SocketAddr>,
    #[serde(serialize_with = "serialize_system_time")]
    pub started_at: Option<SystemTime>,
}

fn serialize_system_time<S>(value: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(time) => {
            let secs = time
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            serializer.serialize_some(&secs)
        }
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretMutationResult {
    pub revision: ConfigRevision,
    pub reference: SecretRef,
    pub restart_required: bool,
}

impl Serialize for SecretRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.canonical())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminConfigView {
    pub revision: ConfigRevision,
    pub restart_required: bool,
    pub server: AdminServerView,
    pub providers: Vec<AdminProviderView>,
    pub model_routes: Vec<AdminModelRouteView>,
    pub logging: AdminLoggingView,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminServerView {
    pub host: String,
    pub port: u16,
    pub proxy_configured: bool,
    pub inference_key_configured: bool,
    pub management_key_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminProviderView {
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub api_key_configured: bool,
    pub account: Option<String>,
    pub accounts: Vec<String>,
    pub model_map: BTreeMap<String, String>,
    pub proxy_configured: bool,
    pub plugin: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminModelRouteView {
    pub pattern: String,
    pub providers: Vec<String>,
    pub upstream_model: Option<String>,
    pub strategy: RouteStrategy,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminLoggingView {
    pub dir: Option<String>,
    pub max_disk_mb: u64,
    pub request_log_enabled: bool,
}

fn config_view(document: &ConfigDocument, restart_required: bool) -> AdminConfigView {
    let config = document.config();
    AdminConfigView {
        revision: document.revision().clone(),
        restart_required,
        server: AdminServerView {
            host: config.server.host.clone(),
            port: config.server.port,
            proxy_configured: config.server.proxy.as_deref().is_some_and(non_empty),
            inference_key_configured: config.server.api_key.as_deref().is_some_and(non_empty),
            management_key_configured: document.management_key_ref().is_some(),
        },
        providers: config
            .providers
            .iter()
            .map(|provider| AdminProviderView {
                name: provider.name.clone(),
                format: provider.format.to_string(),
                base_url: provider.base_url.clone(),
                api_key_configured: non_empty(&provider.api_key),
                account: provider.account.clone(),
                accounts: provider.accounts.clone(),
                model_map: provider.model_map.clone().into_iter().collect(),
                proxy_configured: provider.proxy.as_deref().is_some_and(non_empty),
                plugin: provider.plugin.is_some(),
            })
            .collect(),
        model_routes: config
            .model_routes
            .iter()
            .map(|route| AdminModelRouteView {
                pattern: route.pattern.clone(),
                providers: route.provider_names(),
                upstream_model: route.upstream_model.clone(),
                strategy: route.strategy.clone(),
            })
            .collect(),
        logging: AdminLoggingView {
            dir: config.logging.dir.clone(),
            max_disk_mb: config.logging.max_disk_mb,
            request_log_enabled: config.logging.request_log.enabled,
        },
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    #[serde(default)]
    pub server: Option<ServerPatch>,
    #[serde(default)]
    pub logging: Option<LoggingPatch>,
    #[serde(default)]
    pub upsert_providers: Vec<ProviderPatch>,
    #[serde(default)]
    pub remove_providers: Vec<String>,
    #[serde(default)]
    pub upsert_model_routes: Vec<ModelRoutePatch>,
    #[serde(default)]
    pub remove_model_routes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPatch {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub proxy: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingPatch {
    pub dir: Option<Option<String>>,
    pub max_disk_mb: Option<u64>,
    pub request_log_enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPatch {
    pub name: String,
    pub format: Option<Format>,
    pub base_url: Option<String>,
    pub api_key_ref: Option<String>,
    pub model_map: Option<BTreeMap<String, String>>,
    pub account: Option<Option<String>>,
    pub accounts: Option<Vec<String>>,
    pub proxy: Option<Option<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoutePatch {
    pub pattern: String,
    #[serde(default)]
    pub providers: Vec<String>,
    pub upstream_model: Option<String>,
    pub strategy: Option<RouteStrategy>,
}

fn apply_patch(
    document: &mut Document,
    patch: ConfigPatch,
    key_refs: BTreeMap<String, Option<String>>,
) -> Result<(), ConfigStoreError> {
    if let Some(server) = patch.server {
        let table = document
            .get_mut("server")
            .and_then(Item::as_table_mut)
            .ok_or_else(|| ConfigStoreError::Mutation("missing [server] table".to_owned()))?;
        if let Some(host) = server.host {
            table["host"] = toml_edit::value(host);
        }
        if let Some(port) = server.port {
            table["port"] = toml_edit::value(i64::from(port));
        }
        if let Some(proxy) = server.proxy {
            match proxy {
                Some(proxy) => table["proxy"] = toml_edit::value(proxy),
                None => {
                    table.remove("proxy");
                }
            }
        }
    }

    if let Some(logging) = patch.logging {
        if document.get("logging").is_none() {
            document["logging"] = Item::Table(Table::new());
        }
        let table = document
            .get_mut("logging")
            .and_then(Item::as_table_mut)
            .ok_or_else(|| ConfigStoreError::Mutation("invalid [logging] table".to_owned()))?;
        match logging.dir {
            Some(Some(dir)) => table["dir"] = toml_edit::value(dir),
            Some(None) => {
                table.remove("dir");
            }
            None => {}
        }
        if let Some(max_disk_mb) = logging.max_disk_mb {
            table["max_disk_mb"] = toml_edit::value(i64::try_from(max_disk_mb).unwrap_or(i64::MAX));
        }
        if let Some(enabled) = logging.request_log_enabled {
            table["request_log"] = Item::Table({
                let mut request = Table::new();
                request["enabled"] = toml_edit::value(enabled);
                request
            });
        }
    }

    for provider in patch.upsert_providers {
        let provider_name = provider.name.clone();
        upsert_provider(
            document,
            provider,
            key_refs.get(&provider_name).cloned().flatten(),
        )?;
    }
    for provider in patch.remove_providers {
        remove_provider(document, &provider)?;
    }
    for route in patch.upsert_model_routes {
        upsert_route(document, route)?;
    }
    for pattern in patch.remove_model_routes {
        remove_route(document, &pattern)?;
    }
    Ok(())
}

fn upsert_provider(
    document: &mut Document,
    provider: ProviderPatch,
    key_ref: Option<String>,
) -> Result<(), ConfigStoreError> {
    if provider.name.trim().is_empty() {
        return Err(ConfigStoreError::Mutation(
            "provider name cannot be empty".to_owned(),
        ));
    }
    let providers = providers_array(document)?;
    let existing = providers
        .iter_mut()
        .find(|table| table.get("name").and_then(Item::as_str) == Some(&provider.name));
    let table = match existing {
        Some(table) => table,
        None => {
            let mut table = Table::new();
            table["name"] = toml_edit::value(provider.name.clone());
            table["api_key"] = toml_edit::value(key_ref.clone().unwrap_or_default());
            providers.push(table);
            providers
                .iter_mut()
                .last()
                .ok_or_else(|| ConfigStoreError::Mutation("provider insert failed".to_owned()))?
        }
    };
    if let Some(format) = provider.format {
        table["format"] = toml_edit::value(format.to_string());
    }
    if let Some(base_url) = provider.base_url {
        table["base_url"] = toml_edit::value(base_url);
    }
    if let Some(key_ref) = key_ref {
        table["api_key"] = toml_edit::value(key_ref);
    }
    if let Some(model_map) = provider.model_map {
        let mut map = Table::new();
        for (client, upstream) in model_map {
            map[&client] = toml_edit::value(upstream);
        }
        table["model_map"] = Item::Table(map);
    }
    if let Some(account) = provider.account {
        match account {
            Some(account) => table["account"] = toml_edit::value(account),
            None => {
                table.remove("account");
            }
        }
    }
    if let Some(accounts) = provider.accounts {
        let mut values = Array::new();
        for account in accounts {
            values.push(account);
        }
        table["accounts"] = toml_edit::value(Value::Array(values));
    }
    if let Some(proxy) = provider.proxy {
        match proxy {
            Some(proxy) => table["proxy"] = toml_edit::value(proxy),
            None => {
                table.remove("proxy");
            }
        }
    }
    if table.get("format").is_none() {
        return Err(ConfigStoreError::Mutation(format!(
            "new provider \"{}\" requires format",
            provider.name
        )));
    }
    if table.get("base_url").is_none() {
        return Err(ConfigStoreError::Mutation(format!(
            "new provider \"{}\" requires base_url",
            provider.name
        )));
    }
    Ok(())
}

fn providers_array(document: &mut Document) -> Result<&mut ArrayOfTables, ConfigStoreError> {
    if document.get("providers").is_none() {
        document["providers"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    document["providers"]
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            ConfigStoreError::Mutation("`providers` must be an array of tables".to_owned())
        })
}

fn remove_provider(document: &mut Document, provider: &str) -> Result<(), ConfigStoreError> {
    let Some(providers) = document
        .get_mut("providers")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return Ok(());
    };
    let before = providers.len();
    providers.retain(|table| table.get("name").and_then(Item::as_str) != Some(provider));
    if providers.len() == before {
        return Err(ConfigStoreError::Mutation(format!(
            "provider \"{provider}\" does not exist"
        )));
    }
    Ok(())
}

fn upsert_route(document: &mut Document, route: ModelRoutePatch) -> Result<(), ConfigStoreError> {
    if route.pattern.trim().is_empty() {
        return Err(ConfigStoreError::Mutation(
            "model route pattern cannot be empty".to_owned(),
        ));
    }
    if route.providers.is_empty() {
        return Err(ConfigStoreError::Mutation(format!(
            "model route \"{}\" requires at least one provider",
            route.pattern
        )));
    }
    let routes = routes_array(document)?;
    let existing = routes
        .iter_mut()
        .find(|table| table.get("pattern").and_then(Item::as_str) == Some(&route.pattern));
    let table = match existing {
        Some(table) => table,
        None => {
            let mut table = Table::new();
            table["pattern"] = toml_edit::value(route.pattern.clone());
            routes.push(table);
            routes
                .iter_mut()
                .last()
                .ok_or_else(|| ConfigStoreError::Mutation("route insert failed".to_owned()))?
        }
    };
    let mut providers = Array::new();
    for provider in route.providers {
        providers.push(provider);
    }
    table["providers"] = toml_edit::value(Value::Array(providers));
    table.remove("provider");
    match route.upstream_model {
        Some(model) if !model.is_empty() => table["upstream_model"] = toml_edit::value(model),
        _ => {
            table.remove("upstream_model");
        }
    }
    table["strategy"] = toml_edit::value(match route.strategy.unwrap_or_default() {
        RouteStrategy::Priority => "priority",
        RouteStrategy::RoundRobin => "round_robin",
    });
    Ok(())
}

fn routes_array(document: &mut Document) -> Result<&mut ArrayOfTables, ConfigStoreError> {
    if document.get("model_routes").is_none() {
        document["model_routes"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    document["model_routes"]
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            ConfigStoreError::Mutation("`model_routes` must be an array of tables".to_owned())
        })
}

fn remove_route(document: &mut Document, pattern: &str) -> Result<(), ConfigStoreError> {
    let Some(routes) = document
        .get_mut("model_routes")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return Ok(());
    };
    let before = routes.len();
    routes.retain(|table| table.get("pattern").and_then(Item::as_str) != Some(pattern));
    if routes.len() == before {
        return Err(ConfigStoreError::Mutation(format!(
            "model route \"{pattern}\" does not exist"
        )));
    }
    Ok(())
}

fn set_provider_api_key(
    document: &mut Document,
    provider: &str,
    key: &str,
) -> Result<(), ConfigStoreError> {
    let table = document
        .get_mut("providers")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|providers| {
            providers
                .iter_mut()
                .find(|table| table.get("name").and_then(Item::as_str) == Some(provider))
        })
        .ok_or_else(|| ConfigStoreError::Mutation(format!("provider \"{provider}\" not found")))?;
    table["api_key"] = toml_edit::value(key);
    Ok(())
}

fn set_server_api_key(
    document: &mut Document,
    reference: Option<&str>,
) -> Result<(), ConfigStoreError> {
    let server = document
        .get_mut("server")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ConfigStoreError::Mutation("missing [server] table".to_owned()))?;
    match reference {
        Some(reference) => server["api_key"] = toml_edit::value(reference),
        None => {
            server.remove("api_key");
        }
    }
    Ok(())
}

fn ensure_management_key(
    store: &ConfigStore,
    keys: &ApiKeyStore,
) -> Result<(ConfigDocument, String), ControlPlaneError> {
    let existing_key = keys.try_get(&SecretRef::Management)?;
    match store.load_if_exists()? {
        None => {
            let generated = match existing_key {
                Some(existing) => existing,
                None => keys.rotate_management()?.1,
            };
            let reference = SecretRef::Management.canonical();
            let document = store
                .mutate_if_revision(None, |document| initialize_document(document, &reference))?;
            Ok((document, generated))
        }
        Some(document) => match document.management_key_ref() {
            None => {
                let generated = match existing_key {
                    Some(existing) => existing,
                    None => keys.rotate_management()?.1,
                };
                let reference = SecretRef::Management.canonical();
                let document = store.mutate_if_revision(Some(document.revision()), |document| {
                    set_management_key_ref(document, &reference)
                })?;
                Ok((document, generated))
            }
            Some(reference) => match SecretRef::parse(&reference)? {
                Some(SecretRef::Management) => {
                    let key = keys.get_management()?;
                    Ok((document, key))
                }
                _ => Err(ControlPlaneError::ManagementKeyInvalidReference(reference)),
            },
        },
    }
}

#[derive(Default)]
struct ImportedSecrets {
    provider_references: Vec<(String, SecretRef)>,
    previous_providers: Vec<(String, Option<String>)>,
    inference_reference: Option<SecretRef>,
    previous_inference: Option<String>,
    management_reference: Option<SecretRef>,
    previous_management: Option<String>,
}

fn import_provider_keys(
    config: &ServerConfig,
    keys: &ApiKeyStore,
    imported: &mut ImportedSecrets,
) -> Result<(), ControlPlaneError> {
    for provider in &config.providers {
        if provider.plugin.is_some() || provider.api_key.is_empty() {
            continue;
        }
        let previous = keys.try_get_provider(&provider.name)?;
        let value = keys.resolve(&provider.api_key)?;
        let value = value.unwrap_or_default();
        let provider_reference = SecretRef::Provider(provider.name.clone());
        let reference = match previous.clone() {
            Some(existing) if existing == value => provider_reference,
            Some(_) => {
                return Err(ControlPlaneError::Secret(SecretError::Conflict(
                    provider_reference,
                )));
            }
            None => keys.set_provider(&provider.name, &value)?,
        };
        imported
            .previous_providers
            .push((provider.name.clone(), previous));
        imported
            .provider_references
            .push((provider.name.clone(), reference));
    }
    Ok(())
}

fn import_inference_key(
    config: &ServerConfig,
    keys: &ApiKeyStore,
    imported: &mut ImportedSecrets,
) -> Result<(), ControlPlaneError> {
    if let Some(api_key) = config
        .server
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        imported.previous_inference = keys.try_get(&SecretRef::Inference)?;
        let resolved = keys.resolve(api_key)?;
        let resolved = resolved.unwrap_or_default();
        imported.inference_reference = Some(match imported.previous_inference.clone() {
            Some(existing) if existing == resolved => SecretRef::Inference,
            Some(_) => {
                return Err(ControlPlaneError::Secret(SecretError::Conflict(
                    SecretRef::Inference,
                )));
            }
            None => keys.set_inference(&resolved)?,
        });
    }
    Ok(())
}

fn import_management_key(
    keys: &ApiKeyStore,
    imported: &mut ImportedSecrets,
) -> Result<(), ControlPlaneError> {
    imported.previous_management = keys.try_get(&SecretRef::Management)?;
    let reference = match imported.previous_management.clone() {
        Some(_) => SecretRef::Management,
        None => keys.rotate_management()?.0,
    };
    imported.management_reference = Some(reference);
    Ok(())
}

fn restore_imported_secrets(keys: &ApiKeyStore, imported: &ImportedSecrets) {
    for (provider, previous) in imported.previous_providers.iter().rev() {
        if let Err(error) = restore_provider_key(keys, provider, previous.clone()) {
            log::warn!("failed to restore provider key after import: {error}");
        }
    }
    if imported.inference_reference.is_some() {
        if let Err(error) = restore_server_key(
            keys,
            SecretRef::Inference,
            imported.previous_inference.clone(),
        ) {
            log::warn!("failed to restore inference key after import: {error}");
        }
    }
    if imported.management_reference.is_some() {
        let result = match imported.previous_management.as_deref() {
            Some(value) => keys.set_management(value).map(|_| ()),
            None => keys.delete_management(),
        };
        if let Err(error) = result {
            log::warn!("failed to restore management key after import: {error}");
        }
    }
}

fn initialize_document(
    document: &mut Document,
    management_reference: &str,
) -> Result<(), ConfigStoreError> {
    let mut server = Table::new();
    server["host"] = toml_edit::value("127.0.0.1");
    server["port"] = toml_edit::value(1228_i64);
    document["server"] = Item::Table(server);
    set_management_key_ref(document, management_reference)
}

fn set_management_key_ref(
    document: &mut Document,
    reference: &str,
) -> Result<(), ConfigStoreError> {
    if document.get("management").is_none() {
        document["management"] = Item::Table(Table::new());
    }
    let management = document
        .get_mut("management")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ConfigStoreError::Mutation("invalid [management] table".to_owned()))?;
    management["api_key"] = toml_edit::value(reference);
    Ok(())
}

fn restore_provider_key(
    keys: &ApiKeyStore,
    provider: &str,
    previous: Option<String>,
) -> Result<(), ControlPlaneError> {
    match previous {
        Some(value) => {
            keys.set_provider(provider, &value)?;
        }
        None => keys.delete_provider(provider)?,
    }
    Ok(())
}

fn restore_server_key(
    keys: &ApiKeyStore,
    reference: SecretRef,
    previous: Option<String>,
) -> Result<(), ControlPlaneError> {
    match (reference, previous) {
        (SecretRef::Inference, Some(value)) => {
            keys.set_inference(&value)?;
        }
        (SecretRef::Inference, None) => keys.delete_inference()?,
        _ => {}
    }
    Ok(())
}

fn validate_provider_name(provider: &str) -> Result<(), ControlPlaneError> {
    if provider.is_empty() || provider.contains(':') || provider.contains('/') {
        return Err(ControlPlaneError::InvalidProviderName(provider.to_owned()));
    }
    Ok(())
}

fn non_empty(value: &str) -> bool {
    !value.is_empty()
}

#[derive(Debug, Error)]
pub enum ControlPlaneError {
    #[error(transparent)]
    Config(#[from] ConfigStoreError),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error(transparent)]
    SecretReference(#[from] crate::secrets::SecretRefError),
    #[error("invalid configuration patch: {0}")]
    InvalidPatch(String),
    #[error("invalid provider name: `{0}`")]
    InvalidProviderName(String),
    #[error("config validation failed: {0}")]
    Validation(String),
    #[error(
        "management key is not configured; restart through the managed launcher to bootstrap it"
    )]
    ManagementKeyMissing,
    #[error("management key must be a keychain reference, found `{0}`")]
    ManagementKeyInvalidReference(String),
    #[error("config revision precondition failed; current revision is {0}")]
    Precondition(ConfigRevision),
    #[error("canonical config already exists at `{0}`; use the management API to update it")]
    AlreadyInitialized(std::path::PathBuf),
}

/// Build the route inventory's model-route view shape without exposing raw
/// TOML. Kept here so route/format conversions cannot drift between handlers.
pub fn admin_route_from_config(route: &ModelRouteConfig) -> AdminModelRouteView {
    AdminModelRouteView {
        pattern: route.pattern.clone(),
        providers: route.provider_names(),
        upstream_model: route.upstream_model.clone(),
        strategy: route.strategy.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::home::Home;
    use crate::secrets::{KEYCHAIN_SERVICE, KeychainBackend, MemoryKeychain};

    struct FailSecondProviderKeychain {
        backend: Arc<MemoryKeychain>,
    }

    impl KeychainBackend for FailSecondProviderKeychain {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretError> {
            self.backend.get(service, account)
        }

        fn set(&self, service: &str, account: &str, value: &str) -> Result<(), SecretError> {
            if service == KEYCHAIN_SERVICE && account == "provider:second" {
                return Err(SecretError::InvalidProviderName(account.to_owned()));
            }
            self.backend.set(service, account, value)
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), SecretError> {
            self.backend.delete(service, account)
        }
    }

    fn temp_home(tag: &str) -> (Home, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "any-converter-control-plane-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (Home::from_root(root.clone()), root)
    }

    fn keys() -> (ApiKeyStore, Arc<MemoryKeychain>) {
        let backend = Arc::new(MemoryKeychain::new());
        (
            ApiKeyStore::with_backend(KEYCHAIN_SERVICE, backend.clone()),
            backend,
        )
    }

    #[test]
    fn first_start_bootstraps_keychain_management_key_and_config() {
        let (home, root) = temp_home("bootstrap");
        let (keys, _) = keys();

        let prepared = ControlPlane::prepare(home.clone(), keys).unwrap();

        assert!(root.join("config.toml").exists());
        let source = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(source.contains("[management]"));
        assert!(source.contains("keychain:any-converter:server:management"));
        assert!(!source.contains("acmgmt_"));
        assert!(
            prepared
                .control_plane
                .management_key()
                .starts_with("acmgmt_")
        );
    }

    #[test]
    fn interrupted_bootstrap_reuses_an_existing_management_key() {
        let (home, _) = temp_home("bootstrap-reuse");
        let (keys, _) = keys();
        keys.set_management("acmgmt_existing").unwrap();

        let prepared = ControlPlane::prepare(home, keys).unwrap();

        assert_eq!(prepared.control_plane.management_key(), "acmgmt_existing");
    }

    #[test]
    fn missing_existing_management_secret_fails_closed() {
        let (home, root) = temp_home("missing-management");
        let (keys, _) = keys();
        std::fs::write(
            root.join("config.toml"),
            r#"
[server]
host = "127.0.0.1"
port = 1228

[management]
api_key = "keychain:any-converter:server:management"
"#,
        )
        .unwrap();

        let error = ControlPlane::prepare(home, keys).err().unwrap();
        assert!(matches!(
            error,
            ControlPlaneError::Secret(SecretError::Missing(_))
        ));
    }

    #[test]
    fn one_time_config_import_moves_secrets_to_keychain() {
        let (home, root) = temp_home("import");
        let (keys, backend) = keys();
        let source = r#"
[server]
host = "127.0.0.1"
port = 1228
api_key = "client-key"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-upstream"

[[model_routes]]
pattern = "*"
providers = ["openai"]
"#;

        let prepared =
            ControlPlane::initialize_from_config(home.clone(), keys.clone(), source).unwrap();
        let persisted = std::fs::read_to_string(root.join("config.toml")).unwrap();

        assert!(!persisted.contains("sk-upstream"));
        assert!(!persisted.contains("client-key"));
        assert!(persisted.contains("keychain:any-converter:provider:openai"));
        assert!(persisted.contains("keychain:any-converter:server:inference"));
        assert!(
            prepared
                .control_plane
                .management_key()
                .starts_with("acmgmt_")
        );
        assert_eq!(
            backend
                .get(KEYCHAIN_SERVICE, "provider:openai")
                .unwrap()
                .as_deref(),
            Some("sk-upstream")
        );

        let error = ControlPlane::initialize_from_config(home, keys.clone(), source)
            .err()
            .unwrap();
        assert!(matches!(error, ControlPlaneError::AlreadyInitialized(_)));
    }

    #[test]
    fn failed_one_time_import_restores_keychain_and_leaves_no_config() {
        let (home, root) = temp_home("import-compensation");
        let backend = Arc::new(MemoryKeychain::new());
        let keys = ApiKeyStore::with_backend(
            KEYCHAIN_SERVICE,
            Arc::new(FailSecondProviderKeychain {
                backend: Arc::clone(&backend),
            }),
        );
        let source = r#"
[server]
host = "127.0.0.1"
port = 1228
api_key = "client-key"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-new-provider"

[[providers]]
name = "second"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-second"

[[model_routes]]
pattern = "*"
providers = ["openai"]
"#;

        let error = ControlPlane::initialize_from_config(home, keys.clone(), source)
            .err()
            .expect("management-key write must fail");
        assert!(matches!(
            error,
            ControlPlaneError::Secret(SecretError::InvalidProviderName(_))
        ));
        assert!(!root.join("config.toml").exists());
        assert!(keys.try_get_provider("openai").unwrap().is_none());
        assert!(keys.try_get(&SecretRef::Inference).unwrap().is_none());
        assert!(keys.try_get(&SecretRef::Management).unwrap().is_none());
    }

    #[test]
    fn one_time_import_rejects_conflicting_existing_secret_without_overwriting_it() {
        let (home, _) = temp_home("import-conflict");
        let (keys, _) = keys();
        keys.set_provider("openai", "sk-existing").unwrap();
        let source = r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-imported"
"#;

        let error = match ControlPlane::initialize_from_config(home, keys.clone(), source) {
            Err(error) => error,
            Ok(_) => panic!("conflicting secret import must fail"),
        };
        assert!(matches!(
            error,
            ControlPlaneError::Secret(SecretError::Conflict(_))
        ));
        assert_eq!(keys.get_provider("openai").unwrap(), "sk-existing");
    }

    #[test]
    fn one_time_import_is_retryable_after_config_commit_is_interrupted() {
        let (home, root) = temp_home("import-retry");
        let (keys, _) = keys();
        let source = r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-imported"
"#;
        let first = ControlPlane::initialize_from_config(home.clone(), keys.clone(), source)
            .unwrap()
            .control_plane
            .management_key();
        std::fs::remove_file(home.config_path()).unwrap();

        let second = ControlPlane::initialize_from_config(home, keys.clone(), source)
            .unwrap()
            .control_plane
            .management_key();

        assert_eq!(first, second);
        assert!(root.join("config.toml").exists());
    }

    #[test]
    fn concurrent_one_time_import_initializes_exactly_once() {
        let (home, _) = temp_home("import-concurrent");
        let backend: Arc<dyn KeychainBackend> = Arc::new(MemoryKeychain::new());
        let source = r#"
[server]
host = "127.0.0.1"
port = 1228

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.example.test"
api_key = "sk-upstream"
"#;
        let mut handles = Vec::new();
        for _ in 0..2 {
            let home = home.clone();
            let keys = ApiKeyStore::with_backend(KEYCHAIN_SERVICE, Arc::clone(&backend));
            let source = source.to_owned();
            handles.push(std::thread::spawn(move || {
                ControlPlane::initialize_from_config(home, keys, &source)
            }));
        }

        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ControlPlaneError::AlreadyInitialized(_))))
                .count(),
            1
        );
    }

    #[test]
    fn typed_patch_preserves_comments_and_is_cas_protected() {
        let (home, root) = temp_home("patch");
        let (keys, _) = keys();
        let prepared = ControlPlane::prepare(home, keys.clone()).unwrap();
        let initial_revision = prepared.control_plane.config_view().unwrap().revision;
        keys.set_provider("openai", "sk-upstream").unwrap();

        let view = prepared
            .control_plane
            .patch_config(
                &initial_revision,
                ConfigPatch {
                    server: Some(ServerPatch {
                        port: Some(1229),
                        ..Default::default()
                    }),
                    logging: None,
                    upsert_providers: vec![ProviderPatch {
                        name: "openai".into(),
                        format: Some(Format::OpenAIChat),
                        base_url: Some("https://api.example.test".into()),
                        api_key_ref: None,
                        model_map: Some(BTreeMap::from([("*".into(), "gpt-4.1".into())])),
                        account: None,
                        accounts: None,
                        proxy: None,
                    }],
                    remove_providers: vec![],
                    upsert_model_routes: vec![ModelRoutePatch {
                        pattern: "*".into(),
                        providers: vec!["openai".into()],
                        upstream_model: None,
                        strategy: None,
                    }],
                    remove_model_routes: vec![],
                },
            )
            .unwrap();
        assert_eq!(view.server.port, 1229);
        assert!(view.restart_required);
        assert!(view.providers[0].api_key_configured);

        let stale = prepared
            .control_plane
            .patch_config(&initial_revision, ConfigPatch::default());
        assert!(matches!(stale, Err(ControlPlaneError::Precondition(_))));
        let source = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(source.contains("api_key = \"keychain:any-converter:provider:openai\""));
        assert!(!source.contains("sk-upstream"));
    }

    #[test]
    fn provider_secret_write_is_compensated_when_config_write_fails() {
        let (home, root) = temp_home("compensation");
        let (keys, backend) = keys();
        let prepared = ControlPlane::prepare(home, keys.clone()).unwrap();
        let initial = prepared.control_plane.config_view().unwrap().revision;
        let created = prepared
            .control_plane
            .patch_config(
                &initial,
                ConfigPatch {
                    server: None,
                    logging: None,
                    upsert_providers: vec![ProviderPatch {
                        name: "openai".into(),
                        format: Some(Format::OpenAIChat),
                        base_url: Some("https://api.example.test".into()),
                        api_key_ref: None,
                        model_map: None,
                        account: None,
                        accounts: None,
                        proxy: None,
                    }],
                    remove_providers: vec![],
                    upsert_model_routes: vec![],
                    remove_model_routes: vec![],
                },
            )
            .unwrap();
        keys.set_provider("openai", "sk-old").unwrap();
        let path = root.join("config.toml");
        let before = std::fs::read_to_string(&path).unwrap();

        // Force the atomic write to fail after the keychain write: the backup
        // path is a directory, so `fs::copy` cannot replace it.
        let backup = root.join("config.toml.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::create_dir(&backup).unwrap();

        let error = prepared
            .control_plane
            .put_provider_key(&created.revision, "openai", "sk-new")
            .unwrap_err();
        assert!(matches!(error, ControlPlaneError::Config(_)));
        assert_eq!(
            backend
                .get(KEYCHAIN_SERVICE, "provider:openai")
                .unwrap()
                .as_deref(),
            Some("sk-old")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
        let _ = std::fs::remove_dir_all(backup);
    }
}
