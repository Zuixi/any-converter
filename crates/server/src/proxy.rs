use std::collections::HashMap;
use std::sync::Arc;

use any_converter_core::convert::{Format, convert_stream_event};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{format_sse_event, is_openai_done, parse_sse_block};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use reqwest::Client;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::ProviderConfig;
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

/// Forward a non-streaming request to the upstream provider.
pub async fn forward_non_streaming(
    client: &Client,
    url: &str,
    body: Vec<u8>,
    auth_headers: &[(String, String)],
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);

    for (key, value) in auth_headers {
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
    Ok((status, bytes))
}

/// Forward a streaming request and convert SSE events to the client format.
#[allow(clippy::too_many_arguments)] // proxy boundary needs upstream request, formats, ns_map, and logging config.
pub async fn forward_streaming(
    client: &Client,
    url: &str,
    body: Vec<u8>,
    auth_headers: &[(String, String)],
    from_format: Format,
    to_format: Format,
    ns_map: &HashMap<String, (String, String)>,
    log_ctx: Option<RequestLogContext>,
    logger: Option<Arc<RequestLogger>>,
    max_capture_bytes: usize,
    trace_enabled: bool,
    trace_max_preview_bytes: usize,
) -> Result<Response, String> {
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);

    for (key, value) in auth_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    info!("sending streaming request to upstream url={url}");
    let upstream = req
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;
    let status = upstream.status();
    info!("upstream streaming response received url={url} status={status}");

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
                        );
                        if event_count <= 5 {
                            let block_preview = if block.len() > 200 {
                                &block[..200]
                            } else {
                                &block
                            };
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
            let preview = if buffer.len() > 200 {
                &buffer[..200]
            } else {
                &buffer
            };
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

    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
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
) -> Vec<String> {
    let Some(event) = parse_sse_block(block) else {
        return Vec::new();
    };

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
            model_map: std::collections::HashMap::new(),
            endpoints: crate::config::ProviderEndpointConfig {
                path: Some("/v1/text/chatcompletion_v2".into()),
                stream_path: Some("/v1/text/chatcompletion_v2?stream=true&model={model}".into()),
            },
            auth: crate::config::ProviderAuthConfig::default(),
        };

        let url = build_upstream_url_for_provider(&provider, "abab6.5s-chat", true);

        assert_eq!(
            url,
            "https://api.minimax.io/v1/text/chatcompletion_v2?stream=true&model=abab6.5s-chat"
        );
    }
}
