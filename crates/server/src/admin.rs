//! HTTP management surface and management authentication middleware.

use std::sync::Arc;

use any_converter_core::convert::Format;
use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Query, rejection::JsonRejection},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::ServerConfig;
use crate::config_store::{ConfigRevision, ConfigStoreError};
use crate::control_plane::{
    AdminConfigView, ConfigPatch, ControlPlane, ControlPlaneError, ControlPlaneStatus,
    ManagementAuthority, SecretMutationResult,
};
use crate::model_discovery::{
    DiscoveryCache, DiscoveryError, DiscoveryErrorKind, DiscoveryReport, DiscoveryStatus,
    cache_key, discover,
};
use crate::secrets::SecretError;
use crate::web_auth::{SessionCheck, WebAuth, check_session};

/// Per-router management runtime. The legacy constructor has no control
/// plane, so admin endpoints fail with 503 instead of mutating a real home.
#[derive(Clone)]
pub struct AdminRuntime {
    pub authority: ManagementAuthority,
    pub control_plane: Option<Arc<ControlPlane>>,
    /// Browser page-session credential; `None` keeps the router
    /// bearer-only (legacy constructor).
    pub web_auth: Option<Arc<WebAuth>>,
    /// Last upstream model lists. Derived data only; never persisted.
    pub discoveries: Arc<DiscoveryCache>,
}

impl AdminRuntime {
    pub fn control_plane() -> AdminRuntimeBuilder {
        AdminRuntimeBuilder
    }

    /// Attach the browser page-session credential. Only the managed
    /// gateway has one; the legacy router stays bearer-only.
    pub fn with_web_auth(self, web_auth: Arc<WebAuth>) -> Self {
        Self {
            web_auth: Some(web_auth),
            ..self
        }
    }
}

pub struct AdminRuntimeBuilder;

impl AdminRuntimeBuilder {
    pub fn with_control_plane(self, control_plane: Arc<ControlPlane>) -> AdminRuntime {
        AdminRuntime {
            authority: ManagementAuthority::ControlPlane(Arc::clone(&control_plane)),
            control_plane: Some(control_plane),
            web_auth: None,
            discoveries: Arc::new(DiscoveryCache::default()),
        }
    }
}

/// Management auth applies to accounts, agents, usage, and `/v1/admin/*`.
pub async fn require_management(
    Extension(runtime): Extension<AdminRuntime>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // A supplied bearer never falls back to the page session: it passes on
    // its own or the request fails closed.
    if request.headers().contains_key(header::AUTHORIZATION) {
        return authorize_bearer(runtime, request, next).await;
    }
    // No bearer: the session cookie is the browser credential. Cookie auth
    // is loopback-only and same-origin-only.
    if let Some(web_auth) = runtime.web_auth.as_deref() {
        match check_session(request.headers(), request.method(), web_auth) {
            SessionCheck::Valid => return next.run(request).await,
            SessionCheck::Forbidden => {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "management_forbidden",
                    "page session cannot manage this server",
                    Some("page sessions work only from the loopback with a same-origin fetch"),
                );
            }
            SessionCheck::Missing | SessionCheck::Invalid => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "authentication_error",
                    "missing management API key",
                    Some("send the management key as `Authorization: Bearer <key>`"),
                );
            }
        }
    }
    error_response(
        StatusCode::UNAUTHORIZED,
        "authentication_error",
        "missing management API key",
        Some("send the management key as `Authorization: Bearer <key>`"),
    )
}

/// The bearer branch of [`require_management`], unchanged from the
/// bearer-only behavior: malformed -> 401, disabled -> 503, valid -> pass,
/// invalid -> 403.
async fn authorize_bearer(runtime: AdminRuntime, request: Request<Body>, next: Next) -> Response {
    let Some(header_value) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing management API key",
            Some("send the management key as `Authorization: Bearer <key>`"),
        );
    };
    let Some(provided) = header_value.strip_prefix("Bearer ") else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "malformed Authorization header",
            Some("send the management key as `Authorization: Bearer <key>`"),
        );
    };
    if matches!(runtime.authority, ManagementAuthority::Disabled) {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "management_unavailable",
            "management authentication is not configured",
            Some(
                "start the server through the managed launcher so it can bootstrap a management key",
            ),
        );
    }
    if runtime.authority.verify(provided) {
        return next.run(request).await;
    }
    error_response(
        StatusCode::FORBIDDEN,
        "management_forbidden",
        "credential cannot manage this server",
        Some("agent and inference keys are inference-only; use the management key"),
    )
}

