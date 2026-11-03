//! From a plan to bytes: the body converted to the upstream's own API, the
//! URL it goes to, and the reply converted back to the client's API.

use any_converter_core::convert::Format;
use axum::body::Body;
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use log::{debug, error, info, warn};

use crate::config::UpstreamApi;
use crate::credentials::copilot::{COPILOT_DEFAULT_API_BASE, CopilotError, copilot_api_headers};
use crate::credentials::{ApiKeySource, TokenSource};
use crate::model::patch_model_in_body;
use crate::namespace::patch_response_namespaces;
use crate::proxy::build_upstream_url_for_provider;
use crate::usage::{self, UsageRecord};

use super::super::{
    HandlerError, api_error_response, conversion_error_response, safe_convert_request,
    safe_convert_response, sanitize_headers_for_log, truncated_body,
};
use super::{Attempt, RequestContext};

/// Outcome of building the upstream request for one account attempt (P9).
pub(crate) enum AccountStep {
    /// The converted body, URL and headers are ready to send.
    Ready {
        converted_body: Vec<u8>,
        url: String,
        upstream_headers: Vec<(String, String)>,
    },
    /// The account cannot serve this request (not signed in, revoked, or
    /// its Copilot session was refused): skip it and try the next one.
    /// `response` is what to send when no other account/provider remains.
    Unavailable { response: Response },
}

pub(crate) async fn convert_and_build_upstream(
    att: &Attempt<'_>,
    ctx: &mut RequestContext,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<AccountStep, Box<Response>> {
    let Attempt {
        state,
        provider,
        api,
        route_info,
    } = att;
    let api = *api;
    // Code Assist always speaks Gemini on the wire: conversion targets Gemini
    // (not `provider.format`) and the body rides inside the request envelope.
    let to_format = api.to_format();

    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "request_original request_id={} client_format={} provider_format={} body={}",
            ctx.req_id, route_info.client_format, to_format, truncated_body(body, 4096)
        );
    }

    let converted_body = match safe_convert_request(body, route_info.client_format, to_format) {
        Ok(mut bytes) => {
            if state.config.logging.conversion_log {
                debug!(
                    target: "conversion",
                    "request_converted request_id={} body={}",
                    ctx.req_id, truncated_body(&bytes, 4096)
                );
            }
            if api == UpstreamApi::CodeAssist {
                // The model id rides in the Code Assist envelope, not in the
                // Gemini body or URL.
                crate::wire::wrap_code_assist(&bytes, &ctx.upstream_model).map_err(|err| {
                    error!(
                        "code assist envelope failed request_id={} error={err}",
                        ctx.req_id
                    );
                    Box::new(conversion_error_response(HandlerError::Envelope(err)))
                })?
            } else {
                if let Err(err) =
                    patch_model_in_body(&mut bytes, api.to_format(), &ctx.upstream_model)
                {
                    error!("failed to patch model: {err} request_id={}", ctx.req_id);
                }
                bytes
            }
        }
        Err(err) => {
            error!(
                "request conversion failed request_id={} from={} to={} error={}",
                ctx.req_id, route_info.client_format, to_format, err
            );
            return Err(Box::new(conversion_error_response(err)));
        }
    };

    // Copilot accounts (copilot:...) resolve the GitHub OAuth token and
    // exchange it for a short-lived Copilot session (magpie copilotToken)
    // before building the request: the session supplies both the API base
    // URL and the auth headers.
    let copilot_session = if provider.is_copilot_account() {
        let account_id = provider.account.clone().unwrap_or_default();
        let account = match state.accounts.resolve(&account_id).await {
            Ok(account) => account,
            Err(err) => {
                warn!(
                    "account resolution failed request_id={} account={} error={err}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("account {account_id}: {err}"),
                    ),
                });
            }
        };
        match state.copilot.session(&account.access_token).await {
            Ok(session) => Some(session),
            Err(CopilotError::Refused(message)) => {
                warn!(
                    "copilot session refused request_id={} account={} error={message}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!(
                            "Copilot account {account_id}: {message}; please re-login (重新登录)"
                        ),
                    ),
                });
            }
            Err(CopilotError::Exchange(message)) => {
                warn!(
                    "copilot session exchange failed request_id={} account={} error={message}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("Copilot account {account_id}: {message}"),
                    ),
                });
            }
        }
    } else {
        None
    };

    let url = if let Some(session) = &copilot_session {
        // The session's `endpoints.api` overrides `provider.base_url`
        // (defaulting to the public Copilot base); the path follows the
        // P6.3 rule of stripping the leading `/v1` (magpie api.path()).
        let base = if session.api_base.is_empty() {
            COPILOT_DEFAULT_API_BASE
        } else {
            session.api_base.as_str()
        };
        let path = api.path(&ctx.upstream_model, ctx.streaming);
        let path = path.strip_prefix("/v1").unwrap_or(&path);
        format!("{}{path}", base.trim_end_matches('/'))
    } else if provider.plugin.is_some() {
        // Plugin transport: the URL is `loader.base_url + api.path(...)`,
        // computed on the plugin branch of `process_request` where the
        // loader options are known. The CodeAssist envelope above still
        // applies — only the URL moves.
        String::new()
    } else {
        match api {
            UpstreamApi::Responses
                if provider
                    .account_ids()
                    .first()
                    .is_some_and(|id| id.starts_with("chatgpt-api:")) =>
            {
                crate::proxy::build_upstream_url(
                    Format::OpenAIResponses,
                    &provider.chatgpt_api_base(),
                    &ctx.upstream_model,
                    ctx.streaming,
                )
            }
            UpstreamApi::CodeAssist => crate::wire::build_url(
                UpstreamApi::CodeAssist,
                // Antigravity's generation host is the daily one; a provider
                // that names its own base keeps it (magpie app.base).
                &provider.code_assist_base(),
                &ctx.upstream_model,
                ctx.streaming,
            ),
            _ => build_upstream_url_for_provider(provider, &ctx.upstream_model, ctx.streaming),
        }
    };

    // Account-based OAuth signing wins over the static API key path: when
    // `provider.account` is set, `provider.api_key` is ignored entirely.
    // Plugin providers sign their own requests (the loader owns the base
    // URL, API key and headers), so the host never builds an Authorization
    // header for them; provider-level extra headers still ride along.
    let auth_headers: Vec<(String, String)> = if let Some(session) = &copilot_session {
        // Copilot wire: Bearer session token plus the Copilot client
        // headers and API version (magpie copilotAPIHeaders).
        copilot_api_headers(&session.token)
    } else if provider.plugin.is_some() {
        provider
            .auth
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    } else if let Some(account_id) = &provider.account {
        let account = match state.accounts.resolve(account_id).await {
            Ok(account) => account,
            Err(err) => {
                warn!(
                    "account resolution failed request_id={} account={} error={err}",
                    ctx.req_id, account_id
                );
                return Ok(AccountStep::Unavailable {
                    response: api_error_response(
                        StatusCode::BAD_GATEWAY,
                        "account_error",
                        format!("account {account_id}: {err}"),
                    ),
                });
            }
        };
        let mut headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", account.access_token),
        )];
        headers.extend(
            provider
                .auth
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        headers
    } else {
        ApiKeySource::for_provider(provider)
            .auth_headers()
            .await
            .unwrap_or_default()
    };
    let upstream_headers = crate::proxy::build_upstream_headers(
        headers,
        &auth_headers,
        &state.config.server.pass_through_headers,
    );

    if state.config.logging.upstream_headers_log {
        info!(
            target: "upstream_headers",
            "upstream request headers request_id={} provider={} {}",
            ctx.req_id,
            provider.name,
            sanitize_headers_for_log(&upstream_headers)
        );
    }

    if let Some(ref mut log_ctx) = ctx.log_ctx {
        log_ctx.provider_name = provider.name.clone();
        log_ctx.upstream_url = url.clone();
        log_ctx.upstream_request_body = converted_body.clone();
    }

    Ok(AccountStep::Ready {
        converted_body,
        url,
        upstream_headers,
    })
}

