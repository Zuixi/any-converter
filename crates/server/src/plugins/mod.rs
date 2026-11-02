//! Plugin protocol layer: OpenCode-compatible types, the [`ProviderPlugin`]
//! trait, and a [`PluginRegistry`].
//!
//! This module is deliberately free of process and network concerns: it
//! defines the shapes that cross the host↔plugin boundary (auth storage in
//! [`authstore`], RPC method parameters/results mirroring magpie host.js
//! `handlers`) and the trait a concrete plugin runtime implements. The
//! subprocess runtime (node/bun spawn + stdio JSON-RPC) lands in T7 and
//! implements [`ProviderPlugin`] for its transport.

pub mod authstore;
pub mod host;

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use authstore::{AuthStoreError, PluginAuth, PluginAuthStore};
pub use host::{
    HostedPlugin, PluginFetchRequest, PluginFetchResponse, PluginLaunch, PluginTransport,
};

/// Host → plugin RPC method names (magpie host.js `handlers`).
pub mod methods {
    /// Begin an authorization flow for an account.
    pub const AUTHORIZE: &str = "authorize";
    /// Complete an authorization flow with the callback code.
    pub const CALLBACK: &str = "callback";
    /// Store a manually supplied API key.
    pub const API_KEY: &str = "apiKey";
    /// Drop an account's stored credentials.
    pub const SIGN_OUT: &str = "signOut";
    /// Fetch usage/billing information.
    pub const USAGE: &str = "usage";
    /// Load account information.
    pub const LOAD: &str = "load";
    /// List models available to an account.
    pub const MODELS: &str = "models";
}

/// Errors surfaced by plugin RPC calls.
#[derive(Debug, Error)]
pub enum PluginError {
    /// The plugin transport failed or returned an invalid response.
    #[error("rpc error: {0}")]
    Rpc(String),
    /// The plugin violated the protocol (bad params, unknown method, ...).
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The plugin does not implement the requested method.
    #[error("no such method")]
    NoSuchMethod,
    /// Local auth storage failure.
    #[error("storage error: {0}")]
    Storage(String),
}

/// Future returned by every [`ProviderPlugin`] method.
pub type PluginFuture<T> = BoxFuture<'static, Result<T, PluginError>>;

/// The kind of credential a provider issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethodKind {
    /// OAuth device/authorization-code flow.
    Oauth,
    /// Static API key.
    Api,
}

/// The input shape of an auth prompt (OpenCode `PromptDef`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PromptKind {
    /// Free-form single-line text.
    Text,
    /// Masked single-line text.
    Password,
    /// Choose one of `options`.
    Select {
        /// Allowed choices.
        options: Vec<String>,
    },
}

/// One input the host should ask for during an auth flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptDef {
    /// Identifier sent back to the plugin with the answer.
    pub key: String,
    /// Human label shown in the UI.
    pub label: String,
    /// How to render the input.
    pub kind: PromptKind,
}

/// One way to authenticate to a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthMethod {
    /// The credential kind this method yields.
    pub kind: AuthMethodKind,
    /// Inputs the host collects before starting the flow.
    pub prompts: Vec<PromptDef>,
}

/// Static provider metadata advertised by a plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginProviderInfo {
    /// Provider id used in `<provider>:<account>` keys.
    pub id: String,
    /// Human-readable provider name.
    pub name: String,
    /// Supported authentication methods.
    pub methods: Vec<AuthMethod>,
}

/// `authorize` request: begin an auth flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizeParams {
    /// Provider id.
    pub provider: String,
    /// Auth method index into [`PluginProviderInfo::methods`].
    pub method: usize,
    /// Answers keyed by [`PromptDef::key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<BTreeMap<String, Value>>,
    /// Account name to authorize.
    pub account: String,
    /// Optional upstream proxy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// `authorize` response: how to continue the flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Authorization {
    /// Flow handle passed to `callback`.
    pub session: String,
    /// URL the user should visit, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Instructions shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// `"auto"` (browser opened) or `"code"` (user supplies the code).
    pub method: String,
    /// Whether the plugin already opened the URL.
    pub opened: bool,
}

/// `callback` request: finish a flow started by `authorize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallbackParams {
    /// Flow handle from [`Authorization::session`].
    pub session: String,
    /// Code returned by the provider.
    pub code: String,
}

