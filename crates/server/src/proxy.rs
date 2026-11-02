use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::{Format, convert_stream_event};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{format_sse_event, is_openai_done, parse_sse_block};
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use reqwest::Client;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::{PassThroughHeadersConfig, ProviderConfig, UpstreamApi, glob_match};
use crate::observability::utf8_prefix;
use crate::plugins::{PluginFetchParams, ProviderPlugin};
use crate::request_log::{RequestLogContext, RequestLogger, log_streaming};

/// Build the upstream URL for a provider request.
pub fn build_upstream_url(format: Format, base_url: &str, model: &str, streaming: bool) -> String {
    let base = base_url.trim_end_matches('/');
    match format {
        Format::OpenAIChat => format!("{base}/v1/chat/completions"),
        Format::Claude => format!("{base}/v1/messages"),
        Format::OpenAIResponses => format!("{base}/v1/responses"),
        Format::Gemini if streaming => {
            format!("{base}/v1beta/models/{model}:streamGenerateContent")
        }
        Format::Gemini => format!("{base}/v1beta/models/{model}:generateContent"),
    }
}

/// Build the upstream URL using provider-specific endpoint overrides when configured.
pub fn build_upstream_url_for_provider(
    provider: &ProviderConfig,
    model: &str,
    streaming: bool,
) -> String {
    let custom_path = if streaming {
        provider
            .endpoints
            .stream_path
            .as_deref()
            .or(provider.endpoints.path.as_deref())
    } else {
        provider.endpoints.path.as_deref()
    };

    if let Some(path) = custom_path {
        return join_base_and_path(&provider.base_url, &path.replace("{model}", model));
    }

    build_upstream_url(provider.format, &provider.base_url, model, streaming)
}

fn join_base_and_path(base_url: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    let base = base_url.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Whether `url` stays on the same host (and port) as `base`.
///
/// The plugin transport signs requests with the account's credentials, so
/// the final URL must never drift to another host (magpie `pluginFetch`
/// `HostOf(rest) != HostOf(base)` check). A relative/hostless `base` is
/// rejected too: credentialed requests need an absolute URL whose host the
/// gateway can verify.
pub fn assert_same_host(base: &str, url: &str) -> bool {
    let (Ok(base), Ok(url)) = (reqwest::Url::parse(base), reqwest::Url::parse(url)) else {
        return false;
    };
    match (base.host_str(), url.host_str()) {
        (Some(base_host), Some(url_host)) => {
            base_host == url_host && base.port_or_known_default() == url.port_or_known_default()
        }
        _ => false,
    }
}

/// Headers that must never be forwarded to the upstream provider.
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Headers that are managed by the proxy or replaced by provider auth.
const REQUEST_CONTROL_HEADERS: &[&str] =
    &["host", "content-length", "content-type", "authorization"];

/// Build the complete set of upstream request headers.
///
/// Provider auth headers are always included first so they take precedence over
/// any client-provided headers with the same name. Client headers are then
/// filtered through the global passthrough allow/deny configuration.
pub fn build_upstream_headers(
    client_headers: &HeaderMap,
    auth_headers: &[(String, String)],
    config: &PassThroughHeadersConfig,
) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = auth_headers.to_vec();
    let auth_names: std::collections::HashSet<String> = auth_headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();

    for (name, value) in client_headers.iter() {
        let name_lower = name.as_str().to_ascii_lowercase();

        if HOP_BY_HOP_HEADERS.contains(&name_lower.as_str())
            || REQUEST_CONTROL_HEADERS.contains(&name_lower.as_str())
            || auth_names.contains(&name_lower)
        {
            continue;
        }

        let should_forward = match config.mode {
            crate::config::PassThroughMode::Allow => {
                // Default allow: forward unless explicitly denied.
                !config.deny.iter().any(|pat| glob_match(pat, &name_lower))
                    || config.allow.iter().any(|pat| glob_match(pat, &name_lower))
            }
            crate::config::PassThroughMode::Deny => {
                // Deny-by-default: only forward explicitly allowed headers.
                config.allow.iter().any(|pat| glob_match(pat, &name_lower))
                    && !config.deny.iter().any(|pat| glob_match(pat, &name_lower))
            }
        };

        if !should_forward {
            continue;
        }

        if let Ok(value_str) = value.to_str() {
            result.push((name.as_str().to_string(), value_str.to_string()));
        }
    }

    result
}

