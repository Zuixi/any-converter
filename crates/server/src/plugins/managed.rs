//! [`ManagedHosts`]: lazy lifecycle management for the plugin subprocesses
//! behind providers with a `plugin` config entry.
//!
//! One [`HostedPlugin`] subprocess per `{spec, target}` pair, spawned on
//! first use and shared by every request; a dead process is restarted once
//! when detected. The plugin's model list is cached per
//! `{spec, target, account}` for 60 seconds (magpie keeps a similar
//! short-lived cache) and invalidated on sign-in/sign-out.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::authstore::PluginAuthStore;
use super::host::{HostedPlugin, default_launch};
use super::{
    LoadParams, ModelsParams, PluginError, PluginModel, PluginProviderInfo, PluginSpec,
    ProviderPlugin,
};
use crate::config::PluginRef;

/// How long a plugin model list stays cached (magpie modelsTTL).
const MODELS_TTL: Duration = Duration::from_secs(60);

/// Cache key for one plugin process: the spec/target pair.
fn host_key(r: &PluginRef) -> String {
    format!("{}|{}", r.spec, r.target)
}

/// Placeholder provider metadata used until the init handshake reports the
/// plugin's real `{id, name, methods}` (P6.1: `HostedPlugin::start` replaces
/// it with the init reply's `providers` when that carries one).
fn placeholder_info(spec: &str) -> PluginProviderInfo {
    PluginProviderInfo {
        id: spec.to_string(),
        name: spec.to_string(),
        methods: Vec::new(),
    }
}

/// Whether a cached plugin subprocess still answers RPCs.
///
/// The [`ProviderPlugin`] surface has no transport introspection, so
/// liveness is probed with the cheapest side-effect-free RPC: `load` for an
/// unsigned account is answered by the JS host from local state without
/// invoking the plugin's loader hook.
async fn host_is_alive(host: &HostedPlugin) -> bool {
    ProviderPlugin::load(
        host,
        LoadParams {
            provider: host.info().id.clone(),
            account: String::new(),
            proxy: None,
        },
    )
    .await
    .is_ok()
}

/// Hosts for every plugin provider referenced by the config.
/// One installed plugin: the spec, what it serves, and why it may not load.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub spec: String,
    pub providers: Vec<PluginProviderInfo>,
    pub error: String,
}

/// What a package was found to serve, and the state of its directory when
/// we found it (`plugin-providers.json`, as magpie keeps it).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Known {
    /// Modification time and size of the package directory, so an upgrade
    /// of the package is noticed without loading it.
    stamp: u64,
    #[serde(default)]
    providers: Vec<PluginProviderInfo>,
    #[serde(default)]
    error: String,
}