pub(crate) fn build_non_streaming_response(
    att: &Attempt<'_>,
    ctx: &RequestContext,
    upstream_body: &[u8],
    status: reqwest::StatusCode,
    from_format: Format,
) -> Result<Response, HandlerError> {
    let Attempt {
        state,
        provider,
        route_info,
        ..
    } = att;
    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "response_original request_id={} status={} body={}",
            ctx.req_id, status, truncated_body(upstream_body, 4096)
        );
    }

    let mut converted =
        safe_convert_response(upstream_body, from_format, route_info.client_format)?;

    patch_response_namespaces(&mut converted, &ctx.ns_map);

    if state.config.logging.conversion_log {
        debug!(
            target: "conversion",
            "response_converted request_id={} body={}",
            ctx.req_id, truncated_body(&converted, 4096)
        );
    }

    if let Some(ref logger) = state.usage_logger {
        let (input_tokens, output_tokens) = usage::extract_usage_from_response(upstream_body);
        logger.log(UsageRecord {
            request_id: ctx.req_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            client_format: route_info.client_format.to_string(),
            provider: provider.name.clone(),
            client_model: ctx.client_model.clone(),
            upstream_model: ctx.upstream_model.clone(),
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            latency_ms: ctx.start_time.elapsed().as_millis() as u64,
            status: status.as_u16(),
            streaming: false,
        });
    }

    if let Some(ref logger) = state.request_logger {
        if let Some(ref log_ctx) = ctx.log_ctx {
            crate::request_log::log_non_streaming(
                logger,
                log_ctx,
                upstream_body,
                &converted,
                status,
                from_format,
                &state.config.logging.request_log,
            );
        }
    }

    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(converted))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}