/// `GET /v1/admin/config`
pub async fn get_config(Extension(runtime): Extension<AdminRuntime>) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    match control_plane.config_view() {
        Ok(view) => with_config_etag(StatusCode::OK, &view),
        Err(error) => control_plane_error(error),
    }
}

/// `PATCH /v1/admin/config`
pub async fn patch_config(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
    body: Result<Json<ConfigPatch>, JsonRejection>,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let Json(patch) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                rejection.body_text(),
                None,
            );
        }
    };
    let expected = match if_match_revision(&headers) {
        Ok(revision) => revision,
        Err(response) => return *response,
    };
    match control_plane.patch_config(&expected, patch) {
        Ok(view) => with_config_etag(StatusCode::OK, &view),
        Err(error) => control_plane_error(error),
    }
}

/// `GET /v1/admin/server/status`
pub async fn get_status(Extension(runtime): Extension<AdminRuntime>) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    match control_plane.status() {
        Ok(status) => Json(AdminStatusResponse::from(status)).into_response(),
        Err(error) => control_plane_error(error),
    }
}

/// `POST /v1/admin/providers/discover`
///
/// Ask an upstream which models it offers. Body either names a saved
/// provider or carries a not-yet-saved `format` + `base_url` + `api_key`.
/// This is read-only, so no `If-Match` applies.
pub async fn discover_provider(
    Extension(runtime): Extension<AdminRuntime>,
    body: Result<Json<DiscoverProviderBody>, JsonRejection>,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                rejection.body_text(),
                None,
            );
        }
    };

    let saved = match body.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => match control_plane.provider_connection(name) {
            Ok(Some(connection)) => Some(connection),
            Ok(None) => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    "provider_not_found",
                    format!("provider `{name}` does not exist"),
                    Some("save the provider first, or omit `name` to test unsaved settings"),
                );
            }
            Err(error) => return control_plane_error(error),
        },
        None => None,
    };

    if let Some(connection) = saved {
        if connection.plugin {
            return Json(DiscoveryReport::unsupported_now(
                DiscoveryErrorKind::UnsupportedFormat,
                "this provider is served by a plugin, which owns its model catalog",
                Some("open the account's model list instead"),
            ))
            .into_response();
        }
        if connection.account_based {
            return Json(DiscoveryReport::unsupported_now(
                DiscoveryErrorKind::UnsupportedFormat,
                "this provider signs in per account and has no key-based model list",
                Some("models come from the signed-in account; add them manually if needed"),
            ))
            .into_response();
        }
        let key = cache_key(
            &connection.name,
            connection.format,
            &connection.base_url,
            connection.auth_scheme,
        );
        if body.refresh {
            runtime.discoveries.invalidate(&key);
        } else if let Some(report) = runtime.discoveries.get(&key) {
            return Json(report).into_response();
        }
        let report = discover(
            connection.format,
            &connection.base_url,
            connection.api_key.as_deref().unwrap_or_default(),
            Some(&connection.auth_headers),
            connection.proxy.as_deref(),
        )
        .await;
        runtime.discoveries.put(key, report.clone());
        return Json(report).into_response();
    }

    let (Some(format), Some(base_url)) = (body.format, body.base_url) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "discovery needs either `name` or `format` and `base_url`",
            Some("send a saved provider's name, or the settings you are about to save"),
        );
    };
    let api_key = body.api_key.as_deref().unwrap_or_default();
    // Sign the probe exactly like generation will: the chosen scheme, or the
    // endpoint's default when the form has not picked one.
    let auth_headers = (!api_key.is_empty()).then(|| {
        let scheme = body
            .auth_scheme
            .unwrap_or(crate::config::AuthScheme::Auto)
            .resolve(format, &base_url);
        crate::auth::build_provider_upstream_headers(format, scheme, api_key, &[])
    });
    let report = discover(format, &base_url, api_key, auth_headers.as_deref(), None).await;
    Json(report).into_response()
}

