//! Typed provider credentials and token sources.
//!
//! [`TokenSource`] abstracts how "credential → upstream auth headers" is
//! materialized, so today's static API-key auth can later be extended with
//! OAuth or plugin-backed credentials without touching call sites.

use std::future::Future;

use any_converter_core::convert::Format;

use crate::auth::AuthError;
use crate::config::{AuthScheme, ProviderConfig};

/// The scheme actually used for a provider: an explicit non-`auto` scheme
/// wins; otherwise the endpoint default applies (compatible Claude gateways
/// sign with Bearer), and Anthropic OAuth tokens sign with Bearer on any
/// host because that is the credential kind they are.
pub fn effective_auth_scheme(provider: &ProviderConfig) -> AuthScheme {
    if provider.auth.scheme.is_none()
        && provider.format == Format::Claude
        && is_anthropic_oauth_token(&provider.api_key)
    {
        return AuthScheme::Bearer;
    }
    provider
        .auth
        .scheme
        .unwrap_or(AuthScheme::Auto)
        .resolve(provider.format, &provider.base_url)
}

/// Anthropic OAuth tokens (`sk-ant-oat…`, issued by Claude subscription
/// sign-ins) authenticate as `Authorization: Bearer`, never `x-api-key`.
pub(crate) fn is_anthropic_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

/// A typed provider credential.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A literal API key from config.
    ApiKey { key: String },
    /// An OAuth sign-in result (e.g. Gemini browser flow, Copilot device
    /// flow) persisted by `any-converter accounts add`.
    #[serde(rename = "oauth")]
    OAuth {
        provider: String,
        account: String,
        access_token: String,
        refresh_token: Option<String>,
        expires_at_unix: Option<i64>,
        /// What only a Sign in with ChatGPT account needs — the client id
        /// OpenAI issued at sign-in, this machine's id, the last ID token
        /// and the granted scopes. Kept in the same entry as the tokens on
        /// purpose: two ledgers drift, and a lost client id is an account
        /// that can never refresh again (magpie keeps one `siwcCreds`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        siwc: Option<crate::credentials::SiwcMeta>,
    },
}

/// A [`TokenSource`] that renders a static API key (plus optional extra
/// headers) into upstream auth headers.
#[derive(Debug, Clone)]
pub struct ApiKeySource {
    /// The protocol the outbound request speaks; drives wire-required
    /// headers such as Claude's `anthropic-version`.
    wire_format: Format,
    scheme: AuthScheme,
    api_key: String,
    extra_headers: Vec<(String, String)>,
}

impl ApiKeySource {
    /// Build a source from provider-level config.
    ///
    /// The scheme defaults to the endpoint's default scheme, and
    /// provider-level extra headers are appended after the scheme headers.
    pub fn for_provider(provider: &ProviderConfig) -> Self {
        Self {
            wire_format: provider.effective_api().to_format(),
            scheme: effective_auth_scheme(provider),
            api_key: provider.api_key.clone(),
            extra_headers: provider
                .auth
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn build_headers(&self) -> Vec<(String, String)> {
        crate::auth::build_provider_upstream_headers(
            self.wire_format,
            self.scheme,
            &self.api_key,
            &self.extra_headers,
        )
    }

    /// Synchronous materialization (used by the legacy compat shim in
    /// `auth::build_upstream_auth_headers_for_provider`).
    pub(crate) fn materialize_headers(&self) -> Vec<(String, String)> {
        self.build_headers()
    }
}

/// Anything that can produce upstream auth headers per request.
///
/// Implemented with RPITIT (return-position impl trait in trait) so no
/// `async_trait` dependency is needed; object-safety is not required by
/// current call sites.
pub trait TokenSource: Send + Sync {
    /// Headers to attach to the upstream request (e.g. `Authorization`).
    fn auth_headers(&self)
    -> impl Future<Output = Result<Vec<(String, String)>, AuthError>> + Send;
}

impl TokenSource for ApiKeySource {
    async fn auth_headers(&self) -> Result<Vec<(String, String)>, AuthError> {
        Ok(self.build_headers())
    }
}

pub mod account;
pub mod agent_import;
pub mod allowance;
pub mod claude_signin;
pub mod codex_signin;
pub mod codex_store;
pub mod copilot;
pub mod gemini;
pub mod gemini_models;
pub mod import;
pub mod siwc_signin;
pub mod siwc_store;

pub use siwc_signin::{
    ReqwestSiwc, SIWC_DIRECT_SCOPE, SIWC_SCOPES, SIWC_USAGE_URL, SiwcAccount, SiwcEndpoints,
    SiwcError, SiwcSignIn, SiwcStarted, SiwcTokens,
};
pub use siwc_store::SiwcMeta;
pub mod signout;
pub mod store;
pub mod token;

pub use account::{
    AccountError, AccountResolver, CodeAssistProject, ResolvedAccount, codex_home_of, mark_adopted,
    release_adopted,
};
pub use agent_import::{
    FoundAccount, codex_homes, discover, discover_with_env, read_claude, read_codex,
    read_codex_with_env, to_credential,
};
pub use allowance::{Allowance, CopilotAllowances, Limit};
pub use claude_signin::{
    CLAUDE_INSTALL_HINT, CLAUDE_OPENED_URL_ENV, ClaudeAccount, ClaudeCli, ClaudeSignIn,
    ClaudeStarted, find_claude_cli,
};
pub use codex_signin::{CodexAccount, CodexEndpoints, CodexSignIn, CodexStarted};
pub use codex_store::{
    CodexStore, TokenUpdate, WriteBackError, codex_store, write_tokens, write_tokens_with_timeout,
};
pub use copilot::{
    CopilotAccount, CopilotEndpoints, CopilotError, CopilotSession, CopilotSessions, CopilotSignIn,
    DeviceCodeStarted, ReqwestDeviceHttp,
};
pub use gemini::{
    ANTIGRAVITY_RISK, GeminiAccount, GeminiEndpoints, GeminiSignIn, GoogleApp, SignInError,
    SignInStarted,
};
pub use gemini_models::{GEMINI_MODELS, GeminiFlags, flags_from_experiments, offered};
pub use import::{ImportStatus, ImportedAccount, import_path};
pub use store::{FileSecretStore, SecretStore, SecretStoreError};
pub use token::{AuthErr401, OAuthTokens, TokenManager};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::HashMap;

    use any_converter_core::convert::Format;

    use super::*;

    fn sample_provider(
        format: Format,
        scheme: Option<AuthScheme>,
        auth_headers: &[(&str, &str)],
    ) -> ProviderConfig {
        sample_provider_at(format, scheme, auth_headers, "https://example.com")
    }

    fn sample_provider_at(
        format: Format,
        scheme: Option<AuthScheme>,
        auth_headers: &[(&str, &str)],
        base_url: &str,
    ) -> ProviderConfig {
        ProviderConfig {
            name: "test".into(),
            format,
            base_url: base_url.into(),
            api_key: "sk-test".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            model_map: HashMap::new(),
            endpoints: crate::config::ProviderEndpointConfig::default(),
            auth: crate::config::ProviderAuthConfig {
                scheme,
                headers: auth_headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            },
            plugin: None,
            proxy: None,
        }
    }

    /// Byte-for-byte parity between the new token source and the legacy function.
    async fn assert_parity(provider: &ProviderConfig) -> Vec<(String, String)> {
        let got = ApiKeySource::for_provider(provider)
            .auth_headers()
            .await
            .unwrap();
        let expected = crate::auth::build_upstream_auth_headers_for_provider(provider);
        assert_eq!(got, expected);
        got
    }

    #[tokio::test]
    async fn test_parity_bearer_default_for_openai() {
        let provider = sample_provider(Format::OpenAIChat, None, &[]);
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![("Authorization".to_string(), "Bearer sk-test".to_string())]
        );
    }