/// Forward a non-streaming request to the upstream provider.
///
/// On the [`UpstreamApi::CodeAssist`] transport the response envelope is unwrapped
/// (before any format conversion, which happens in the caller) so downstream
/// conversion sees a plain Gemini payload.
pub async fn forward_non_streaming(
    client: &Client,
    url: &str,
    body: Vec<u8>,
    auth_headers: &[(String, String)],
    client_headers: &HeaderMap,
    pass_through_config: &PassThroughHeadersConfig,
    api: UpstreamApi,
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let upstream_headers =
        build_upstream_headers(client_headers, auth_headers, pass_through_config);

    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);

    for (key, value) in upstream_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    info!("sending non-streaming request to upstream url={url}");
    let response = req
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;
    let status = response.status();
    info!("upstream response received url={url} status={status}");
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("failed to read upstream response: {e}"))?
        .to_vec();
    // Code Assist wraps every payload in a `{"response": ...}` envelope;
    // unwrap before the caller converts. Anything without the envelope
    // passes through byte-for-byte.
    let bytes = if api == UpstreamApi::CodeAssist {
        crate::wire::unwrap_code_assist(&bytes).unwrap_or(bytes)
    } else {
        bytes
    };
    Ok((status, bytes))
}

/// Convert a response body with the panic isolation the server domain
/// requires (`core` conversion may panic on unimplemented paths; the error
/// must become a gateway error, never a crash).
fn convert_response_guarded(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        any_converter_core::convert::convert_response(input, from, to)
    })) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(format!("response conversion failed: {err}")),
        Err(_) => Err("response conversion panicked".to_string()),
    }
}

/// Forward a non-streaming request whose transport is a plugin and convert
/// the plugin's answer to the client format.
///
/// The plugin returns the complete body (see [`ProviderPlugin::fetch`]), so
/// this is a plain request/response round trip: status, headers and body all
/// arrive at once and the converted body is returned whole.
pub async fn forward_plugin_non_streaming(
    plugin: &dyn ProviderPlugin,
    params: PluginFetchParams,
    from_format: Format,
    to_format: Format,
    api: UpstreamApi,
    ns_map: &HashMap<String, (String, String)>,
) -> Result<(Response, StatusCode), String> {
    let upstream = plugin
        .fetch(params)
        .await
        .map_err(|e| format!("plugin fetch failed: {e}"))?;
    let status = StatusCode::from_u16(upstream.status)
        .map_err(|e| format!("plugin returned invalid status {}: {e}", upstream.status))?;

    let mut bytes = upstream.body.into_bytes();
    // Code Assist wraps every payload in a `{"response": ...}` envelope;
    // unwrap before conversion, exactly like the http transport does.
    if api == UpstreamApi::CodeAssist {
        bytes = crate::wire::unwrap_code_assist(&bytes).unwrap_or(bytes);
    }
    let mut converted = convert_response_guarded(&bytes, from_format, to_format)?;
    crate::namespace::patch_response_namespaces(&mut converted, ns_map);

    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(converted))
        .map_err(|e| format!("failed to build response: {e}"))?;
    Ok((response, status))
}

/// Forward a request whose transport is a plugin and replay the plugin's
/// complete answer as SSE blocks.
///
/// The plugin returns the WHOLE body — the plugin transport has no
/// incremental streaming — so this function buffers everything, converts
/// each SSE block, and returns one fully-assembled SSE payload in a single
/// [`Body::from`]. This is NOT incremental streaming (documented
/// limitation): the client sees valid SSE, but nothing is emitted before
/// the plugin finishes.
pub async fn forward_plugin_streaming(
    plugin: &dyn ProviderPlugin,
    params: PluginFetchParams,
    from_format: Format,
    to_format: Format,
    api: UpstreamApi,
    ns_map: &HashMap<String, (String, String)>,
) -> Result<(Response, StatusCode), String> {
    let upstream = plugin
        .fetch(params)
        .await
        .map_err(|e| format!("plugin fetch failed: {e}"))?;
    let status = StatusCode::from_u16(upstream.status)
        .map_err(|e| format!("plugin returned invalid status {}: {e}", upstream.status))?;

    // A non-success answer is returned whole (like `forward_streaming`) so
    // the caller's failover loop can retry the next provider.
    if !status.is_success() {
        let content_type = upstream
            .headers
            .get("content-type")
            .cloned()
            .unwrap_or_else(|| "application/json".to_string());
        let response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(upstream.body))
            .map_err(|e| format!("failed to build response: {e}"))?;
        return Ok((response, status));
    }

    // Split the buffered body into SSE blocks (the same parser the
    // incremental path uses), convert each one, and reassemble one SSE
    // payload.
    let mut buffer = upstream.body;
    let mut state_in = StreamState::default();
    let mut state_out = StreamState::default();
    let mut out = String::new();
    for block in take_complete_sse_blocks(&mut buffer) {
        for line in convert_sse_block(
            &block,
            from_format,
            to_format,
            &mut state_in,
            &mut state_out,
            api,
        ) {
            out.push_str(&patch_sse_namespaces(&line, ns_map));
        }
    }
    if !buffer.trim().is_empty() {
        for line in convert_sse_block(
            buffer.trim(),
            from_format,
            to_format,
            &mut state_in,
            &mut state_out,
            api,
        ) {
            out.push_str(&patch_sse_namespaces(&line, ns_map));
        }
    }

    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(out))
        .map_err(|e| format!("failed to build response: {e}"))?;
    Ok((response, status))
}