/// `GET /v1/admin/providers/{name}/models?refresh=1`
pub async fn get_provider_models(
    Extension(runtime): Extension<AdminRuntime>,
    Path(name): Path<String>,
    Query(query): Query<RefreshQuery>,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let connection = match control_plane.provider_connection(&name) {
        Ok(Some(connection)) => connection,
        Ok(None) => {
            return error_response(
                StatusCode::NOT_FOUND,
                "provider_not_found",
                format!("provider `{name}` does not exist"),
                None,
            );
        }
        Err(error) => return control_plane_error(error),
    };
    if connection.plugin || connection.account_based {
        return Json(DiscoveryReport::unsupported_now(
            DiscoveryErrorKind::UnsupportedFormat,
            "this provider's model catalog is managed by its plugin or signed-in account",
            Some("open the account's model list instead"),
        ))
        .into_response();
    }

    let key = cache_key(
        &connection.name,
        connection.format,
        &connection.base_url,
        connection.auth_scheme,
    );
    if query.wants_refresh() {
        runtime.discoveries.invalidate(&key);
    } else if let Some(report) = runtime.discoveries.get(&key) {
        return Json(report).into_response();
    }
    let report = discover(
        connection.format,
        &connection.base_url,
        connection.api_key.as_deref().unwrap_or_default(),
        Some(&connection.auth_headers),
        connection.proxy.as_deref(),
    )
    .await;
    runtime.discoveries.put(key, report.clone());
    Json(report).into_response()
}

/// `GET /v1/admin/catalog`
///
/// One view of what each provider offers upstream, what the gateway
/// exposes, and whether the exposed ids resolve to a route.
pub async fn get_catalog(Extension(runtime): Extension<AdminRuntime>) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    match control_plane.config_snapshot() {
        Ok((revision, config)) => {
            Json(build_catalog(&runtime.discoveries, &revision, &config)).into_response()
        }
        Err(error) => control_plane_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverProviderBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub format: Option<Format>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Signing scheme for an unsaved provider; omit for the format default.
    #[serde(default)]
    pub auth_scheme: Option<crate::config::AuthScheme>,
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct RefreshQuery {
    #[serde(default)]
    refresh: Option<String>,
}

impl RefreshQuery {
    fn wants_refresh(&self) -> bool {
        self.refresh
            .as_deref()
            .is_some_and(|value| matches!(value, "1" | "true" | "yes"))
    }
}

#[derive(Debug, Serialize)]
pub struct CatalogView {
    pub revision: ConfigRevision,
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Serialize)]
pub struct CatalogProvider {
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub plugin: bool,
    pub account_based: bool,
    pub exposed_count: usize,
    pub discovered_count: usize,
    pub checked_at_unix: Option<u64>,
    pub status: Option<DiscoveryStatus>,
    pub error: Option<DiscoveryError>,
}

#[derive(Debug, Serialize)]
pub struct CatalogModel {
    pub id: String,
    pub provider: String,
    pub upstream: String,
    pub exposed: bool,
    pub routed: bool,
    pub source: &'static str,
    pub capabilities: Vec<String>,
}

fn build_catalog(
    cache: &DiscoveryCache,
    revision: &ConfigRevision,
    config: &ServerConfig,
) -> CatalogView {
    let mut providers = Vec::with_capacity(config.providers.len());
    let mut models = Vec::new();

    for provider in &config.providers {
        let format = provider.effective_api().to_format();
        let scheme = crate::credentials::effective_auth_scheme(provider);
        let report = cache.get(&cache_key(
            &provider.name,
            format,
            &provider.base_url,
            scheme,
        ));
        let mut exposed_count = 0;

        for (model, upstream) in &provider.model_map {
            if model == "*" {
                continue;
            }
            exposed_count += 1;
            let routed = config.resolve_provider(model).is_some_and(|route| {
                route
                    .provider_names
                    .iter()
                    .any(|name| name == &provider.name)
            });
            models.push(CatalogModel {
                id: model.clone(),
                provider: provider.name.clone(),
                upstream: upstream.clone(),
                exposed: true,
                routed,
                source: "configured",
                capabilities: vec!["chat".to_owned()],
            });
        }

        let discovered_count = report.as_ref().map_or(0, |report| report.count);
        if let Some(report) = &report {
            for discovered in &report.models {
                if provider.model_map.contains_key(&discovered.id) {
                    continue;
                }
                models.push(CatalogModel {
                    id: discovered.id.clone(),
                    provider: provider.name.clone(),
                    upstream: discovered.id.clone(),
                    exposed: false,
                    routed: false,
                    source: "upstream",
                    capabilities: discovered.capabilities.clone(),
                });
            }
        }

        providers.push(CatalogProvider {
            name: provider.name.clone(),
            format: format.as_str().to_owned(),
            base_url: provider.base_url.clone(),
            plugin: provider.plugin.is_some(),
            account_based: !provider.account_ids().is_empty(),
            exposed_count,
            discovered_count,
            checked_at_unix: report.as_ref().map(|report| report.checked_at_unix),
            status: report.as_ref().map(|report| report.status),
            error: report.and_then(|report| report.error),
        });
    }

    CatalogView {
        revision: revision.clone(),
        providers,
        models,
    }
}

