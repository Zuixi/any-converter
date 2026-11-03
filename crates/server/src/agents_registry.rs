//! Agent wiring registry internals.
//!
//! The registry is the only writer of agent configuration files. Every agent
//! mutation takes a single async mutex, persists display state through
//! [`agent_wiring::atomic`], and keeps plaintext gateway keys in memory only
//! (the durable copy lives in [`FileSecretStore`]).
//!
//! The request hot path never waits for a catalog sync: it compares the stored
//! digest in memory and spawns a best-effort background sync when it is stale.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_wiring::{
    AgentSpec, CatalogModel, ConnectReport, DetectResult, DisconnectReport, HomeCtx, ModelCatalog,
    WiredState, Wiring, WiringCtx, all_specs,
};
use serde::{Deserialize, Serialize};

pub use crate::agents::ApiError;
use crate::auth::{self, AuthError};
use crate::config::ServerConfig;
use crate::credentials::{Credential, FileSecretStore, SecretStore};

const STATE_FILE: &str = "agent_state.json";
pub(crate) const STATUS_CONNECTED: &str = "connected";
pub(crate) const STATUS_DISCONNECTED: &str = "disconnected";
const STATUS_DRIFTED: &str = "drifted";
const STATUS_ABSENT: &str = "absent";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentStateFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    agents: BTreeMap<String, AgentEntry>,
}

fn default_version() -> u32 {
    1
}

impl Default for AgentStateFile {
    fn default() -> Self {
        Self {
            version: 1,
            agents: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentEntry {
    #[serde(default)]
    status: String,
    #[serde(default)]
    agent_key_id: Option<String>,
    #[serde(default)]
    connected_at: Option<i64>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    catalog_digest: Option<String>,
    #[serde(default)]
    home: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentKeyEntry {
    key: String,
    id: String,
    created_at: i64,
    last_used: Option<i64>,
}

struct RegistryInner {
    state: AgentStateFile,
    agent_keys: HashMap<String, AgentKeyEntry>,
}

/// One agent row returned by `GET /v1/agents`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    /// Stable agent identifier.
    pub id: String,
    /// `connected`, `disconnected`, `drifted`, or `absent`.
    pub status: String,
    /// Last connected default model, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Digest of the catalog last written to this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_digest: Option<String>,
    /// Key id only; the plaintext key never leaves the registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// Unix timestamp of the last successful connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_at: Option<i64>,
}

/// Key metadata returned by `GET /v1/agents/{id}/keys` (never plaintext).
#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    /// Stable key identifier (`ak_<agent>_<n>`).
    pub id: String,
    /// Unix timestamp when the key was created.
    pub created_at: i64,
    /// Unix timestamp of the last inference request using this key.
    pub last_used: Option<i64>,
}

/// Serializes every agent write behind one async mutex.
pub struct AgentRegistry {
    data_dir: PathBuf,
    secrets: Arc<FileSecretStore>,
    gateway_url: String,
    global_key: Option<String>,
    catalog: RwLock<ModelCatalog>,
    home_override: RwLock<Option<PathBuf>>,
    inner: tokio::sync::Mutex<RegistryInner>,
}

impl AgentRegistry {
    /// Creates a registry backed by `data_dir` and `secret_store`.
    ///
    /// A missing state file starts empty; a corrupt one is moved aside as
    /// `agent_state.json.bad-<ts>` and replaced with an empty state.
    pub fn new(
        data_dir: PathBuf,
        secret_store: Arc<FileSecretStore>,
        gateway_url: String,
        global_key: Option<String>,
    ) -> Self {
        let state = load_state(&data_dir);
        let agent_keys = load_keys(&secret_store, &state);
        Self {
            data_dir,
            secrets: secret_store,
            gateway_url,
            global_key,
            catalog: RwLock::new(ModelCatalog::default()),
            home_override: RwLock::new(None),
            inner: tokio::sync::Mutex::new(RegistryInner { state, agent_keys }),
        }
    }

    /// A throwaway registry for tests: isolated data dir *and* isolated home,
    /// so route tests can never touch a developer's real agent configuration.
    pub fn in_memory() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "any-converter-agent-registry-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(FileSecretStore::new(dir.join("secrets.json")));
        let registry = Self::new(
            dir.clone(),
            store,
            "http://127.0.0.1:8080".to_owned(),
            None,
        );
        registry.set_home(dir);
        registry
    }

    /// Replaces the catalog snapshot used by connect/sync/digest checks.
    pub fn set_catalog(&self, catalog: ModelCatalog) {
        let mut guard = self.catalog.write().unwrap_or_else(|lock| lock.into_inner());
        *guard = catalog;
    }

    /// Overrides the home directory resolved for agent specs (tests).
    pub fn set_home(&self, home: impl Into<PathBuf>) {
        let mut guard = self
            .home_override
            .write()
            .unwrap_or_else(|lock| lock.into_inner());
        *guard = Some(home.into());
    }

    /// The home directory agent specs resolve their configuration under.
    pub fn home(&self) -> PathBuf {
        let override_home = self
            .home_override
            .read()
            .unwrap_or_else(|lock| lock.into_inner())
            .clone();
        override_home.unwrap_or_else(|| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
        })
    }

