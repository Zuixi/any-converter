use any_converter_core::convert::{convert_stream_event, Format};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{is_openai_done, parse_sse_block};
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use reqwest::Client;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

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

    info!(url = url, "sending non-streaming request to upstream");
    let response = req
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;
    let status = response.status();
    info!(url = url, status = %status, "upstream response received");
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("failed to read upstream response: {e}"))?
        .to_vec();
    Ok((status, bytes))
}

/// Forward a streaming request and convert SSE events to the client format.
pub async fn forward_streaming(
    client: &Client,
    url: &str,
    body: Vec<u8>,
    auth_headers: &[(String, String)],
    from_format: Format,
    to_format: Format,
    ns_map: &std::collections::HashMap<String, (String, String)>,
) -> Result<Response, String> {
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);

    for (key, value) in auth_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    info!(url = url, "sending streaming request to upstream");
    let upstream = req
        .send()
        .await
        .map_err(|e| format!("upstream request failed: {e}"))?;
    let status = upstream.status();
    info!(url = url, status = %status, "upstream streaming response received");

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(64);

    let ns_map_owned = ns_map.clone();
    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut state_in = StreamState::default();
        let mut state_out = StreamState::default();
        let mut upstream_stream = upstream.bytes_stream();
        let mut chunk_count: u64 = 0;
        let mut event_count: u64 = 0;
        let mut emitted_count: u64 = 0;

        while let Some(chunk_result) = upstream_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    chunk_count += 1;
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    if chunk_count <= 3 {
                        debug!(chunk_count, chunk_len = chunk_str.len(), "upstream chunk");
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
                            let block_preview = if block.len() > 200 { &block[..200] } else { &block };
                            info!(event_count, converted_count = lines.len(), block_preview, "SSE block converted");
                        }
                        for line in lines {
                            let line = patch_sse_namespaces(&line, &ns_map_owned);
                            emitted_count += 1;
                            if tx.send(Ok(line)).await.is_err() {
                                warn!(emitted_count, "client disconnected during streaming");
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
            let preview = if buffer.len() > 200 { &buffer[..200] } else { &buffer };
            info!(remaining_buffer_len = buffer.len(), preview, "processing remaining buffer");
            for line in convert_sse_block(
                buffer.trim(),
                from_format,
                to_format,
                &mut state_in,
                &mut state_out,
            ) {
                let line = patch_sse_namespaces(&line, &ns_map_owned);
                let _ = tx.send(Ok(line)).await;
                emitted_count += 1;
            }
        }
        info!(chunk_count, event_count, emitted_count, "streaming completed");
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
        if line.starts_with("data: ") { Some(0) } else { None }
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
            Vec::new()
        }
        Err(_) => {
            error!("stream conversion panicked (likely unimplemented adapter)");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
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
}
