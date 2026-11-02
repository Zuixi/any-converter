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
use crate::formats::openai_chat::{ContentPart, MessageContent};

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

pub fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let semi = rest.find(';')?;
    let mime = &rest[..semi];
    let data = rest[semi + 1..].strip_prefix("base64,")?;
    Some((mime.to_string(), data.to_string()))
}

pub fn normalize_id_to_chat(id: &str) -> String {
    if id.is_empty() {
        return new_chat_id();
    }
    if id.starts_with("chatcmpl-") {
        return id.to_string();
    }
    let body = id
        .strip_prefix("msg_")
        .or_else(|| id.strip_prefix("resp_"))
        .unwrap_or(id);
    format!("chatcmpl-{body}")
}

pub fn normalize_id_to_claude(id: &str) -> String {
    if id.is_empty() {
        return new_msg_id();
    }
    if id.starts_with("msg_") {
        return id.to_string();
    }
    let body = id
        .strip_prefix("chatcmpl-")
        .or_else(|| id.strip_prefix("resp_"))
        .unwrap_or(id);
    format!("msg_{body}")
}

pub fn normalize_id_to_resp(id: &str) -> String {
    if id.is_empty() {
        return new_resp_id();
    }
    if id.starts_with("resp_") {
        return id.to_string();
    }
    let body = id
        .strip_prefix("chatcmpl-")
        .or_else(|| id.strip_prefix("msg_"))
        .unwrap_or(id);
    format!("resp_{body}")
}

#[cfg(test)]
mod normalize_id_tests {
    use super::*;

    #[test]
    fn test_normalize_id_to_chat() {
        assert!(normalize_id_to_chat("").starts_with("chatcmpl-"));
        assert_eq!(normalize_id_to_chat("chatcmpl-abc"), "chatcmpl-abc");
        assert_eq!(normalize_id_to_chat("msg_abc"), "chatcmpl-abc");
        assert_eq!(normalize_id_to_chat("resp_abc"), "chatcmpl-abc");
        assert_eq!(normalize_id_to_chat("plain"), "chatcmpl-plain");
    }

    #[test]
    fn test_normalize_id_to_claude() {
        assert!(normalize_id_to_claude("").starts_with("msg_"));
        assert_eq!(normalize_id_to_claude("msg_abc"), "msg_abc");
        assert_eq!(normalize_id_to_claude("chatcmpl-abc"), "msg_abc");
        assert_eq!(normalize_id_to_claude("resp_abc"), "msg_abc");
        assert_eq!(normalize_id_to_claude("plain"), "msg_plain");
    }

    #[test]
    fn test_normalize_id_to_resp() {
        assert!(normalize_id_to_resp("").starts_with("resp_"));
        assert_eq!(normalize_id_to_resp("resp_abc"), "resp_abc");
        assert_eq!(normalize_id_to_resp("chatcmpl-abc"), "resp_abc");
        assert_eq!(normalize_id_to_resp("msg_abc"), "resp_abc");
        assert_eq!(normalize_id_to_resp("plain"), "resp_plain");
    }
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