    /// Current catalog digest, used by the hot-path staleness check.
    pub fn digest_now(&self) -> String {
        self.catalog_snapshot().digest()
    }

    fn catalog_snapshot(&self) -> ModelCatalog {
        self.catalog
            .read()
            .unwrap_or_else(|lock| lock.into_inner())
            .clone()
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.join(STATE_FILE)
    }

    fn save_state(&self, state: &AgentStateFile) -> Result<(), ApiError> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        agent_wiring::atomic::write_atomic(&self.state_path(), &bytes)
            .map_err(|error| ApiError::ConfigUnwritable(error.to_string()))
    }

    /// Detects every supported agent and reports its current status.
    ///
    /// Lazy drift checks run here (UI polling cadence), never on the request
    /// hot path: a stored `connected` agent whose files no longer point at the
    /// gateway is persisted as `drifted`.
    pub async fn list(&self, home: &Path) -> Vec<AgentStatus> {
        let ctx = home_ctx(home);
        let mut rows = Vec::new();
        let mut drifted = Vec::new();
        {
            let inner = self.inner.lock().await;
            for spec in all_specs() {
                let id = spec.id().to_owned();
                if matches!(spec.detect(&ctx), DetectResult::Absent) {
                    rows.push(AgentStatus {
                        id,
                        status: STATUS_ABSENT.to_owned(),
                        model: None,
                        catalog_digest: None,
                        key_id: None,
                        connected_at: None,
                    });
                    continue;
                }
                let entry = inner.state.agents.get(&id);
                let mut status = entry
                    .map(|entry| {
                        if entry.status.is_empty() {
                            STATUS_DISCONNECTED.to_owned()
                        } else {
                            entry.status.clone()
                        }
                    })
                    .unwrap_or_else(|| STATUS_DISCONNECTED.to_owned());
                if status == STATUS_CONNECTED && self.agent_drifted(spec.as_ref(), &ctx) {
                    status = STATUS_DRIFTED.to_owned();
                    drifted.push(id.clone());
                }
                rows.push(AgentStatus {
                    id,
                    status,
                    model: entry.and_then(|entry| entry.default_model.clone()),
                    catalog_digest: entry.and_then(|entry| entry.catalog_digest.clone()),
                    key_id: entry.and_then(|entry| entry.agent_key_id.clone()),
                    connected_at: entry.and_then(|entry| entry.connected_at),
                });
            }
        }
        if !drifted.is_empty() {
            let mut inner = self.inner.lock().await;
            for id in drifted {
                if let Some(entry) = inner.state.agents.get_mut(&id) {
                    entry.status = STATUS_DRIFTED.to_owned();
                }
            }
            if let Err(error) = self.save_state(&inner.state) {
                log::warn!("could not persist agent drift state: {error}");
            }
        }
        rows
    }

    /// Reports whether a connected agent's files no longer hold the values we
    /// applied — precise drift detection by reconciling against the applied
    /// audit (`agent_applied.json`, written by `agent_wiring::Wiring` on every
    /// connect), with a gateway-authority check as a fallback when no audit
    /// exists for the agent.
    ///
    /// The audit answers a fact ("do our keys still hold the values we
    /// wrote?"); the fallback asks only "does the base URL still point at
    /// THIS gateway" — never "does it look like a gateway", which cannot
    /// tell a third-party proxy from our wiring. The audit lives in this
    /// registry's `data_dir`, the same directory `Wiring::new` was given at
    /// connect time.
    fn agent_drifted(&self, spec: &dyn AgentSpec, ctx: &HomeCtx) -> bool {
        let applied =
            agent_wiring::backup::applied::Applied::load(&self.data_dir.join("agent_applied.json"));
        if !applied.recorded(spec.id()).is_empty() {
            let current = spec.applied_values(ctx);
            return !applied.drifted_keys(spec.id(), &current).is_empty();
        }
        spec.base_url_key()
            .and_then(|key| spec.read_key(ctx, &key))
            .is_some_and(|value| !agent_wiring::gw_base_match(&value, &self.gateway_url))
    }

    /// Wires `agent_id` onto the gateway, reusing its key when one exists.
    pub async fn connect(
        &self,
        agent_id: &str,
        model: Option<String>,
        home: &Path,
    ) -> Result<ConnectReport, ApiError> {
        let catalog = self.catalog_snapshot();
        if catalog.models.is_empty() {
            return Err(ApiError::NoProviders);
        }
        let home_ctx = home_ctx(home);
        let mut inner = self.inner.lock().await;
        let spec = spec_for(agent_id)?;

        let existing = inner.state.agents.get(agent_id).cloned().unwrap_or_default();
        let default_model = resolve_model(&catalog, model, existing.default_model.as_deref())?;

        let (key, key_id, created_at, last_used) = match inner.agent_keys.get(agent_id) {
            Some(entry) => (
                entry.key.clone(),
                entry.id.clone(),
                entry.created_at,
                entry.last_used,
            ),
            None => {
                let key_id = next_key_id(agent_id, existing.agent_key_id.as_deref());
                let key = generate_key(agent_id);
                self.put_key(agent_id, &key)?;
                (key, key_id, now(), None)
            }
        };

        let wiring_ctx = WiringCtx {
            gateway_url: &self.gateway_url,
            agent_key: &key,
            catalog: &catalog,
            default_model: &default_model,
        };
        let wiring = Wiring::new(&self.data_dir);
        let report = wiring.connect(spec.as_ref(), &home_ctx, &wiring_ctx)?;

        inner.agent_keys.insert(
            agent_id.to_owned(),
            AgentKeyEntry {
                key,
                id: key_id.clone(),
                created_at,
                last_used,
            },
        );
        inner.state.agents.insert(
            agent_id.to_owned(),
            AgentEntry {
                status: STATUS_CONNECTED.to_owned(),
                agent_key_id: Some(key_id),
                connected_at: Some(now()),
                default_model: Some(default_model),
                catalog_digest: Some(catalog.digest()),
                home: Some(home.display().to_string()),
            },
        );
        self.save_state(&inner.state)?;
        Ok(report)
    }

    /// Restores `agent_id` to its pre-connect configuration.
    pub async fn disconnect(
        &self,
        agent_id: &str,
        home: &Path,
    ) -> Result<DisconnectReport, ApiError> {
        let home_ctx = home_ctx(home);
        let mut inner = self.inner.lock().await;
        let spec = spec_for(agent_id)?;
        let wiring = Wiring::new(&self.data_dir);
        let report = wiring.disconnect(spec.as_ref(), &home_ctx)?;
        let entry = inner
            .state
            .agents
            .entry(agent_id.to_owned())
            .or_default();
        entry.status = STATUS_DISCONNECTED.to_owned();
        self.save_state(&inner.state)?;
        Ok(report)
    }

    /// Reapplies the current catalog after a provider/model change.
    pub async fn sync(&self, agent_id: &str, home: &Path) -> Result<(), ApiError> {
        let catalog = self.catalog_snapshot();
        if catalog.models.is_empty() {
            return Err(ApiError::NoProviders);
        }
        let home_ctx = home_ctx(home);
        let mut inner = self.inner.lock().await;
        let spec = spec_for(agent_id)?;
        let entry = inner
            .state
            .agents
            .get(agent_id)
            .cloned()
            .ok_or_else(|| ApiError::NotConnected(agent_id.to_owned()))?;
        if entry.status != STATUS_CONNECTED && entry.status != STATUS_DRIFTED {
            return Err(ApiError::NotConnected(agent_id.to_owned()));
        }
        let key = inner
            .agent_keys
            .get(agent_id)
            .cloned()
            .ok_or_else(|| ApiError::NotConnected(agent_id.to_owned()))?;
        let default_model = resolve_model(&catalog, None, entry.default_model.as_deref())?;
        let wiring_ctx = WiringCtx {
            gateway_url: &self.gateway_url,
            agent_key: &key.key,
            catalog: &catalog,
            default_model: &default_model,
        };
        let wiring = Wiring::new(&self.data_dir);
        wiring.connect(spec.as_ref(), &home_ctx, &wiring_ctx)?;

        inner.state.agents.insert(
            agent_id.to_owned(),
            AgentEntry {
                status: STATUS_CONNECTED.to_owned(),
                agent_key_id: Some(key.id),
                connected_at: entry.connected_at,
                default_model: Some(default_model),
                catalog_digest: Some(catalog.digest()),
                home: Some(home.display().to_string()),
            },
        );
        self.save_state(&inner.state)?;
        Ok(())
    }

    /// Issues a fresh gateway key and rewrites the agent configuration.
    pub async fn rotate_key(&self, agent_id: &str, home: &Path) -> Result<String, ApiError> {
        let catalog = self.catalog_snapshot();
        if catalog.models.is_empty() {
            return Err(ApiError::NoProviders);
        }
        let home_ctx = home_ctx(home);
        let mut inner = self.inner.lock().await;
        let spec = spec_for(agent_id)?;
        let entry = inner
            .state
            .agents
            .get(agent_id)
            .cloned()
            .ok_or_else(|| ApiError::NotConnected(agent_id.to_owned()))?;
        let old = inner
            .agent_keys
            .get(agent_id)
            .cloned()
            .ok_or_else(|| ApiError::NotConnected(agent_id.to_owned()))?;
        let default_model = resolve_model(&catalog, None, entry.default_model.as_deref())?;

        let key_id = next_key_id(agent_id, Some(&old.id));
        let key = generate_key(agent_id);
        self.put_key(agent_id, &key)?;

        let wiring_ctx = WiringCtx {
            gateway_url: &self.gateway_url,
            agent_key: &key,
            catalog: &catalog,
            default_model: &default_model,
        };
        let wiring = Wiring::new(&self.data_dir);
        wiring.connect(spec.as_ref(), &home_ctx, &wiring_ctx)?;

        inner.agent_keys.insert(
            agent_id.to_owned(),
            AgentKeyEntry {
                key,
                id: key_id.clone(),
                created_at: now(),
                last_used: None,
            },
        );
        inner.state.agents.insert(
            agent_id.to_owned(),
            AgentEntry {
                status: STATUS_CONNECTED.to_owned(),
                agent_key_id: Some(key_id.clone()),
                connected_at: entry.connected_at,
                default_model: Some(default_model),
                catalog_digest: Some(catalog.digest()),
                home: Some(home.display().to_string()),
            },
        );
        self.save_state(&inner.state)?;
        Ok(key_id)
    }

    /// Metadata for an agent's key, never the plaintext value.
    pub async fn key_info(&self, agent_id: &str) -> Option<KeyInfo> {
        let inner = self.inner.lock().await;
        inner.agent_keys.get(agent_id).map(|entry| KeyInfo {
            id: entry.id.clone(),
            created_at: entry.created_at,
            last_used: entry.last_used,
        })
    }

    /// Records that an agent key authenticated a request.
    ///
    /// Returns the agent id so the caller can trigger a stale-sync check.
    pub async fn note_request_key(&self, key_plaintext: &str) -> Option<String> {
        let mut inner = self.inner.lock().await;
        let mut matched = None;
        for (agent_id, entry) in inner.agent_keys.iter_mut() {
            if entry.key == key_plaintext {
                entry.last_used = Some(now());
                matched = Some(agent_id.clone());
                break;
            }
        }
        matched
    }

    /// Validates an inference request against the global and agent keys, then
    /// records key usage and schedules a background sync when the catalog moved.
    pub async fn authenticate(
        self: &Arc<Self>,
        global_key: Option<&str>,
        header: Option<&str>,
    ) -> Result<(), AuthError> {
        self.validate_key(global_key, header).await?;
        let Some(provided) = bearer(header) else {
            return Ok(());
        };
        if let Some(agent_id) = self.note_request_key(provided).await {
            self.request_sync_if_stale(&agent_id).await;
        }
        Ok(())
    }

    /// Validates a read-only agent request (global or agent key).
    pub async fn validate_key(
        &self,
        global_key: Option<&str>,
        header: Option<&str>,
    ) -> Result<(), AuthError> {
        let keys: Vec<String> = {
            let inner = self.inner.lock().await;
            inner
                .agent_keys
                .values()
                .map(|entry| entry.key.clone())
                .collect()
        };
        let global = global_key.or(self.global_key.as_deref());
        auth::validate_client_keys(global, keys.iter().map(String::as_str), header)
    }

    /// Spawns a best-effort sync when the stored digest differs from the live
    /// catalog. The caller's request never awaits the sync itself.
    pub async fn request_sync_if_stale(self: &Arc<Self>, agent_id: &str) {
        let current = self.digest_now();
        let home = {
            let inner = self.inner.lock().await;
            let Some(entry) = inner.state.agents.get(agent_id) else {
                return;
            };
            if entry.status != STATUS_CONNECTED
                || entry.catalog_digest.as_deref() == Some(current.as_str())
            {
                return;
            }
            entry
                .home
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home())
        };
        let registry = Arc::clone(self);
        let agent = agent_id.to_owned();
        tokio::spawn(async move {
            if let Err(error) = registry.sync(&agent, &home).await {
                log::warn!("background agent sync failed agent={agent} error={error}");
            }
        });
    }

    fn put_key(&self, agent_id: &str, key: &str) -> Result<(), ApiError> {
        self.secrets
            .put(
                &key_store_id(agent_id),
                &Credential::ApiKey {
                    key: key.to_owned(),
                },
            )
            .map_err(|error| ApiError::ConfigUnwritable(error.to_string()))
    }
}

