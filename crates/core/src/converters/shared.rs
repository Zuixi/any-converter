pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn new_chat_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4())
}

pub fn new_resp_id() -> String {
    format!("resp_{}", uuid::Uuid::new_v4())
}

pub fn new_msg_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4())
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Strip top-level fields starting with `_` from a JSON request body.
/// These are internal/private fields from clients (e.g. `_stream_tokens`)
/// that should not be forwarded to upstream providers.
#[allow(dead_code)]
pub fn strip_private_fields(body: &mut serde_json::Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.retain(|key, _| !key.starts_with('_'));
    }
}

/// Remove `x-anthropic-billing-header:` prefix lines from system text.
#[allow(dead_code)]
pub fn clean_system_billing_headers(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("x-anthropic-billing-header:"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Responses API item type constants ──────────────────────────────────

pub const ITEM_TYPE_MESSAGE: &str = "message";
pub const ITEM_TYPE_FUNCTION_CALL: &str = "function_call";
pub const ITEM_TYPE_FUNCTION_CALL_OUTPUT: &str = "function_call_output";
pub const ITEM_TYPE_INPUT_TEXT: &str = "input_text";
pub const ITEM_TYPE_OUTPUT_TEXT: &str = "output_text";
pub const ITEM_TYPE_INPUT_IMAGE: &str = "input_image";

pub const CLAUDE_TEMPERATURE_MAX: f32 = 1.0;

// ── Shared helpers extracted from converter duplicates ─────────────────

use crate::error::ConvertError;
use crate::formats::openai_chat::{ContentPart, MessageContent, StopValue};

pub fn message_content_as_text(content: Option<MessageContent>) -> Option<String> {
    match content? {
        MessageContent::Text(t) => Some(t),
        MessageContent::Parts(parts) => {
            let texts: Vec<String> = parts
                .into_iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join(""))
            }
        }
    }
}

pub fn parse_arguments(args: &str) -> serde_json::Value {
    if args.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(args).unwrap_or(serde_json::json!({}))
    }
}

pub fn stop_to_sequences(stop: &StopValue) -> Vec<String> {
    match stop {
        StopValue::Single(s) => vec![s.clone()],
        StopValue::Multiple(v) => v.clone(),
    }
}

pub fn parse_content_value(
    value: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    match value {
        serde_json::Value::String(text) => {
            Ok(vec![serde_json::json!({"type": "text", "text": text})])
        }
        serde_json::Value::Array(arr) => Ok(arr.clone()),
        _ => Err(ConvertError::InvalidField {
            field: "content".into(),
            reason: "expected string or array".into(),
        }),
    }
}

pub fn tool_result_text(value: Option<&serde_json::Value>) -> Result<String, ConvertError> {
    match value {
        None => Ok(String::new()),
        Some(serde_json::Value::String(text)) => Ok(text.clone()),
        Some(serde_json::Value::Array(arr)) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect();
            Ok(parts.join(""))
        }
        _ => Err(ConvertError::InvalidField {
            field: "tool_result.content".into(),
            reason: "expected string or array".into(),
        }),
    }
}

pub fn image_block_to_url(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, ConvertError> {
    let source = obj
        .get("source")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ConvertError::MissingField("image.source".into()))?;
    let source_type = source
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("image.source.type".into()))?;

    match source_type {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.media_type".into()))?;
            let data = source
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.data".into()))?;
            Ok(format!("data:{media_type};base64,{data}"))
        }
        "url" => {
            let url = source
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.url".into()))?;
            Ok(url.to_string())
        }
        other => Err(ConvertError::UnsupportedContentType(format!(
            "image source: {other}"
        ))),
    }
}

