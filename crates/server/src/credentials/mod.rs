//! Typed provider credentials and token sources.
//!
//! [`TokenSource`] abstracts how "credential → upstream auth headers" is
//! materialized, so today's static API-key auth can later be extended with
//! OAuth or plugin-backed credentials without touching call sites.

use std::future::Future;

use crate::auth::{AuthError, build_headers_for_scheme, default_auth_scheme};
use crate::config::{AuthScheme, ProviderConfig};

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
    },
}

/// A [`TokenSource`] that renders a static API key (plus optional extra
/// headers) into upstream auth headers.
#[derive(Debug, Clone)]
pub struct ApiKeySource {
    scheme: AuthScheme,
    api_key: String,
    extra_headers: Vec<(String, String)>,
}

impl ApiKeySource {
    /// Build a source from provider-level config.
    ///
    /// Mirrors the legacy `build_upstream_auth_headers_for_provider` logic:
    /// the scheme defaults to the provider format's default scheme, and
    /// provider-level extra headers are appended after the scheme headers.
    pub fn for_provider(provider: &ProviderConfig) -> Self {
        Self {
            scheme: provider
                .auth
                .scheme
                .unwrap_or_else(|| default_auth_scheme(provider.format)),
            api_key: provider.api_key.clone(),
            extra_headers: provider
                .auth
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// Synchronous materialization (used by the legacy compat shim in
    /// `auth::build_upstream_auth_headers_for_provider`).
    pub(crate) fn materialize_headers(&self) -> Vec<(String, String)> {
        let mut headers = build_headers_for_scheme(self.scheme, &self.api_key);
        headers.extend(self.extra_headers.iter().cloned());
        headers
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
        let mut headers = build_headers_for_scheme(self.scheme, &self.api_key);
        headers.extend(self.extra_headers.iter().cloned());
        Ok(headers)
    }
}

pub mod account;
pub mod copilot;
pub mod gemini;
pub mod store;
pub mod token;

pub use account::{AccountError, AccountResolver, ResolvedAccount};
pub use copilot::{
    CopilotAccount, CopilotEndpoints, CopilotSignIn, DeviceCodeStarted, ReqwestDeviceHttp,
};
pub use gemini::{GeminiAccount, GeminiEndpoints, GeminiSignIn, SignInError, SignInStarted};
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
        ProviderConfig {
            name: "test".into(),
            format,
            base_url: "https://example.com".into(),
            api_key: "sk-test".into(),
            account: None,
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
    async fn test_parity_anthropic_default_for_claude() {
        let provider = sample_provider(Format::Claude, None, &[]);
        let headers = assert_parity(&provider).await;
        assert_eq!(
            headers,
            vec![
                ("x-api-key".to_string(), "sk-test".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
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
