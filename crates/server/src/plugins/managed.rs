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
pub struct ManagedHosts {
    auth: Arc<PluginAuthStore>,
    /// `{spec}|{target}` → running plugin process.
    hosts: Mutex<HashMap<String, Arc<HostedPlugin>>>,
    /// `{spec}|{target}|{account}` → (models, fetched_at).
    models: Mutex<HashMap<String, (Vec<PluginModel>, Instant)>>,
}

impl ManagedHosts {
    /// Create a manager that hands `auth` to every spawned plugin process
    /// (the JS host persists credentials there via `client.auth.set`).
    pub fn with_store(auth: Arc<PluginAuthStore>) -> Self {
        Self {
            auth,
            hosts: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
        }
    }

    /// The plugin subprocess for `r`, spawned on first use.
    ///
    /// A cached host is reused while its process still answers; a dead
    /// process is restarted once (the stale entry is dropped and a fresh
    /// subprocess takes its place). Callers still treat RPC errors on the
    /// returned host as transport failures: the liveness probe cannot rule
    /// out the process dying right after it.
    pub async fn host_for(&self, r: &PluginRef) -> Result<Arc<HostedPlugin>, PluginError> {
        let key = host_key(r);
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
}
