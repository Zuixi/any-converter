use crate::converters::FormatConverter;
use crate::converters::shared::{parse_content_value, tool_result_text};
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::gemini::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: ClaudeRequest = serde_json::from_slice(input)?;

        let system_instruction = req.system.map(system_to_gemini_content).transpose()?;
        let contents = claude_messages_to_gemini(req.messages)?;

        let tools = req.tools.map(|tools| {
            vec![GeminiToolDeclaration {
                function_declarations: tools
                    .into_iter()
                    .map(|t| GeminiFunctionDeclaration {
                        name: t.name,
                        description: t.description,
                        parameters: Some(t.input_schema),
                    })
                    .collect(),
            }]
        });

        let tool_config = req
            .tool_choice
            .as_ref()
            .map(claude_tool_choice_to_gemini)
            .transpose()?;

        let has_stop_sequences = req.stop_sequences.as_ref().is_some_and(|s| !s.is_empty());

        let generation_config = GeminiGenerationConfig {
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: req.top_k,
            max_output_tokens: Some(req.max_tokens),
            stop_sequences: req.stop_sequences,
            seed: None,
            response_mime_type: None,
            response_schema: None,
        };

        let has_gen_config = req.temperature.is_some()
            || req.top_p.is_some()
            || req.top_k.is_some()
            || req.max_tokens > 0
            || has_stop_sequences;

        let out = GeminiRequest {
            model: Some(req.model),
            contents,
            system_instruction,
            generation_config: if has_gen_config {
                Some(generation_config)
            } else {
                None
            },
            tools,
            tool_config,
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: ClaudeResponse = serde_json::from_slice(input)?;

        let parts = claude_content_to_gemini_parts(&resp.content)?;
        let finish_reason = match resp.stop_reason.as_deref() {
            Some("max_tokens") => "MAX_TOKENS",
            _ => "STOP",
        };

        let total_tokens = resp.usage.input_tokens + resp.usage.output_tokens;

        let out = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".into()),
                    parts,
                },
                finish_reason: Some(finish_reason.into()),
                index: Some(0),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(resp.usage.input_tokens),
                candidates_token_count: Some(resp.usage.output_tokens),
                total_token_count: Some(total_tokens),
            }),
            model_version: Some(resp.model),
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
            output.extend(crate::formats::gemini::GeminiStreamAdapter::emit_sse_event(
                ce, state_out,
            )?);
        }
        Ok(output)
    }
}

fn system_to_gemini_content(value: serde_json::Value) -> Result<GeminiContent, ConvertError> {
    let text = match value {
        serde_json::Value::String(text) => text,
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => {
            return Err(ConvertError::InvalidField {
                field: "system".into(),
                reason: "expected string or array".into(),
            });
        }
    };

    Ok(GeminiContent {
        role: None,
        parts: vec![GeminiPart::text(text)],
    })
}

fn claude_messages_to_gemini(
    messages: Vec<ClaudeMessage>,
) -> Result<Vec<GeminiContent>, ConvertError> {
    messages
        .into_iter()
        .map(|msg| match msg.role.as_str() {
            "user" => Ok(GeminiContent {
                role: Some("user".into()),
                parts: claude_content_to_gemini_parts_value(&msg.content)?,
            }),
            "assistant" => Ok(GeminiContent {
                role: Some("model".into()),
                parts: claude_content_to_gemini_parts_value(&msg.content)?,
            }),
            other => Err(ConvertError::InvalidField {
                field: "role".into(),
                reason: format!("unsupported role: {other}"),
            }),
        })
        .collect()
}

fn claude_content_to_gemini_parts(
    content: &[ClaudeContentBlock],
) -> Result<Vec<GeminiPart>, ConvertError> {
    let mut parts = Vec::new();
    for block in content {
        match block {
            ClaudeContentBlock::Text { text } => {
                parts.push(GeminiPart::text(text.clone()));
            }
            ClaudeContentBlock::ToolUse { id, name, input } => {
                parts.push(GeminiPart::function_call_with_id(
                    name.clone(),
                    input.clone(),
                    id.clone(),
                ));
            }
            ClaudeContentBlock::Thinking { thinking, .. } => {
                parts.push(GeminiPart::text(thinking.clone()));
            }
        }
    }
    Ok(parts)
}

fn claude_content_to_gemini_parts_value(
    content: &serde_json::Value,
) -> Result<Vec<GeminiPart>, ConvertError> {
    let blocks = parse_content_value(content)?;
    let mut parts = Vec::new();

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
                let text = obj
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                parts.push(GeminiPart::text(text));
            }
            "image" => {
                let source = obj
                    .get("source")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| ConvertError::MissingField("image.source".into()))?;
                let source_type = source
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("image.source.type".into()))?;

                if source_type == "base64" {
                    let mime_type = source
                        .get("media_type")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ConvertError::MissingField("image.source.media_type".into())
                        })?
                        .to_string();
                    let data = source
                        .get("data")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ConvertError::MissingField("image.source.data".into()))?
                        .to_string();
                    parts.push(GeminiPart::inline_data(mime_type, data));
                } else {
                    return Err(ConvertError::UnsupportedContentType(format!(
                        "image source: {source_type}"
                    )));
                }
            }
            "tool_use" => {
                let id = obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_use.id".into()))?
                    .to_string();
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_use.name".into()))?
                    .to_string();
                let input = obj.get("input").cloned().unwrap_or(serde_json::json!({}));
                parts.push(GeminiPart::function_call_with_id(name, input, id));
            }
            "tool_result" => {
                let tool_use_id = obj
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("tool_result.tool_use_id".into()))?
                    .to_string();
                let text = tool_result_text(obj.get("content"))?;
                parts.push(GeminiPart::function_response_with_id(
                    "function",
                    serde_json::json!({ "result": text }),
                    tool_use_id,
                ));
            }
            "thinking" => {
                if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str()) {
                    parts.push(GeminiPart::text(thinking.to_string()));
                }
            }
            other => {
                return Err(ConvertError::UnsupportedContentType(other.to_string()));
            }
        }
    }

    Ok(parts)
}

fn claude_tool_choice_to_gemini(
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
        "auto" => Ok(serde_json::json!({
            "functionCallingConfig": { "mode": "AUTO" }
        })),
        "none" => Ok(serde_json::json!({
            "functionCallingConfig": { "mode": "NONE" }
        })),
        "any" => Ok(serde_json::json!({
            "functionCallingConfig": { "mode": "ANY" }
        })),
        "tool" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("tool_choice.name".into()))?;
            Ok(serde_json::json!({
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": [name]
                }
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
    use crate::converters::FormatConverter;

    #[test]
    fn test_convert_request_simple_user_message() {
        let input = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: GeminiRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.contents.len(), 1);
        assert_eq!(req.contents[0].role.as_deref(), Some("user"));
        assert_eq!(req.contents[0].parts[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn test_convert_response_simple_assistant_message() {
        let input = serde_json::to_vec(&serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&input).unwrap();
        let resp: GeminiResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(
            resp.candidates[0].content.parts[0].text.as_deref(),
            Some("hi")
        );
    }
}