pub fn extract_message_text(item: &serde_json::Value) -> String {
    let content = match item.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|part| {
                let t = part.get("type").and_then(|v| v.as_str())?;
                if t == "output_text" || t == "input_text" || t == "text" {
                    part.get("text").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

pub fn parse_function_call_fields(
    item: &serde_json::Value,
) -> Result<(String, String, serde_json::Value), ConvertError> {
    let call_id = item
        .get("call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("function_call.name".into()))?
        .to_string();
    let arguments = item
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let input = serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));
    Ok((call_id, name, input))
}

pub fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let semi = rest.find(';')?;
    let mime = &rest[..semi];
    let data = rest[semi + 1..].strip_prefix("base64,")?;
    Some((mime.to_string(), data.to_string()))
}

pub fn extract_function_response_text(response: &serde_json::Value) -> String {
    if let Some(text) = response.as_str() {
        return text.to_string();
    }
    if let Some(obj) = response.as_object() {
        if let Some(result) = obj.get("result").or(obj.get("output")) {
            return result.to_string();
        }
    }
    response.to_string()
}

// ── Claude message merge helpers ──────────────────────────────────────

use crate::formats::claude::ClaudeMessage;

pub fn assistant_blocks_to_content(blocks: &[serde_json::Value]) -> serde_json::Value {
    if blocks.len() == 1 {
        let block_type = blocks[0].get("type").and_then(|v| v.as_str());
        if block_type == Some("text") {
            if let Some(text) = blocks[0].get("text").and_then(|v| v.as_str()) {
                return serde_json::Value::String(text.to_string());
            }
        }
    }
    serde_json::Value::Array(blocks.to_vec())
}

pub fn blocks_to_user_content(blocks: &[serde_json::Value]) -> serde_json::Value {
    if blocks.len() == 1 {
        if let Some(text) = blocks[0].get("text").and_then(|v| v.as_str()) {
            if blocks[0].get("type").and_then(|v| v.as_str()) == Some("text") {
                return serde_json::Value::String(text.to_string());
            }
        }
    }
    serde_json::Value::Array(blocks.to_vec())
}

pub fn merge_claude_messages(messages: Vec<ClaudeMessage>) -> Vec<ClaudeMessage> {
    let mut merged: Vec<ClaudeMessage> = Vec::with_capacity(messages.len());
    for msg in messages {
        if let Some(last) = merged.last_mut() {
            if last.role == msg.role {
                let combined = merge_claude_content(&last.content, &msg.content, &last.role);
                last.content = combined;
                continue;
            }
        }
        merged.push(msg);
    }
    merged
}

pub fn merge_claude_content(
    a: &serde_json::Value,
    b: &serde_json::Value,
    role: &str,
) -> serde_json::Value {
    let mut blocks = content_value_to_blocks(a);
    blocks.extend(content_value_to_blocks(b));
    if role == "assistant" {
        assistant_blocks_to_content(&blocks)
    } else if blocks.len() == 1 {
        if let Some(text) = blocks[0].get("text").and_then(|v| v.as_str()) {
            if blocks[0].get("type").and_then(|v| v.as_str()) == Some("text") {
                return serde_json::Value::String(text.to_string());
            }
        }
        serde_json::Value::Array(blocks)
    } else {
        serde_json::Value::Array(blocks)
    }
}

pub fn content_value_to_blocks(value: &serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![serde_json::json!({"type": "text", "text": text})]
            }
        }
        serde_json::Value::Array(arr) => arr.clone(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_private_fields() {
        let mut body = serde_json::json!({
            "model": "gpt-4.1",
            "messages": [],
            "_stream_tokens": true,
            "_internal_id": "abc"
        });
        strip_private_fields(&mut body);
        assert!(body.get("model").is_some());
        assert!(body.get("messages").is_some());
        assert!(body.get("_stream_tokens").is_none());
        assert!(body.get("_internal_id").is_none());
    }

    #[test]
    fn test_strip_private_fields_no_underscore() {
        let mut body = serde_json::json!({"model": "gpt-4.1", "stream": true});
        strip_private_fields(&mut body);
        assert!(body.get("model").is_some());
        assert!(body.get("stream").is_some());
    }

    #[test]
    fn test_clean_system_billing_headers() {
        let input = "You are helpful.\nx-anthropic-billing-header: abc\nBe concise.";
        let result = clean_system_billing_headers(input);
        assert_eq!(result, "You are helpful.\nBe concise.");
    }

    #[test]
    fn test_clean_system_billing_headers_no_header() {
        let input = "You are helpful.\nBe concise.";
        let result = clean_system_billing_headers(input);
        assert_eq!(result, input);
    }
}