    #[tokio::test]
    async fn test_parity_x_api_key_explicit_scheme() {
        let provider = sample_provider(Format::OpenAIChat, Some(AuthScheme::XApiKey), &[]);
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![("x-api-key".to_string(), "sk-test".to_string())]
        );
    }

    #[tokio::test]
    async fn test_parity_google_api_key_default_for_gemini() {
        let provider = sample_provider(Format::Gemini, None, &[]);
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![("x-goog-api-key".to_string(), "sk-test".to_string())]
        );
    }

    #[tokio::test]
    async fn test_parity_anthropic_default_on_official_host() {
        let provider = sample_provider_at(Format::Claude, None, &[], "https://api.anthropic.com");
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                ("x-api-key".to_string(), "sk-test".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
    }

    /// Compatible Claude gateways (MiniMax and friends) sign with Bearer but
    /// still speak the versioned Messages wire, so the version header rides
    /// along instead of being dropped with the `x-api-key` default.
    #[tokio::test]
    async fn test_bearer_default_on_compatible_claude_gateway_keeps_version() {
        let provider = sample_provider_at(
            Format::Claude,
            None,
            &[],
            "https://api.minimax.cn/anthropic",
        );
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                ("Authorization".to_string(), "Bearer sk-test".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
    }

    /// Claude subscription OAuth tokens are Bearer credentials no matter
    /// which host they are pointed at.
    #[tokio::test]
    async fn test_oauth_token_signs_bearer_even_on_official_host() {
        let provider = ProviderConfig {
            api_key: "sk-ant-oat12-test".into(),
            ..sample_provider_at(Format::Claude, None, &[], "https://api.anthropic.com")
        };
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                (
                    "Authorization".to_string(),
                    "Bearer sk-ant-oat12-test".to_string()
                ),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
    }

    /// A user-pinned extra header wins over the injected version default.
    #[tokio::test]
    async fn test_custom_anthropic_version_is_not_duplicated() {
        let provider = sample_provider_at(
            Format::Claude,
            None,
            &[("anthropic-version", "2023-01-01")],
            "https://api.minimax.cn/anthropic",
        );
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                ("Authorization".to_string(), "Bearer sk-test".to_string()),
                ("anthropic-version".to_string(), "2023-01-01".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn test_custom_auth_headers_appended() {
        let provider = sample_provider(Format::OpenAIChat, None, &[("api-version", "2024-10-21")]);
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                ("Authorization".to_string(), "Bearer sk-test".to_string()),
                ("api-version".to_string(), "2024-10-21".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn test_none_scheme_yields_no_headers() {
        let provider = sample_provider(Format::OpenAIChat, Some(AuthScheme::None), &[]);
        assert_eq!(
            assert_parity(&provider).await,
            Vec::<(String, String)>::new()
        );
    }
}