/// `callback` / `apiKey` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthResult {
    /// Whether the credential was stored.
    pub ok: bool,
    /// Failure detail when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Provider id the credential belongs to.
    pub provider: String,
    /// Account name the credential belongs to.
    pub account: String,
}

/// `callback` response (alias; same shape as `apiKey`).
pub type CallbackResult = AuthResult;

/// `apiKey` request: store a manually supplied key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyParams {
    /// Provider id.
    pub provider: String,
    /// Auth method index.
    pub method: usize,
    /// Answers keyed by [`PromptDef::key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<BTreeMap<String, Value>>,
    /// The key itself.
    pub key: String,
    /// Account name.
    pub account: String,
    /// Optional upstream proxy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// `apiKey` response (alias; same shape as `callback`).
pub type ApiKeyResult = AuthResult;

/// `signOut` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignOutParams {
    /// Provider id.
    pub provider: String,
    /// Account name.
    pub account: String,
}

/// `usage` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageParams {
    /// Provider id.
    pub provider: String,
    /// Account name.
    pub account: String,
    /// Optional upstream proxy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// A provider plugin: metadata plus the host↔plugin RPC surface.
///
/// Methods mirror the magpie host.js `handlers` one-to-one. The trait is
/// object-safe (futures boxed via [`PluginFuture`]) so plugins can be held as
/// `Arc<dyn ProviderPlugin>` in the [`PluginRegistry`]; the subprocess
/// runtime (T7) implements this over stdio JSON-RPC.
pub trait ProviderPlugin: Send + Sync {
    /// Static provider metadata.
    fn info(&self) -> &PluginProviderInfo;

    /// Begin an authorization flow (`authorize`).
    fn authorize(&self, params: AuthorizeParams) -> PluginFuture<Authorization>;

    /// Complete a flow (`callback`).
    fn callback(&self, params: CallbackParams) -> PluginFuture<AuthResult>;

    /// Store a manually supplied key (`apiKey`).
    fn api_key(&self, params: ApiKeyParams) -> PluginFuture<AuthResult>;

    /// Drop stored credentials (`signOut`); `Ok(())` on success.
    fn sign_out(&self, params: SignOutParams) -> PluginFuture<()>;

    /// Fetch usage/billing JSON (`usage`); `Ok(None)` when unavailable.
    fn usage(&self, params: UsageParams) -> PluginFuture<Option<Value>>;
}

/// Registry of available plugins keyed by provider id.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, Arc<dyn ProviderPlugin>>,
}