/// `PUT /v1/admin/secrets/provider/{provider}`
pub async fn put_provider_secret(
    Extension(runtime): Extension<AdminRuntime>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ApiKeyBody>, JsonRejection>,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                rejection.body_text(),
                None,
            );
        }
    };
    let expected = match if_match_revision(&headers) {
        Ok(revision) => revision,
        Err(response) => return *response,
    };
    match control_plane.put_provider_key(&expected, &provider, &body.api_key) {
        Ok(result) => secret_result_response(result),
        Err(error) => control_plane_error(error),
    }
}

/// `DELETE /v1/admin/secrets/provider/{provider}`
pub async fn delete_provider_secret(
    Extension(runtime): Extension<AdminRuntime>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let expected = match if_match_revision(&headers) {
        Ok(revision) => revision,
        Err(response) => return *response,
    };
    match control_plane.delete_provider_key(&expected, &provider) {
        Ok(result) => secret_result_response(result),
        Err(error) => control_plane_error(error),
    }
}

/// `PUT /v1/admin/secrets/inference`
pub async fn put_inference_secret(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
    body: Result<Json<ApiKeyBody>, JsonRejection>,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                rejection.body_text(),
                None,
            );
        }
    };
    let expected = match if_match_revision(&headers) {
        Ok(revision) => revision,
        Err(response) => return *response,
    };
    match control_plane.put_inference_key(&expected, &body.api_key) {
        Ok(result) => secret_result_response(result),
        Err(error) => control_plane_error(error),
    }
}

/// `DELETE /v1/admin/secrets/inference`
pub async fn delete_inference_secret(
    Extension(runtime): Extension<AdminRuntime>,
    headers: HeaderMap,
) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    let expected = match if_match_revision(&headers) {
        Ok(revision) => revision,
        Err(response) => return *response,
    };
    match control_plane.delete_inference_key(&expected) {
        Ok(result) => secret_result_response(result),
        Err(error) => control_plane_error(error),
    }
}

