//! The plugin transport: a request for a provider a plugin serves leaves
//! through the plugin's own `fetch` hook instead of this machine's HTTP
//! client, so the plugin signs it.

use axum::http::StatusCode;
use axum::response::Response;
use log::error;

use crate::config::PluginRef;
use crate::plugins::{LoadParams, PluginFetchParams, ProviderPlugin};

use super::super::api_error_response;
use super::{Attempt, RequestContext};

/// Outcome of one plugin-transport attempt in the failover loop.
pub(crate) enum PluginOutcome {
    /// The plugin answered; the shared retryable-status rule applies.
    Answered(Response, StatusCode),
    /// The plugin transport failed; move to the next provider.
    Failed { error: String },
}

/// Send the already-converted request through the plugin's `fetch` hook.
///
/// URL construction mirrors magpie `pluginFetch`: the loader's base URL
/// (trailing slash trimmed, falling back to the matched model's own `url`)
/// plus [`UpstreamApi::path`]. A same-host guard keeps the plugin from
/// steering credentialed requests anywhere but its own base host.
#[allow(clippy::too_many_arguments)] // provider-branch boundary: state, config, and converted request parts
pub(crate) async fn forward_via_plugin(
    att: &Attempt<'_>,
    ctx: &mut RequestContext,
    plugin_ref: &PluginRef,
    upstream_headers: Vec<(String, String)>,
    converted_body: Vec<u8>,
) -> PluginOutcome {
    let Attempt {
        state,
        provider,
        api,
        route_info,
    } = att;
    let api = *api;
    let account = provider.account.clone().unwrap_or_default();

    // Lazily start (and reuse) the plugin subprocess; `host_for` restarts
    // a dead process once.
    let host = match state.hosts.host_for(plugin_ref).await {
        Ok(host) => host,
        Err(err) => {
            error!("plugin start failed provider={} error={err}", provider.name);
            return PluginOutcome::Failed {
                error: format!("plugin {}: {err}", provider.name),
            };
        }
    };
    let provider_id = host.info().id.clone();

    // The loader decides where and how the request is sent. The host never
    // injects a local Authorization header for plugin providers — the
    // plugin signs its own requests.
    let loader = match ProviderPlugin::load(
        &*host,
        LoadParams {
            provider: provider_id.clone(),
            account: account.clone(),
            proxy: None,
        },
    )
    .await
    {
        Ok(options) => options,
        Err(err) => {
            error!("plugin load failed provider={} error={err}", provider.name);
            return PluginOutcome::Failed {
                error: format!("plugin {} load: {err}", provider.name),
            };
        }
    };

    // Model list: `npm` travels in the fetch params and identifies the wire
    // the body was shaped as (`resolve_upstream_api` used the same list).
    let models = match state.hosts.models_for(plugin_ref, &account).await {
        Ok(models) => models,
        Err(err) => {
            error!(
                "plugin models failed provider={} error={err}",
                provider.name
            );
            return PluginOutcome::Failed {
                error: format!("plugin {} models: {err}", provider.name),
            };
        }
    };
    let matched = models.iter().find(|m| {
        m.id == ctx.upstream_model || (!m.api_id.is_empty() && m.api_id == ctx.upstream_model)
    });

    // base = loader.base_url (trailing slash trimmed), else the model's own
    // `url`; an empty base means the plugin cannot send requests at all.
    let mut base = loader.base_url.trim_end_matches('/').to_string();
    if base.is_empty() {
        base = matched
            .map(|m| m.url.trim_end_matches('/').to_string())
            .unwrap_or_default();
    }
    if base.is_empty() {
        error!(
            "plugin provider={} has nowhere to send its requests",
            provider.name
        );
        return PluginOutcome::Answered(
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                format!(
                    "plugin says nowhere to send its requests (provider {})",
                    provider.name
                ),
            ),
            StatusCode::BAD_GATEWAY,
        );
    }

    // The AI SDK's base ends where magpie's `/v1` does: magpie sends
    // `base + TrimPrefix(rest, "/v1")` (plugins.go pluginFetch), so a
    // loader base of https://api.githubcopilot.com asks for
    // /chat/completions, and one of https://api.anthropic.com/v1 must not
    // become /v1/v1/messages.
    let path = api.path(&ctx.upstream_model, ctx.streaming);
    let path = path.strip_prefix("/v1").unwrap_or(&path);
    let url = format!("{base}{path}");
    // Same-host guard (magpie pluginFetch): credentialed requests must stay
    // on the loader's host; relative/hostless bases fail it outright.
    if !crate::proxy::assert_same_host(&base, &url) {
        error!(
            "plugin provider={} tried to send a request outside its base host",
            provider.name
        );
        return PluginOutcome::Answered(
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                "plugin tried to send a request to a different host than its base URL",
            ),
            StatusCode::BAD_GATEWAY,
        );
    }

    if let Some(ref mut log_ctx) = ctx.log_ctx {
        log_ctx.upstream_url = url.clone();
    }

    let params = PluginFetchParams {
        url,
        method: "POST".to_string(),
        headers: upstream_headers,
        body: String::from_utf8_lossy(&converted_body).into_owned(),
        provider: provider_id,
        account,
        model: ctx.upstream_model.clone(),
        npm: matched.map(|m| m.npm.clone()).unwrap_or_default(),
    };

    let from_format = api.to_format();
    let to_format = route_info.client_format;
    if ctx.streaming {
        match crate::proxy::forward_plugin_streaming(
            &*host,
            params,
            from_format,
            to_format,
            api,
            &ctx.ns_map,
        )
        .await
        {
            Ok(pair) => PluginOutcome::Answered(pair.0, pair.1),
            Err(err) => PluginOutcome::Failed { error: err },
        }
    } else {
        match crate::proxy::forward_plugin_non_streaming(
            &*host,
            params,
            from_format,
            to_format,
            api,
            &ctx.ns_map,
        )
        .await
        {
            Ok(pair) => PluginOutcome::Answered(pair.0, pair.1),
            Err(err) => PluginOutcome::Failed { error: err },
        }
    }
}