impl PluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `plugin` under its provider id, returning the displaced
    /// plugin if one was already registered.
    pub fn register(&mut self, plugin: Arc<dyn ProviderPlugin>) -> Option<Arc<dyn ProviderPlugin>> {
        self.plugins.insert(plugin.info().id.clone(), plugin)
    }

    /// Look up the plugin registered under `provider`.
    pub fn get(&self, provider: &str) -> Option<Arc<dyn ProviderPlugin>> {
        self.plugins.get(provider).cloned()
    }

    /// Registered provider ids, sorted.
    pub fn list(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    /// A no-op plugin used as registry test filler.
    struct NullPlugin {
        info: PluginProviderInfo,
    }

    impl ProviderPlugin for NullPlugin {
        fn info(&self) -> &PluginProviderInfo {
            &self.info
        }

        fn authorize(&self, _params: AuthorizeParams) -> PluginFuture<Authorization> {
            Box::pin(async { Err(PluginError::NoSuchMethod) })
        }

        fn callback(&self, _params: CallbackParams) -> PluginFuture<AuthResult> {
            Box::pin(async { Err(PluginError::NoSuchMethod) })
        }

        fn api_key(&self, _params: ApiKeyParams) -> PluginFuture<AuthResult> {
            Box::pin(async { Err(PluginError::NoSuchMethod) })
        }

        fn sign_out(&self, _params: SignOutParams) -> PluginFuture<()> {
            Box::pin(async { Err(PluginError::NoSuchMethod) })
        }

        fn usage(&self, _params: UsageParams) -> PluginFuture<Option<Value>> {
            Box::pin(async { Err(PluginError::NoSuchMethod) })
        }
    }

    struct FakePlugin {
        info: PluginProviderInfo,
        store: PluginAuthStore,
    }

    impl FakePlugin {
        fn new(id: &str, store: PluginAuthStore) -> Self {
            Self {
                info: PluginProviderInfo {
                    id: id.into(),
                    name: format!("{id} fake"),
                    methods: vec![
                        AuthMethod {
                            kind: AuthMethodKind::Oauth,
                            prompts: vec![],
                        },
                        AuthMethod {
                            kind: AuthMethodKind::Api,
                            prompts: vec![],
                        },
                    ],
                },
                store,
            }
        }
    }

    impl ProviderPlugin for FakePlugin {
        fn info(&self) -> &PluginProviderInfo {
            &self.info
        }

        fn authorize(&self, p: AuthorizeParams) -> PluginFuture<Authorization> {
            let out = Authorization {
                session: format!("{}:{}:s1", p.provider, p.account),
                url: Some(format!("https://{}/login", p.provider)),
                instructions: Some("visit the url".into()),
                method: "code".into(),
                opened: false,
            };
            Box::pin(async move { Ok(out) })
        }

        fn callback(&self, p: CallbackParams) -> PluginFuture<AuthResult> {
            // session is "<provider>:<account>:s1"
            let key = p
                .session
                .strip_suffix(":s1")
                .unwrap_or(&p.session)
                .to_string();
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() != 2 {
                return Box::pin(async { Err(PluginError::Protocol("bad session".into())) });
            }
            let (provider, account) = (parts[0].to_string(), parts[1].to_string());
            let auth = PluginAuth::OAuth {
                refresh: Some(p.code),
                access: None,
                expires: None,
                extra: BTreeMap::new(),
            };
            let res = match self.store.set(&key, auth) {
                Ok(()) => AuthResult {
                    ok: true,
                    error: None,
                    provider,
                    account,
                },
                Err(e) => AuthResult {
                    ok: false,
                    error: Some(e.to_string()),
                    provider,
                    account,
                },
            };
            Box::pin(async move { Ok(res) })
        }

        fn api_key(&self, p: ApiKeyParams) -> PluginFuture<AuthResult> {
            let key = format!("{}:{}", p.provider, p.account);
            let auth = PluginAuth::ApiKey {
                key: p.key,
                extra: BTreeMap::new(),
            };
            let res = match self.store.set(&key, auth) {
                Ok(()) => AuthResult {
                    ok: true,
                    error: None,
                    provider: p.provider,
                    account: p.account,
                },
                Err(e) => AuthResult {
                    ok: false,
                    error: Some(e.to_string()),
                    provider: p.provider,
                    account: p.account,
                },
            };
            Box::pin(async move { Ok(res) })
        }

        fn sign_out(&self, p: SignOutParams) -> PluginFuture<()> {
            let key = format!("{}:{}", p.provider, p.account);
            let out = self.store.remove(&key).map(|_| ());
            Box::pin(async move { out.map_err(|e| PluginError::Storage(e.to_string())) })
        }

        fn usage(&self, p: UsageParams) -> PluginFuture<Option<Value>> {
            let out = json!({ "provider": p.provider, "account": p.account });
            Box::pin(async move { Ok(Some(out)) })
        }
    }

    fn info(id: &str) -> PluginProviderInfo {
        PluginProviderInfo {
            id: id.into(),
            name: format!("{id} plugin"),
            methods: vec![],
        }
    }

    #[test]
    fn registry_register_get_list() {
        let mut reg = PluginRegistry::new();
        assert!(reg.get("github").is_none());
        assert!(reg.list().is_empty());

        let displaced = reg.register(Arc::new(NullPlugin { info: info("zeta") }));
        assert!(displaced.is_none());
        reg.register(Arc::new(NullPlugin {
            info: info("alpha"),
        }));

        assert_eq!(reg.list(), vec!["alpha".to_string(), "zeta".to_string()]);
        assert!(reg.get("alpha").is_some());
        assert!(reg.get("missing").is_none());

        let displaced = reg.register(Arc::new(NullPlugin { info: info("zeta") }));
        assert!(displaced.is_some());
        assert_eq!(reg.list().len(), 2);
    }

    #[tokio::test]
    async fn fake_plugin_full_flow_stores_credentials() {
        let root = tempfile::tempdir().unwrap().keep();
        let store = PluginAuthStore::new(root.join("auth.json"));
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(FakePlugin::new(
            "github",
            PluginAuthStore::new(root.join("auth.json")),
        )));

        let plugin = reg.get("github").unwrap();
        assert_eq!(plugin.info().id, "github");

        // authorize → callback stores an oauth entry under "github:me".
        let auth = plugin
            .authorize(AuthorizeParams {
                provider: "github".into(),
                method: 0,
                inputs: None,
                account: "me".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(auth.session, "github:me:s1");
        assert_eq!(auth.method, "code");
        assert!(!auth.opened);

        let res = plugin
            .callback(CallbackParams {
                session: auth.session,
                code: "oauth-code-1".into(),
            })
            .await
            .unwrap();
        assert!(res.ok);
        assert_eq!(res.provider, "github");
        assert_eq!(res.account, "me");

        let map = store.load().unwrap();
        assert_eq!(
            map.get("github:me"),
            Some(&PluginAuth::OAuth {
                refresh: Some("oauth-code-1".into()),
                access: None,
                expires: None,
                extra: BTreeMap::new(),
            })
        );

        // apiKey stores an api entry.
        let res = plugin
            .api_key(ApiKeyParams {
                provider: "github".into(),
                method: 1,
                inputs: None,
                key: "sk-1".into(),
                account: "bot".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert!(res.ok);
        assert!(matches!(map_or_api(&store, "github:bot"), Some(k) if k == "sk-1"));

        // usage returns arbitrary JSON.
        let usage = plugin
            .usage(UsageParams {
                provider: "github".into(),
                account: "me".into(),
                proxy: None,
            })
            .await
            .unwrap();
        assert_eq!(usage, Some(json!({"provider": "github", "account": "me"})));

        // signOut removes the entry.
        plugin
            .sign_out(SignOutParams {
                provider: "github".into(),
                account: "me".into(),
            })
            .await
            .unwrap();
        assert!(!store.load().unwrap().contains_key("github:me"));
    }

    fn map_or_api(store: &PluginAuthStore, key: &str) -> Option<String> {
        match store.load().unwrap().get(key) {
            Some(PluginAuth::ApiKey { key, .. }) => Some(key.clone()),
            _ => None,
        }
    }

    #[test]
    fn authorize_json_shape() {
        let params = AuthorizeParams {
            provider: "github".into(),
            method: 0,
            inputs: Some(BTreeMap::from([("org".to_string(), json!("acme"))])),
            account: "me".into(),
            proxy: Some("http://127.0.0.1:7890".into()),
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(
            v,
            json!({
                "provider": "github",
                "method": 0,
                "inputs": {"org": "acme"},
                "account": "me",
                "proxy": "http://127.0.0.1:7890",
            })
        );

        let auth = Authorization {
            session: "s1".into(),
            url: Some("https://example.com".into()),
            instructions: None,
            method: "auto".into(),
            opened: true,
        };
        let v = serde_json::to_value(&auth).unwrap();
        assert_eq!(
            v,
            json!({
                "session": "s1",
                "url": "https://example.com",
                "method": "auto",
                "opened": true,
            })
        );
    }

    #[test]
    fn auth_entry_json_shape() {
        let oauth = PluginAuth::OAuth {
            refresh: Some("r".into()),
            access: Some("a".into()),
            expires: Some(123),
            extra: BTreeMap::from([("custom".to_string(), json!(1))]),
        };
        assert_eq!(
            serde_json::to_value(&oauth).unwrap(),
            json!({"type": "oauth", "refresh": "r", "access": "a", "expires": 123, "custom": 1})
        );

        let api = PluginAuth::ApiKey {
            key: "sk".into(),
            extra: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(&api).unwrap(),
            json!({"type": "api", "key": "sk"})
        );

        // Method/prompt shapes use OpenCode's lowercase type tags.
        let method = AuthMethod {
            kind: AuthMethodKind::Oauth,
            prompts: vec![PromptDef {
                key: "org".into(),
                label: "Organization".into(),
                kind: PromptKind::Select {
                    options: vec!["acme".into()],
                },
            }],
        };
        assert_eq!(
            serde_json::to_value(&method).unwrap(),
            json!({
                "kind": "oauth",
                "prompts": [{
                    "key": "org",
                    "label": "Organization",
                    "kind": {"type": "select", "options": ["acme"]},
                }],
            })
        );
    }
}