/// `POST /v1/admin/secrets/management/rotate`
pub async fn rotate_management_secret(Extension(runtime): Extension<AdminRuntime>) -> Response {
    let Some(control_plane) = runtime.control_plane else {
        return control_plane_unavailable();
    };
    match control_plane.rotate_management_key() {
        Ok(result) => secret_result_response(result),
        Err(error) => control_plane_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyBody {
    pub api_key: String,
}

#[derive(Debug, Serialize)]
struct AdminStatusResponse {
    run_state: &'static str,
    config_revision: ConfigRevision,
    restart_required: bool,
    management_key_configured: bool,
    ready_address: Option<std::net::SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_unix: Option<u64>,
}

impl From<ControlPlaneStatus> for AdminStatusResponse {
    fn from(status: ControlPlaneStatus) -> Self {
        Self {
            run_state: "running",
            config_revision: status.revision,
            restart_required: status.restart_required,
            management_key_configured: status.management_key_configured,
            ready_address: status.ready_address,
            started_at_unix: status.started_at.and_then(|started| {
                started
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs())
            }),
        }
    }
}

fn with_config_etag(status: StatusCode, view: &AdminConfigView) -> Response {
    let etag = format!("\"{}\"", view.revision);
    let mut response = (status, Json(view)).into_response();
    if let Ok(value) = etag.parse() {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

fn secret_result_response(result: SecretMutationResult) -> Response {
    let etag = format!("\"{}\"", result.revision);
    let mut response = Json(result).into_response();
    if let Ok(value) = etag.parse() {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

fn if_match_revision(headers: &HeaderMap) -> Result<ConfigRevision, Box<Response>> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Err(Box::new(error_response(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "If-Match is required for management mutations",
            Some("read GET /v1/admin/config and send its ETag unchanged"),
        )));
    };
    let Ok(value) = value.to_str() else {
        return Err(Box::new(invalid_precondition()));
    };
    let value = value.trim().trim_matches('"');
    value.parse().map_err(|_| Box::new(invalid_precondition()))
}

fn invalid_precondition() -> Response {
    error_response(
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
        "If-Match is not a valid config revision",
        Some("read GET /v1/admin/config and send its ETag unchanged"),
    )
}

fn control_plane_error(error: ControlPlaneError) -> Response {
    match error {
        ControlPlaneError::Precondition(current) => {
            let mut response = error_response(
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
                "configuration changed since it was read",
                Some("re-read GET /v1/admin/config and retry"),
            );
            if let Ok(value) = format!("\"{current}\"").parse() {
                response.headers_mut().insert(header::ETAG, value);
            }
            response
        }
        ControlPlaneError::Config(ConfigStoreError::Conflict { .. }) => error_response(
            StatusCode::CONFLICT,
            "config_revision_conflict",
            "another writer changed the configuration",
            Some("re-read GET /v1/admin/config and retry"),
        ),
        ControlPlaneError::AlreadyInitialized(_) => error_response(
            StatusCode::CONFLICT,
            "config_already_initialized",
            error.to_string(),
            None,
        ),
        ControlPlaneError::Config(error) => {
            let status = match error {
                ConfigStoreError::Validation(_)
                | ConfigStoreError::Mutation(_)
                | ConfigStoreError::InvalidConfig(_)
                | ConfigStoreError::InvalidDocument(_)
                | ConfigStoreError::RedactedSecret => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error_response(status, "config_error", error.to_string(), None)
        }
        ControlPlaneError::Secret(SecretError::Missing(_))
        | ControlPlaneError::Secret(SecretError::Keychain { .. }) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "keychain_unavailable",
            error.to_string(),
            Some("unlock the OS keychain and retry; the operation failed closed"),
        ),
        ControlPlaneError::Secret(error) => error_response(
            StatusCode::BAD_REQUEST,
            "secret_error",
            error.to_string(),
            None,
        ),
        ControlPlaneError::SecretReference(error) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_secret_reference",
            error.to_string(),
            None,
        ),
        ControlPlaneError::InvalidPatch(message) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_patch", message, None)
        }
        ControlPlaneError::InvalidProviderName(provider) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_provider_name",
            format!("invalid provider name: `{provider}`"),
            None,
        ),
        ControlPlaneError::Validation(message) => error_response(
            StatusCode::BAD_REQUEST,
            "config_validation_failed",
            message,
            None,
        ),
        ControlPlaneError::WebKey(message) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "web_key_invalid",
            message,
            Some("fix ANY_CONVERTER_WEB_KEY or <home>/web-key and restart"),
        ),
        ControlPlaneError::ManagementKeyMissing
        | ControlPlaneError::ManagementKeyInvalidReference(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "management_key_unavailable",
            error.to_string(),
            Some("restart through the managed launcher to repair the management key"),
        ),
    }
}

fn control_plane_unavailable() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "management_unavailable",
        "this router has no control plane",
        Some("start the server through the managed launcher"),
    )
}