/// Forward a streaming request and convert SSE events to the client format.
#[allow(clippy::too_many_arguments)] // proxy boundary needs upstream request, formats, ns_map, and logging config.
pub async fn forward_streaming(
    client: &Client,
    url: &str,
    body: Vec<u8>,
    auth_headers: &[(String, String)],
    client_headers: &HeaderMap,
    pass_through_config: &PassThroughHeadersConfig,
    from_format: Format,
    to_format: Format,
    api: UpstreamApi,
    ns_map: &HashMap<String, (String, String)>,
    log_ctx: Option<RequestLogContext>,
    logger: Option<Arc<RequestLogger>>,
    max_capture_bytes: usize,
    trace_enabled: bool,
    trace_max_preview_bytes: usize,
) -> Result<(Response, StatusCode), String> {
    let upstream_headers =
        build_upstream_headers(client_headers, auth_headers, pass_through_config);

    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);

    for (key, value) in upstream_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    info!("sending streaming request to upstream url={url}");
    let upstream = req
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;
    let status = upstream.status();
    info!("upstream streaming response received url={url} status={status}");

    // a non-success upstream answer is returned whole so the caller can
    // retry the next provider (the retry rule lives in process_request,
    // as for non-streaming)
    if !status.is_success() {
        let content_type = upstream
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        let body = upstream
            .bytes()
            .await
            .map_err(|e| format!("error body read failed: {e}"))?;
        let response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        return Ok((response, status));
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(64);

    let ns_map_owned = ns_map.clone();
    let should_log = log_ctx.is_some() && logger.is_some();
    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut state_in = StreamState::default();
        let mut state_out = StreamState::default();
        let mut upstream_stream = upstream.bytes_stream();
        let mut chunk_count: u64 = 0;
        let mut event_count: u64 = 0;
        let mut emitted_count: u64 = 0;
        let mut sse_lines: Vec<String> = Vec::new();
        let mut captured_bytes: usize = 0;
        let mut response_truncated = false;
        let mut time_to_first_byte_ms: Option<u64> = None;

        while let Some(chunk_result) = upstream_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    if time_to_first_byte_ms.is_none() {
                        if let Some(ref ctx) = log_ctx {
                            time_to_first_byte_ms =
                                Some(ctx.start_time.elapsed().as_millis() as u64);
                        }
                    }
                    chunk_count += 1;
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    if chunk_count <= 3 {
                        debug!(
                            "upstream chunk chunk_count={chunk_count} chunk_len={}",
                            chunk_str.len()
                        );
                    }
                    buffer.push_str(&chunk_str);
                    for block in take_complete_sse_blocks(&mut buffer) {
                        event_count += 1;
                        let lines = convert_sse_block(
                            &block,
                            from_format,
                            to_format,
                            &mut state_in,
                            &mut state_out,
                            api,
                        );
                        if event_count <= 5 {
                            let block_preview = utf8_prefix(&block, 200);
                            info!(
                                "SSE block converted event_count={event_count} converted_count={} block_preview={block_preview}",
                                lines.len()
                            );
                        }
                        for line in lines {
                            let line = patch_sse_namespaces(&line, &ns_map_owned);
                            if should_log && !response_truncated {
                                let line_bytes = line.len();
                                if captured_bytes + line_bytes <= max_capture_bytes {
                                    sse_lines.push(line.clone());
                                    captured_bytes += line_bytes;
                                } else {
                                    response_truncated = true;
                                }
                            }
                            emitted_count += 1;
                            if tx.send(Ok(line)).await.is_err() {
                                warn!(
                                    "client disconnected during streaming emitted_count={emitted_count}"
                                );
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("stream chunk error: {e}");
                }
            }
        }

        if !buffer.trim().is_empty() {
            let preview = utf8_prefix(&buffer, 200);
            info!(
                "processing remaining buffer remaining_buffer_len={} preview={preview}",
                buffer.len()
            );
            for line in convert_sse_block(
                buffer.trim(),
                from_format,
                to_format,
                &mut state_in,
                &mut state_out,
                api,
            ) {
                let line = patch_sse_namespaces(&line, &ns_map_owned);
                if should_log && !response_truncated {
                    let line_bytes = line.len();
                    if captured_bytes + line_bytes <= max_capture_bytes {
                        sse_lines.push(line.clone());
                        captured_bytes += line_bytes;
                    } else {
                        response_truncated = true;
                    }
                }
                let _ = tx.send(Ok(line)).await;
                emitted_count += 1;
            }
        }
        info!(
            "streaming completed chunk_count={chunk_count} event_count={event_count} emitted_count={emitted_count}"
        );

        if let (Some(ctx), Some(logger)) = (log_ctx, logger) {
            let out_usage = state_out.accumulated_usage.clone().unwrap_or_default();
            let in_usage = state_in.accumulated_usage.clone().unwrap_or_default();
            let usage = if out_usage.input_tokens > 0
                || out_usage.output_tokens > 0
                || out_usage.cache_read_tokens.is_some()
                || out_usage.cache_write_tokens.is_some()
                || out_usage.reasoning_tokens.is_some()
            {
                out_usage
            } else {
                in_usage
            };
            log_streaming(
                &logger,
                &ctx,
                sse_lines,
                status,
                from_format,
                time_to_first_byte_ms.unwrap_or(0),
                usage,
                response_truncated,
                max_capture_bytes,
                trace_enabled,
                trace_max_preview_bytes,
            );
        }
    });

    Ok((
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(ReceiverStream::new(rx)))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        status,
    ))
}

