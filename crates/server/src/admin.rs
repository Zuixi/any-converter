//! HTTP management surface and management authentication middleware.

use std::sync::Arc;

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, rejection::JsonRejection},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config_store::{ConfigRevision, ConfigStoreError};
use crate::control_plane::{
    AdminConfigView, ConfigPatch, ControlPlane, ControlPlaneError, ControlPlaneStatus,
    ManagementAuthority, SecretMutationResult,
};
use crate::secrets::SecretError;

/// Per-router management runtime. The legacy constructor has no control
/// plane, so admin endpoints fail with 503 instead of mutating a real home.
#[derive(Clone)]
pub struct AdminRuntime {
    pub authority: ManagementAuthority,
    pub control_plane: Option<Arc<ControlPlane>>,
}

impl AdminRuntime {
    pub fn control_plane() -> AdminRuntimeBuilder {
        AdminRuntimeBuilder
    }
}

pub struct AdminRuntimeBuilder;

impl AdminRuntimeBuilder {
    pub fn with_control_plane(self, control_plane: Arc<ControlPlane>) -> AdminRuntime {
        AdminRuntime {
            authority: ManagementAuthority::ControlPlane(Arc::clone(&control_plane)),
            control_plane: Some(control_plane),
        }
    }
}

/// Management auth applies to accounts, agents, usage, and `/v1/admin/*`.
pub async fn require_management(
    Extension(runtime): Extension<AdminRuntime>,
    request: Request<Body>,
    next: Next,
) -> Response {
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
        Err(response) => return response,
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
        Err(response) => return response,
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
        Err(response) => return response,
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
        Err(response) => return response,
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
        Err(response) => return response,
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

fn if_match_revision(headers: &HeaderMap) -> Result<ConfigRevision, Response> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Err(error_response(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "If-Match is required for management mutations",
            Some("read GET /v1/admin/config and send its ETag unchanged"),
        ));
    };
    let Ok(value) = value.to_str() else {
        return Err(invalid_precondition());
    };
    let value = value.trim().trim_matches('"');
    value.parse().map_err(|_| invalid_precondition())
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
        }
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