/// Builds the normalized model catalog from configured providers.
///
/// Each model is exposed as `<provider>/<model>` so routing remains
/// unambiguous when several providers share a model name. Both explicit
/// `model_map` entries and `[[model_routes]]` patterns contribute: the
/// README's recommended setup routes via `model_routes`, so a catalog that
/// ignored them would be empty for most real configs. Wildcard patterns
/// (containing `*` or `?`) are skipped — a pattern is not a model an agent
/// can pick.
pub fn catalog_from_config(config: &ServerConfig) -> ModelCatalog {
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    for provider in &config.providers {
        let mut names: Vec<&String> = provider.model_map.keys().collect();
        names.sort();
        for name in names {
            if name.contains('*') || name.contains('?') {
                continue;
            }
            let id = format!("{}/{}", provider.name, name);
            if seen.insert(id.clone()) {
                models.push(CatalogModel::new(id, name.clone(), false, None));
            }
        }
    }
    for route in &config.model_routes {
        if route.pattern.contains('*') || route.pattern.contains('?') {
            continue;
        }
        let provider_name = route.provider.as_deref().or_else(|| route.providers.first().map(String::as_str));
        let Some(provider_name) = provider_name else {
            continue;
        };
        let id = format!("{}/{}", provider_name, route.pattern);
        if seen.insert(id.clone()) {
            models.push(CatalogModel::new(id, route.pattern.clone(), false, None));
        }
    }
    ModelCatalog::new(models)
}