fn error_response(
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    hint: Option<&str>,
) -> Response {
    let mut body = json!({
        "error": {
            "code": code,
            "message": message.to_string(),
        }
    });
    if let Some(hint) = hint {
        body["error"]["hint"] = json!(hint);
    }
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::secrets::KEYCHAIN_SERVICE;
    use crate::secrets::{ApiKeyStore, MemoryKeychain};
    use std::sync::Arc;

    fn fixed_runtime(key: Option<&str>) -> AdminRuntime {
        AdminRuntime {
            authority: ManagementAuthority::from_legacy_key(key),
            control_plane: None,
            web_auth: None,
            discoveries: Arc::new(DiscoveryCache::default()),
        }
    }

    fn catalog_fixture() -> CatalogView {
        let document = crate::config_store::ConfigDocument::parse(
            r#"
[server]
host = "127.0.0.1"
port = 3028

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "keychain:provider:deepseek"
model_map = { "deepseek-chat" = "deepseek-chat" }

[[providers]]
name = "local"
format = "openai_chat"
base_url = "http://127.0.0.1:11434"
api_key = "ollama"
model_map = { "qwen2.5" = "qwen2.5" }

[[model_routes]]
pattern = "deepseek-chat"
providers = ["deepseek"]
"#,
        )
        .unwrap();
        let cache = DiscoveryCache::default();
        let provider = &document.config().providers[0];
        cache.put(
            cache_key(
                &provider.name,
                provider.effective_api().to_format(),
                &provider.base_url,
                crate::credentials::effective_auth_scheme(provider),
            ),
            DiscoveryReport {
                status: DiscoveryStatus::Ok,
                source: "upstream".to_owned(),
                checked_at_unix: 42,
                latency_ms: 3,
                count: 2,
                models: vec![
                    crate::model_discovery::DiscoveredModel {
                        id: "deepseek-chat".to_owned(),
                        display_name: None,
                        capabilities: vec!["chat".to_owned()],
                    },
                    crate::model_discovery::DiscoveredModel {
                        id: "deepseek-reasoner".to_owned(),
                        display_name: None,
                        capabilities: vec!["chat".to_owned()],
                    },
                ],
                error: None,
            },
        );
        build_catalog(&cache, document.revision(), document.config())
    }

    #[test]
    fn catalog_separates_exposed_from_discovered_and_reports_routing() {
        let catalog = catalog_fixture();
        let deepseek = &catalog.providers[0];
        assert_eq!(deepseek.exposed_count, 1);
        assert_eq!(deepseek.discovered_count, 2);
        assert_eq!(deepseek.status, Some(DiscoveryStatus::Ok));

        let exposed = catalog
            .models
            .iter()
            .find(|model| model.id == "deepseek-chat")
            .unwrap();
        assert!(exposed.exposed);
        assert!(exposed.routed, "the explicit route must resolve");

        let discovered = catalog
            .models
            .iter()
            .find(|model| model.id == "deepseek-reasoner")
            .unwrap();
        assert!(!discovered.exposed);
        assert!(!discovered.routed);
        assert_eq!(discovered.source, "upstream");

        let unrouted = catalog
            .models
            .iter()
            .find(|model| model.id == "qwen2.5")
            .unwrap();
        assert!(unrouted.exposed);
        assert!(
            !unrouted.routed,
            "no route means the model cannot be served"
        );
    }

    #[test]
    fn fixed_authority_compares_exactly() {
        let runtime = fixed_runtime(Some("secret"));
        assert!(runtime.authority.verify("secret"));
        assert!(!runtime.authority.verify("Secret"));
        assert!(!runtime.authority.verify("secret "));
    }

    #[test]
    fn control_plane_authority_uses_rotated_key() {
        let root =
            std::env::temp_dir().join(format!("any-converter-admin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let backend = Arc::new(MemoryKeychain::new());
        let keys = ApiKeyStore::with_backend(KEYCHAIN_SERVICE, backend);
        let prepared =
            ControlPlane::prepare(crate::home::Home::from_root(root.clone()), keys).unwrap();
        let before = prepared.control_plane.management_key();
        let runtime = AdminRuntime::control_plane().with_control_plane(prepared.control_plane);

        assert!(runtime.authority.verify(&before));
        let old = runtime.control_plane.as_ref().unwrap().management_key();
        runtime
            .control_plane
            .as_ref()
            .unwrap()
            .rotate_management_key()
            .unwrap();
        let new = runtime.control_plane.as_ref().unwrap().management_key();
        assert_ne!(old, new);
        assert!(!runtime.authority.verify(&old));
        assert!(runtime.authority.verify(&new));
        let _ = std::fs::remove_dir_all(root);
    }
}
