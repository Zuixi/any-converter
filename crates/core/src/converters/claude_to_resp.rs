use crate::converters::FormatConverter;
use crate::converters::reasoning;
use crate::converters::shared::{
    image_block_to_url, now_unix_secs, parse_content_value, tool_result_text, *,
};
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::openai_resp::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: ClaudeRequest = serde_json::from_slice(input)?;

        let instructions = req.system.map(system_to_instructions).transpose()?;
        let input_items = claude_messages_to_input(req.messages)?;

        let tools = req.tools.map(|tools| {
            tools
                .into_iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                        "strict": true,
                    })
                })
                .collect()
        });

        let tool_choice = req
            .tool_choice
            .as_ref()
            .map(claude_tool_choice_to_resp)
            .transpose()?;

        let out = OpenAIResponsesRequest {
            model: req.model,
            input: Some(serde_json::Value::Array(input_items)),
            instructions,
            max_output_tokens: Some(req.max_tokens),
            temperature: req.temperature,
            top_p: req.top_p,
            stream: req.stream,
            tools,
            tool_choice,
            text: None,
            reasoning: req
                .thinking
                .as_ref()
                .and_then(reasoning::thinking_to_reasoning_json),
            previous_response_id: None,
            store: None,
            extra: Default::default(),
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: ClaudeResponse = serde_json::from_slice(input)?;

        let output = claude_content_to_output(&resp.content)?;
        let status = match resp.stop_reason.as_deref() {
            Some("max_tokens") => "incomplete",
            _ => "completed",
        };

        let id = normalize_id_to_resp(&resp.id);

        let total_tokens = resp.usage.input_tokens + resp.usage.output_tokens;

        let out = OpenAIResponsesResponse {
            id,
            object: "response".into(),
            created_at: now_unix_secs(),
            model: resp.model,
            status: status.to_string(),
            output,
            usage: Some(ResponsesUsage {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                total_tokens: Some(total_tokens),
                input_tokens_details: None,
            }),
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_stream_event(
        &self,
        event: &SseEvent,
        state_in: &mut StreamState,
        state_out: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        use crate::formats::StreamAdapter;
        let canonical =
            crate::formats::claude::ClaudeStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(
                crate::formats::openai_resp::OpenAIResponsesStreamAdapter::emit_sse_event(
                    ce, state_out,
                )?,
            );
        }
        Ok(output)
    }
}

fn system_to_instructions(value: serde_json::Value) -> Result<String, ConvertError> {
    match value {
        serde_json::Value::String(text) => Ok(text),
        serde_json::Value::Array(arr) => Ok(arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n")),
        _ => Err(ConvertError::InvalidField {
            field: "system".into(),
            reason: "expected string or array".into(),
        }),
    }
}

fn claude_messages_to_input(
    messages: Vec<ClaudeMessage>,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut items = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => items.extend(claude_user_to_input(&msg.content)?),
            "assistant" => items.extend(claude_assistant_to_input(&msg.content)?),
            other => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }
    Ok(items)
}

fn claude_user_to_input(
    content: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let blocks = parse_content_value(content)?;
    let mut items = Vec::new();
    let mut message_parts = Vec::new();

    for block in blocks {
        let obj = block
            .as_object()
            .ok_or_else(|| ConvertError::InvalidField {
                field: "content".into(),
                reason: "expected object block".into(),
            })?;
        let block_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match block_type {
            "tool_result" => {
                let call_id = obj
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_result.tool_use_id".into()))?;
                let output = tool_result_text(obj.get("content"))?;
                items.push(serde_json::json!({
                    "type": ITEM_TYPE_FUNCTION_CALL_OUTPUT,
                    "call_id": call_id,
                    "output": output,
                }));
            }
            "text" => {
                let text = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                message_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_INPUT_TEXT,
                    "text": text,
                }));
            }
            "image" => {
                let url = image_block_to_url(obj)?;
                message_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_INPUT_IMAGE,
                    "image_url": url,
                }));
            }
            other => {
                return Err(ConvertError::UnsupportedContentType(other.to_string()));
            }
        }
    }

    if !message_parts.is_empty() {
        items.insert(
            0,
            serde_json::json!({
                "type": ITEM_TYPE_MESSAGE,
                "role": "user",
                "content": message_parts,
            }),
        );
    }

    Ok(items)
}