pub(crate) fn spec_for(agent_id: &str) -> Result<Box<dyn AgentSpec>, ApiError> {
    all_specs()
        .into_iter()
        .find(|spec| spec.id() == agent_id)
        .ok_or_else(|| ApiError::UnknownAgent(agent_id.to_owned()))
}

fn resolve_model(
    catalog: &ModelCatalog,
    requested: Option<String>,
    stored: Option<&str>,
) -> Result<String, ApiError> {
    if let Some(model) = requested {
        if catalog.find(&model).is_some() {
            return Ok(model);
        }
        return Err(ApiError::ModelNotFound(model));
    }
    if let Some(model) = stored {
        if catalog.find(model).is_some() {
            return Ok(model.to_owned());
        }
    }
    catalog
        .first_model()
        .map(|model| model.id.clone())
        .ok_or(ApiError::NoProviders)
}

fn next_key_id(agent_id: &str, current: Option<&str>) -> String {
    let sequence = current
        .and_then(|id| id.rsplit('_').next())
        .and_then(|suffix| suffix.parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    format!("ak_{agent_id}_{sequence:02}")
}

fn generate_key(agent_id: &str) -> String {
    let first = uuid::Uuid::new_v4().simple().to_string();
    let second = uuid::Uuid::new_v4().simple().to_string();
    let hex: String = first
        .chars()
        .take(16)
        .chain(second.chars().take(16))
        .collect();
    format!("acw_{agent_id}_{hex}")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn bearer(header: Option<&str>) -> Option<&str> {
    header.and_then(|value| value.strip_prefix("Bearer "))
}

fn home_ctx(home: &Path) -> HomeCtx {
    HomeCtx {
        home: home.to_path_buf(),
        env: env_lookup,
    }
}

fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn key_store_id(agent_id: &str) -> String {
    format!("agent-key:{agent_id}")
}

fn load_state(data_dir: &Path) -> AgentStateFile {
    let path = data_dir.join(STATE_FILE);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                let bad = path.with_extension(format!("json.bad-{}", now()));
                log::warn!(
                    "agent state at {} is corrupt ({error}); moving it to {}",
                    path.display(),
                    bad.display()
                );
                let _ = fs::rename(&path, &bad);
                AgentStateFile::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AgentStateFile::default(),
        Err(error) => {
            log::warn!(
                "could not read agent state at {}: {error}; starting empty",
                path.display()
            );
            AgentStateFile::default()
        }
    }
}

fn load_keys(
    secrets: &FileSecretStore,
    state: &AgentStateFile,
) -> HashMap<String, AgentKeyEntry> {
    let mut keys = HashMap::new();
    for (agent_id, entry) in &state.agents {
        let Some(key_id) = entry.agent_key_id.as_deref() else {
            continue;
        };
        match secrets.get(&key_store_id(agent_id)) {
            Ok(Some(Credential::ApiKey { key })) => {
                keys.insert(
                    agent_id.clone(),
                    AgentKeyEntry {
                        key,
                        id: key_id.to_owned(),
                        created_at: entry.connected_at.unwrap_or(0),
                        last_used: None,
                    },
                );
            }
            Ok(Some(_)) => {
                log::warn!("agent key store entry for {agent_id} is not an API key");
            }
            Ok(None) => {
                log::warn!("agent key store entry for {agent_id} is missing");
            }
            Err(error) => {
                log::warn!("could not read agent key for {agent_id}: {error}");
            }
        }
    }
    keys
}