/// Patch namespace info in a single SSE line.
/// SSE lines have the format: `event: <type>\ndata: <json>\n\n`
/// Finds `function_call` items by qualified name and splits into namespace + short name.
fn patch_sse_namespaces(
    line: &str,
    ns_map: &std::collections::HashMap<String, (String, String)>,
) -> String {
    if ns_map.is_empty() {
        return line.to_string();
    }
    let Some(data_start) = line.find("\ndata: ").or_else(|| {
        if line.starts_with("data: ") {
            Some(0)
        } else {
            None
        }
    }) else {
        return line.to_string();
    };
    let (prefix, data_part) = if data_start == 0 {
        ("", &line[6..])
    } else {
        (&line[..data_start + 1], &line[data_start + 7..])
    };
    let json_str = data_part.trim_end();
    if json_str.is_empty() || json_str == "[DONE]" {
        return line.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return line.to_string();
    };
    let mut patched = false;

    // Patch function_call items in `output` arrays (response.completed events)
    if let Some(output) = value.get_mut("output").and_then(|v| v.as_array_mut()) {
        patch_function_call_items(output, ns_map, &mut patched);
    }
    // response.completed wraps output inside a `response` object
    if let Some(resp) = value.get_mut("response") {
        if let Some(output) = resp.get_mut("output").and_then(|v| v.as_array_mut()) {
            patch_function_call_items(output, ns_map, &mut patched);
        }
    }
    // Streaming events: response.output_item.added / response.output_item.done
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type == "response.output_item.added" || event_type == "response.output_item.done" {
        if let Some(item) = value.get_mut("item") {
            if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()).map(String::from) {
                    if let Some((ns, short)) = ns_map.get(&name) {
                        item["name"] = serde_json::Value::String(short.clone());
                        item["namespace"] = serde_json::Value::String(ns.clone());
                        patched = true;
                    }
                }
            }
        }
    }

    if patched {
        let new_json = serde_json::to_string(&value).unwrap_or_else(|_| json_str.to_string());
        format!("{prefix}data: {new_json}\n\n")
    } else {
        line.to_string()
    }
}