fn claude_assistant_to_input(
    content: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let blocks = parse_content_value(content)?;
    let mut items = Vec::new();
    let mut message_parts = Vec::new();

    for block in blocks {
        let obj = block
            .as_object()
            .ok_or_else(|| ConvertError::InvalidField {
                field: "content".into(),
                reason: "expected object block".into(),
            })?;
        let block_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match block_type {
            "text" => {
                let text = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                message_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                }));
            }
            "tool_use" => {
                let id = obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_use.id".into()))?;
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_use.name".into()))?;
                let input = obj.get("input").cloned().unwrap_or(serde_json::json!({}));
                items.push(serde_json::json!({
                    "type": ITEM_TYPE_FUNCTION_CALL,
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(&input).unwrap_or_else(|_| "{}".into()),
                }));
            }
            "thinking" => {}
            other => {
                return Err(ConvertError::UnsupportedContentType(other.to_string()));
            }
        }
    }

    if !message_parts.is_empty() {
        items.insert(
            0,
            serde_json::json!({
                "type": ITEM_TYPE_MESSAGE,
                "role": "assistant",
                "content": message_parts,
            }),
        );
    }

    Ok(items)
}

fn claude_content_to_output(
    content: &[ClaudeContentBlock],
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut output = Vec::new();
    let mut text_parts = Vec::new();

    for block in content {
        match block {
            ClaudeContentBlock::Text { text } => {
                text_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                }));
            }
            ClaudeContentBlock::ToolUse { id, name, input } => {
                if !text_parts.is_empty() {
                    output.push(serde_json::json!({
                        "type": ITEM_TYPE_MESSAGE,
                        "role": "assistant",
                        "content": std::mem::take(&mut text_parts),
                    }));
                }
                output.push(serde_json::json!({
                    "type": ITEM_TYPE_FUNCTION_CALL,
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                }));
            }
            ClaudeContentBlock::Thinking {
                thinking,
                signature,
            } => {
                output.push(serde_json::json!({
                    "type": "reasoning",
                    "id": format!("rs_{}", crate::converters::shared::new_uuid()),
                    "summary": [{"type": "summary_text", "text": thinking}],
                }));
                let _ = signature;
            }
        }
    }

    if !text_parts.is_empty() {
        output.push(serde_json::json!({
            "type": ITEM_TYPE_MESSAGE,
            "role": "assistant",
            "content": text_parts,
        }));
    }

    Ok(output)
}

fn claude_tool_choice_to_resp(
    value: &serde_json::Value,
) -> Result<serde_json::Value, ConvertError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ConvertError::InvalidField {
            field: "tool_choice".into(),
            reason: "expected object".into(),
        })?;
    let choice_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool_choice.type".into()))?;

    match choice_type {
        "auto" => Ok(serde_json::json!("auto")),
        "none" => Ok(serde_json::json!("none")),
        "any" => Ok(serde_json::json!("required")),
        "tool" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("tool_choice.name".into()))?;
            Ok(serde_json::json!({
                "type": "function",
                "name": name,
            }))
        }
        other => Err(ConvertError::InvalidField {
            field: "tool_choice.type".into(),
            reason: format!("unsupported type: {other}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_request_system_array_joined_with_blank_lines() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "system": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ],
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let resp_req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp_req.instructions.as_deref(), Some("first\n\nsecond"));
    }

    #[test]
    fn test_convert_response_id_normalizes_to_resp() {
        let resp_bytes = serde_json::to_vec(&serde_json::json!({
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&resp_bytes).unwrap();
        let resp: OpenAIResponsesResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.id, "resp_abc");
    }

    #[test]
    fn test_thinking_enabled_high_maps_to_reasoning_high() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": "hello"}],
            "thinking": {"type": "enabled", "budget_tokens": 8192}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let resp_req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        assert!(resp_req.reasoning.is_some());
        let effort = resp_req
            .reasoning
            .unwrap()
            .get("effort")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        assert_eq!(effort, "high");
    }

    #[test]
    fn test_thinking_enabled_low_maps_to_reasoning_low() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hello"}],
            "thinking": {"type": "enabled", "budget_tokens": 512}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let resp_req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        let effort = resp_req
            .reasoning
            .unwrap()
            .get("effort")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        assert_eq!(effort, "low");
    }

    #[test]
    fn test_no_thinking_maps_to_no_reasoning() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let resp_req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        assert!(resp_req.reasoning.is_none());
    }

    #[test]
    fn test_thinking_response_mapped_to_reasoning_output() {
        let resp_bytes = serde_json::to_vec(&serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "thinking", "thinking": "deep thought", "signature": "sig"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 20}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&resp_bytes).unwrap();
        let resp: OpenAIResponsesResponse = serde_json::from_slice(&result).unwrap();

        let has_reasoning = resp
            .output
            .iter()
            .any(|item| item.get("type").and_then(|v| v.as_str()) == Some("reasoning"));
        assert!(has_reasoning);
    }
}
