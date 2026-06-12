use crate::converters::FormatConverter;
use crate::converters::shared;
use crate::converters::shared::*;
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::gemini::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: GeminiRequest = serde_json::from_slice(input)?;

        let system = req
            .system_instruction
            .as_ref()
            .map(gemini_content_to_system)
            .transpose()?;

        let messages = merge_claude_messages(gemini_contents_to_claude_messages(req.contents)?);

        let tools = req.tools.map(|tool_groups| {
            tool_groups
                .into_iter()
                .flat_map(|g| g.function_declarations)
                .map(|fd| ClaudeTool {
                    name: fd.name,
                    description: fd.description,
                    input_schema: fd
                        .parameters
                        .unwrap_or(serde_json::json!({"type": "object"})),
                })
                .collect()
        });

        let tool_choice = req
            .tool_config
            .as_ref()
            .map(gemini_tool_config_to_claude)
            .transpose()?;

        let generation_config = req.generation_config.as_ref();
        let max_tokens = generation_config
            .and_then(|g| g.max_output_tokens)
            .unwrap_or(DEFAULT_MAX_TOKENS);
        let temperature = generation_config
            .and_then(|g| g.temperature)
            .map(|t| t.clamp(0.0, CLAUDE_TEMPERATURE_MAX));
        let top_p = generation_config.and_then(|g| g.top_p);
        let top_k = generation_config.and_then(|g| g.top_k);
        let stop_sequences = generation_config
            .and_then(|g| g.stop_sequences.clone())
            .filter(|s| !s.is_empty());

        let out = ClaudeRequest {
            model: req.model.unwrap_or_default(),
            max_tokens,
            messages,
            system,
            temperature,
            top_p,
            top_k,
            stop_sequences,
            stream: None,
            tools,
            tool_choice,
            metadata: None,
            thinking: None,
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: GeminiResponse = serde_json::from_slice(input)?;

        let candidate = resp
            .candidates
            .first()
            .ok_or_else(|| ConvertError::MissingField("candidates".into()))?;

        let content = gemini_parts_to_claude_blocks(&candidate.content.parts)?;

        let stop_reason = match candidate.finish_reason.as_deref() {
            Some("MAX_TOKENS") => "max_tokens",
            Some("SAFETY") => "end_turn",
            _ => "end_turn",
        };

        let usage = resp.usage_metadata.unwrap_or(GeminiUsageMetadata {
            prompt_token_count: None,
            candidates_token_count: None,
            total_token_count: None,
        });

        let out = ClaudeResponse {
            id: shared::new_msg_id(),
            r#type: "message".into(),
            role: "assistant".into(),
            model: resp.model_version.unwrap_or_default(),
            content,
            stop_reason: Some(stop_reason.into()),
            stop_sequence: None,
            usage: ClaudeUsage {
                input_tokens: usage.prompt_token_count.unwrap_or(0),
                output_tokens: usage.candidates_token_count.unwrap_or(0),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
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
            crate::formats::gemini::GeminiStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(crate::formats::claude::ClaudeStreamAdapter::emit_sse_event(
                ce, state_out,
            )?);
        }
        Ok(output)
    }
}

fn gemini_content_to_system(content: &GeminiContent) -> Result<serde_json::Value, ConvertError> {
    let text = content
        .parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    Ok(serde_json::Value::String(text))
}

fn gemini_contents_to_claude_messages(
    contents: Vec<GeminiContent>,
) -> Result<Vec<ClaudeMessage>, ConvertError> {
    let mut messages = Vec::new();
    for content in contents {
        let role = match content.role.as_deref() {
            Some("user") => "user",
            Some("model") => "assistant",
            None => "user",
            Some(other) => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        };

        let blocks = gemini_parts_to_claude_value_blocks(&content.parts, role)?;
        if blocks.is_empty() {
            continue;
        }

        let content_value = if role == "assistant" {
            assistant_blocks_to_content(&blocks)
        } else {
            blocks_to_user_content(&blocks)
        };

        messages.push(ClaudeMessage {
            role: role.into(),
            content: content_value,
        });
    }
    Ok(messages)
}

fn gemini_parts_to_claude_value_blocks(
    parts: &[GeminiPart],
    role: &str,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut blocks = Vec::new();
    for part in parts {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                blocks.push(serde_json::json!({"type": "text", "text": text}));
            }
        }
        if let Some(inline) = &part.inline_data {
            blocks.push(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": inline.mime_type,
                    "data": inline.data,
                }
            }));
        }
        if let Some(fc) = &part.function_call {
            if role != "assistant" {
                return Err(ConvertError::InvalidField {
                    field: "function_call".into(),
                    reason: "function_call only allowed in model content".into(),
                });
            }
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": fc.id.clone().unwrap_or_else(shared::new_uuid),
                "name": fc.name,
                "input": fc.args,
            }));
        }
        if let Some(fr) = &part.function_response {
            if role != "user" {
                return Err(ConvertError::InvalidField {
                    field: "function_response".into(),
                    reason: "function_response only allowed in user content".into(),
                });
            }
            blocks.push(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": fr.id.clone().unwrap_or_else(shared::new_uuid),
                "content": extract_function_response_text(&fr.response),
                "is_error": check_function_response_error(&fr.response),
            }));
        }
    }
    Ok(blocks)
}

fn gemini_parts_to_claude_blocks(
    parts: &[GeminiPart],
) -> Result<Vec<ClaudeContentBlock>, ConvertError> {
    let mut blocks = Vec::new();
    for part in parts {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                blocks.push(ClaudeContentBlock::Text { text: text.clone() });
            }
        }
        if let Some(fc) = &part.function_call {
            blocks.push(ClaudeContentBlock::ToolUse {
                id: fc.id.clone().unwrap_or_else(shared::new_uuid),
                name: fc.name.clone(),
                input: fc.args.clone(),
            });
        }
    }
    Ok(blocks)
}

fn check_function_response_error(response: &serde_json::Value) -> bool {
    response.get("error").map(|e| !e.is_null()).unwrap_or(false)
}

fn gemini_tool_config_to_claude(
    value: &serde_json::Value,
) -> Result<serde_json::Value, ConvertError> {
    let config = value
        .get("functionCallingConfig")
        .ok_or_else(|| ConvertError::MissingField("functionCallingConfig".into()))?;
    let mode = config
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("functionCallingConfig.mode".into()))?;

    let allowed = config
        .get("allowedFunctionNames")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if allowed.len() == 1 {
        return Ok(serde_json::json!({"type": "tool", "name": allowed[0]}));
    }

    match mode {
        "AUTO" | "VALIDATED" => Ok(serde_json::json!({"type": "auto"})),
        "NONE" => Ok(serde_json::json!({"type": "none"})),
        "ANY" => Ok(serde_json::json!({"type": "any"})),
        other => Err(ConvertError::InvalidField {
            field: "functionCallingConfig.mode".into(),
            reason: format!("unsupported mode: {other}"),
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
            "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content.as_str(), Some("hello"));
        assert!(req.thinking.is_none());
    }

    #[test]
    fn test_convert_response_simple_assistant_message() {
        let input = serde_json::to_vec(&serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hi"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&input).unwrap();
        let resp: ClaudeResponse = serde_json::from_slice(&result).unwrap();

        assert!(matches!(
            &resp.content[0],
            ClaudeContentBlock::Text { text } if text == "hi"
        ));
    }
}