fn patch_function_call_items(
    items: &mut [serde_json::Value],
    ns_map: &std::collections::HashMap<String, (String, String)>,
    patched: &mut bool,
) {
    for item in items {
        if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
            continue;
        }
        if let Some(name) = item.get("name").and_then(|v| v.as_str()).map(String::from) {
            if let Some((ns, short)) = ns_map.get(&name) {
                item["name"] = serde_json::Value::String(short.clone());
                item["namespace"] = serde_json::Value::String(ns.clone());
                *patched = true;
            }
        }
    }
}

fn take_complete_sse_blocks(buffer: &mut String) -> Vec<String> {
    let mut blocks = Vec::new();
    while let Some(pos) = buffer.find("\n\n") {
        let block = buffer[..pos].trim().to_string();
        *buffer = buffer[pos + 2..].to_string();
        if !block.is_empty() {
            blocks.push(block);
        }
    }
    blocks
}

fn convert_sse_block(
    block: &str,
    from_format: Format,
    to_format: Format,
    state_in: &mut StreamState,
    state_out: &mut StreamState,
    api: UpstreamApi,
) -> Vec<String> {
    let Some(mut event) = parse_sse_block(block) else {
        return Vec::new();
    };

    // Code Assist envelopes every SSE payload in a `{"response": ...}`
    // wrapper. Unwrap the `data:` payload before conversion while keeping the
    // SSE framing (`data: ` prefix and blank-line separator) intact; anything
    // that is not an enveloped JSON payload passes through byte-for-byte.
    if api == UpstreamApi::CodeAssist {
        if let Ok(unwrapped) = crate::wire::unwrap_code_assist(event.data.as_bytes()) {
            event.data = String::from_utf8_lossy(&unwrapped).into_owned();
        }
    }

    if is_openai_done(&event.data) {
        return vec!["data: [DONE]\n\n".to_string()];
    }

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_stream_event(&event, from_format, to_format, state_in, state_out)
    })) {
        Ok(Ok(lines)) => lines,
        Ok(Err(err)) => {
            error!("stream conversion error: {err}");
            vec![format_sse_event(
                "error",
                &serde_json::json!({"error": err.to_string()}).to_string(),
            )]
        }
        Err(_) => {
            error!("stream conversion panicked (likely unimplemented adapter)");
            vec![format_sse_event(
                "error",
                &serde_json::json!({"error": "stream conversion panicked"}).to_string(),
            )]
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const BASE: &str = "https://api.example.com";

    #[test]
    fn test_build_upstream_url_openai_chat() {
        let url = build_upstream_url(Format::OpenAIChat, BASE, "gpt-4", false);
        assert_eq!(url, "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn test_build_upstream_url_claude() {
        let url = build_upstream_url(Format::Claude, BASE, "claude-3", false);
        assert_eq!(url, "https://api.example.com/v1/messages");
    }

    #[test]
    fn test_build_upstream_url_responses() {
        let url = build_upstream_url(Format::OpenAIResponses, BASE, "gpt-4", false);
        assert_eq!(url, "https://api.example.com/v1/responses");
    }

    #[test]
    fn test_build_upstream_url_gemini_non_streaming() {
        let url = build_upstream_url(Format::Gemini, BASE, "gemini-pro", false);
        assert_eq!(
            url,
            "https://api.example.com/v1beta/models/gemini-pro:generateContent"
        );
    }

    #[test]
    fn test_build_upstream_url_gemini_streaming() {
        let url = build_upstream_url(Format::Gemini, BASE, "gemini-pro", true);
        assert_eq!(
            url,
            "https://api.example.com/v1beta/models/gemini-pro:streamGenerateContent"
        );
    }

    #[test]
    fn test_build_upstream_url_trims_trailing_slash() {
        let url = build_upstream_url(Format::OpenAIChat, "https://api.example.com/", "m", false);
        assert_eq!(url, "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn test_build_upstream_url_uses_provider_endpoint_override() {
        let provider = crate::config::ProviderConfig {
            name: "minimax".into(),
            format: Format::OpenAIChat,
            base_url: "https://api.minimax.io".into(),
            api_key: "sk-test".into(),
            account: None,
            api: None,
            wire: None,
            model_map: std::collections::HashMap::new(),
            endpoints: crate::config::ProviderEndpointConfig {
                path: Some("/v1/text/chatcompletion_v2".into()),
                stream_path: Some("/v1/text/chatcompletion_v2?stream=true&model={model}".into()),
            },
            auth: crate::config::ProviderAuthConfig::default(),
            plugin: None,
        };

        let url = build_upstream_url_for_provider(&provider, "abab6.5s-chat", true);

        assert_eq!(
            url,
            "https://api.minimax.io/v1/text/chatcompletion_v2?stream=true&model=abab6.5s-chat"
        );
    }

    fn header_map_from_pairs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                name.parse::<axum::http::HeaderName>().unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn test_build_upstream_headers_passthrough_non_sensitive() {
        let client_headers = header_map_from_pairs(&[
            ("x-requested-with", "codex"),
            ("openai-organization", "org-123"),
            ("authorization", "Bearer client-key"),
            ("connection", "keep-alive"),
            ("host", "localhost:8080"),
            ("content-length", "1234"),
        ]);
        let auth_headers = vec![(
            "Authorization".to_string(),
            "Bearer provider-key".to_string(),
        )];
        let config = PassThroughHeadersConfig::default();

        let headers = build_upstream_headers(&client_headers, &auth_headers, &config);
        let names: Vec<&str> = headers.iter().map(|(n, _)| n.as_str()).collect();

        assert!(names.contains(&"Authorization"));
        assert!(
            headers
                .iter()
                .any(|(n, v)| n == "Authorization" && v == "Bearer provider-key")
        );
        assert!(
            headers
                .iter()
                .any(|(n, v)| n == "x-requested-with" && v == "codex")
        );
        assert!(
            headers
                .iter()
                .any(|(n, v)| n == "openai-organization" && v == "org-123")
        );
        assert!(!names.contains(&"connection"));
        assert!(!names.contains(&"host"));
        assert!(!names.contains(&"content-length"));
        // Client Authorization must be replaced by provider auth.
        assert!(
            !headers
                .iter()
                .any(|(n, v)| n == "authorization" && v == "Bearer client-key")
        );
    }

    #[test]
    fn test_build_upstream_headers_deny_mode_only_allows_listed() {
        let client_headers =
            header_map_from_pairs(&[("x-allowed-foo", "yes"), ("x-dropped-bar", "no")]);
        let auth_headers = vec![("Authorization".to_string(), "Bearer key".to_string())];
        let config = PassThroughHeadersConfig {
            mode: crate::config::PassThroughMode::Deny,
            allow: vec!["x-allowed-*".to_string()],
            deny: vec![],
        };

        let headers = build_upstream_headers(&client_headers, &auth_headers, &config);

        assert!(
            headers
                .iter()
                .any(|(n, v)| n == "x-allowed-foo" && v == "yes")
        );
        assert!(!headers.iter().any(|(n, _v)| n == "x-dropped-bar"));
    }

    #[test]
    fn test_build_upstream_headers_deny_pattern_blocks_allowed() {
        let client_headers = header_map_from_pairs(&[("x-internal-foo", "secret")]);
        let auth_headers = Vec::new();
        let config = PassThroughHeadersConfig {
            mode: crate::config::PassThroughMode::Allow,
            allow: vec![],
            deny: vec!["x-internal-*".to_string()],
        };

        let headers = build_upstream_headers(&client_headers, &auth_headers, &config);

        assert!(!headers.iter().any(|(n, _)| n == "x-internal-foo"));
    }

    #[test]
    fn assert_same_host_accepts_the_same_host() {
        assert!(assert_same_host(
            "https://gw.example.test",
            "https://gw.example.test/v1/messages"
        ));
        // Trailing slash and explicit default port do not change the host.
        assert!(assert_same_host(
            "https://gw.example.test/",
            "https://gw.example.test:443/v1/chat/completions"
        ));
    }

    #[test]
    fn assert_same_host_rejects_cross_host_urls() {
        assert!(!assert_same_host(
            "https://gw.example.test",
            "https://evil.example.test/v1/messages"
        ));
        // Non-default ports are part of the host identity.
        assert!(!assert_same_host(
            "https://gw.example.test",
            "https://gw.example.test:8443/v1/messages"
        ));
    }

    #[test]
    fn assert_same_host_rejects_relative_or_unparsable_bases() {
        // Hostless bases cannot be verified to keep credentialed requests
        // on one host: rejected outright.
        assert!(!assert_same_host(
            "/relative-only",
            "/relative-only/v1/messages"
        ));
        assert!(!assert_same_host("v1/messages", "v1/messages/v1/messages"));
        assert!(!assert_same_host("not a url at all", "not a url at all/x"));
    }
}