/// `<config>/plugin-providers.json`.
pub fn known_path() -> std::path::PathBuf {
    crate::config::secrets_path()
        .parent()
        .map(|dir| dir.join("plugin-providers.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("plugin-providers.json"))
}

fn load_known() -> std::collections::BTreeMap<String, Known> {
    std::fs::read(known_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// A cheap fingerprint of the package directory: installed or upgraded, the
/// entry we remembered is stale and the package is loaded again.
fn package_stamp(r: &PluginRef) -> u64 {
    let dir = std::path::Path::new(if r.target.is_empty() { "." } else { &r.target })
        .join("node_modules")
        .join(&r.spec);
    let Ok(meta) = std::fs::metadata(&dir) else {
        return 0;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    mtime ^ meta.len().wrapping_mul(1_000_003)
}

/// Remember what `spec` serves. Best effort: a listing that can't be
/// written is still a correct listing.
fn remember(spec: &str, stamp: u64, providers: &[PluginProviderInfo], error: &str) {
    let mut all = load_known();
    all.insert(
        spec.to_string(),
        Known {
            stamp,
            providers: providers.to_vec(),
            error: error.to_string(),
        },
    );
    if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
        let _ = crate::credentials::store::write_private(&known_path(), &bytes);
    }
}

/// `<config>/plugins`: where `plugin install` puts a package. `ANY_CONVERTER_PLUGINS`
/// overrides it (the same name the CLI uses).
pub fn plugins_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ANY_CONVERTER_PLUGINS") {
        return std::path::PathBuf::from(dir);
    }
    crate::config::secrets_path()
        .parent()
        .map(|dir| dir.join("plugins"))
        .unwrap_or_else(|| std::path::PathBuf::from("plugins"))
}

/// The package names under `node_modules` in `dir` (scopes unfolded one
/// level), sorted so the list is stable.
pub fn installed_specs(dir: &std::path::Path) -> Vec<String> {
    let root = dir.join("node_modules");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name == ".bin" {
            continue;
        }
        if name.starts_with('@') {
            if let Ok(scoped) = std::fs::read_dir(entry.path()) {
                for inner in scoped.flatten() {
                    if let Ok(one) = inner.file_name().into_string() {
                        out.push(format!("{name}/{one}"));
                    }
                }
            }
            continue;
        }
        out.push(name);
    }
    out.sort();
    out
}

pub struct ManagedHosts {
    auth: Arc<PluginAuthStore>,
    /// `{spec}|{target}` → running plugin process.
    hosts: Mutex<HashMap<String, Arc<HostedPlugin>>>,
    /// `{spec}|{target}|{account}` → (models, fetched_at).
    models: Mutex<HashMap<String, (Vec<PluginModel>, Instant)>>,
    /// One start lock per spec, so a thundering herd spawns one host.
    starts: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ManagedHosts {
    /// Create a manager that hands `auth` to every spawned plugin process
    /// (the JS host persists credentials there via `client.auth.set`).
    pub fn with_store(auth: Arc<PluginAuthStore>) -> Self {
        Self {
            auth,
            hosts: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
            starts: Mutex::new(HashMap::new()),
        }
    }

    /// The auth ledger every plugin process writes through. Read-only
    /// callers use it to list signed-in plugin accounts.
    pub fn auth_store(&self) -> &PluginAuthStore {
        &self.auth
    }

    /// The plugin subprocess for `r`, spawned on first use.
    ///
    /// A cached host is reused while its process still answers; a dead
    /// process is restarted once (the stale entry is dropped and a fresh
    /// subprocess takes its place). Callers still treat RPC errors on the
    /// returned host as transport failures: the liveness probe cannot rule
    /// out the process dying right after it.
    /// Start (or reuse) the host for `r`.
    ///
    /// Starting is single-flight per spec: two requests arriving before the
    /// first host is up would otherwise each spawn a Node process, and the
    /// loser (dropped without being referenced again) was a process nobody
    /// shut down.
    pub async fn host_for(&self, r: &PluginRef) -> Result<Arc<HostedPlugin>, PluginError> {
        let key = host_key(r);
        // Cheap path first: an already-running host needs no start lock.
        if let Some(host) = self.hosts.lock().await.get(&key).cloned() {
            if host_is_alive(&host).await {
                return Ok(host);
            }
        }
        let start_lock = {
            let mut locks = self.starts.lock().await;
            Arc::clone(locks.entry(key.clone()).or_default())
        };
        let _starting = start_lock.lock().await;

        // Re-check under the start lock: the winner may have spawned while
        // we waited.
        if let Some(host) = self.hosts.lock().await.get(&key).cloned() {
            if host_is_alive(&host).await {
                return Ok(host);
            }
            log::warn!("plugin host for `{}` is gone; restarting it", r.spec);
            self.hosts.lock().await.remove(&key);
        }
        let host = Arc::new(
            HostedPlugin::start(
                default_launch(vec![PluginSpec::new(r.spec.clone(), r.target.clone())]),
                placeholder_info(&r.spec),
                Arc::clone(&self.auth),
            )
            .await?,
        );
        self.hosts.lock().await.insert(key, host.clone());
        Ok(host)
    }

    /// Start (or reuse) the host for `r` and return the providers it serves.
    ///
    /// Each host process loads one spec, so this is the host's `info()` —
    /// the init reply's provider entry carrying `id`, `name`, and `methods`.
    /// Plugins that would serve several providers would need the full init
    /// reply surfaced by the host.
    pub async fn providers(&self, r: &PluginRef) -> Result<Vec<PluginProviderInfo>, PluginError> {
        let host = self.host_for(r).await?;
        Ok(vec![host.info().clone()])
    }

    /// Every plugin installed under `dir`, with the providers each serves.
    ///
    /// Listing must not start a process per package: an account page that
    /// spawns ten Node runtimes before it can draw is not a listing, it is a
    /// stampede. What each package serves is therefore remembered in
    /// `plugin-providers.json` (magpie keeps the same file for the same
    /// reason) and only a package we have never loaded — or one whose
    /// directory has changed since — is started. A package that won't load
    /// is reported with why, never skipped silently.
    pub async fn installed(&self, dir: &std::path::Path) -> Vec<InstalledPlugin> {
        let known = load_known();
        let mut out = Vec::new();
        for spec in installed_specs(dir) {
            let r = PluginRef {
                spec: spec.clone(),
                target: dir.to_string_lossy().into_owned(),
            };
            let stamp = package_stamp(&r);
            if let Some(entry) = known.get(&spec) {
                if entry.stamp == stamp {
                    out.push(InstalledPlugin {
                        spec,
                        providers: entry.providers.clone(),
                        error: entry.error.clone(),
                    });
                    continue;
                }
            }
            match self.providers(&r).await {
                Ok(providers) => {
                    remember(&spec, stamp, &providers, "");
                    out.push(InstalledPlugin {
                        spec,
                        providers,
                        error: String::new(),
                    });
                }
                Err(err) => {
                    remember(&spec, stamp, &[], &err.to_string());
                    out.push(InstalledPlugin {
                        spec,
                        providers: Vec::new(),
                        error: err.to_string(),
                    });
                }
            }
        }
        out
    }

    /// The models the plugin lists for `account`, cached for 60 seconds.
    pub async fn models_for(
        &self,
        r: &PluginRef,
        account: &str,
    ) -> Result<Vec<PluginModel>, PluginError> {
        let key = format!("{}|{}", host_key(r), account);
        if let Some((models, fetched_at)) = self.models.lock().await.get(&key) {
            if fetched_at.elapsed() < MODELS_TTL {
                return Ok(models.clone());
            }
        }
        let host = self.host_for(r).await?;
        let models = ProviderPlugin::models(
            &*host,
            ModelsParams {
                provider: host.info().id.clone(),
                account: account.to_string(),
                proxy: None,
            },
        )
        .await?;
        self.models
            .lock()
            .await
            .insert(key, (models.clone(), Instant::now()));
        Ok(models)
    }

    /// Drop cached state after a sign-in/sign-out for `account` (an empty
    /// account clears every account of the spec).
    pub async fn invalidate(&self, r: &PluginRef, account: &str) {
        let prefix = format!("{}|", host_key(r));
        self.models.lock().await.retain(|key, _| {
            !(key.starts_with(&prefix) && (account.is_empty() || key[prefix.len()..] == *account))
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    /// Whether Node is on PATH (the fixture host needs it).
    fn node_reachable() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    use super::*;

    fn sample_ref() -> PluginRef {
        PluginRef {
            spec: "plugin-x".into(),
            target: String::new(),
        }
    }

    #[test]
    fn host_key_distinguishes_spec_and_target() {
        assert_eq!(
            host_key(&PluginRef {
                spec: "a".into(),
                target: "t".into()
            }),
            "a|t"
        );
        assert_ne!(
            host_key(&PluginRef {
                spec: "a".into(),
                target: "t1".into()
            }),
            host_key(&PluginRef {
                spec: "a".into(),
                target: "t2".into()
            })
        );
    }

    #[test]
    fn placeholder_info_uses_the_spec_until_init_replaces_it() {
        let info = placeholder_info("/path/to/plugin");
        assert_eq!(info.id, "/path/to/plugin");
        assert_eq!(info.name, "/path/to/plugin");
        assert!(info.methods.is_empty());
    }

    #[tokio::test]
    async fn invalidate_drops_matching_model_cache_entries() {
        let temp = tempfile::tempdir().unwrap();
        let hosts = ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
            temp.path().join("auth.json"),
        )));
        let r = sample_ref();
        // Seed the cache directly: `models_for` would need a real plugin
        // process (covered by the e2e suite).
        {
            let mut cache = hosts.models.lock().await;
            cache.insert(
                format!("{}|one", host_key(&r)),
                (Vec::new(), Instant::now()),
            );
            cache.insert(
                format!("{}|two", host_key(&r)),
                (Vec::new(), Instant::now()),
            );
            cache.insert("other|spec|one".to_string(), (Vec::new(), Instant::now()));
        }

        hosts.invalidate(&r, "one").await;
        let keys: Vec<String> = hosts.models.lock().await.keys().cloned().collect();
        assert!(!keys.contains(&format!("{}|one", host_key(&r))));
        assert!(keys.contains(&format!("{}|two", host_key(&r))));
        assert!(keys.contains(&"other|spec|one".to_string()));

        hosts.invalidate(&r, "").await;
        let keys: Vec<String> = hosts.models.lock().await.keys().cloned().collect();
        assert_eq!(keys, vec!["other|spec|one".to_string()]);
    }
    /// Concurrent first use must spawn ONE host: a racing duplicate used to
    /// leak a Node process nobody shut down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_host_for_spawns_one_host() {
        if !node_reachable() {
            log::debug!("skipping: node not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let hosts = Arc::new(ManagedHosts::with_store(Arc::new(PluginAuthStore::new(
            dir.path().join("auth.json"),
        ))));
        let spec = PluginRef {
            spec: format!("{}/tests/fixtures/plugin-hello", env!("CARGO_MANIFEST_DIR")),
            target: String::new(),
        };
        let mut handles = Vec::new();
        for _ in 0..8 {
            let hosts = Arc::clone(&hosts);
            let spec = spec.clone();
            handles.push(tokio::spawn(
                async move { hosts.host_for(&spec).await.unwrap() },
            ));
        }
        let mut pids = Vec::new();
        for handle in handles {
            pids.push(handle.await.unwrap().pid());
        }
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), 1, "one host process, got {pids:?}");
    }
}
