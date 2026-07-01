use crate::converters::FormatConverter;
use crate::converters::reasoning;
use crate::converters::shared::{
    image_block_to_url, normalize_id_to_chat, now_unix_secs, parse_content_value, tool_result_text,
};
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::openai_chat::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: ClaudeRequest = serde_json::from_slice(input)?;
        let mut messages = system_to_messages(req.system)?;
        messages.extend(claude_messages_to_openai(req.messages)?);

        let tools = req.tools.map(|tools| {
            tools
                .into_iter()
                .map(|t| OpenAIChatTool {
                    r#type: "function".into(),
                    function: FunctionDef {
                        name: t.name,
                        description: t.description,
                        parameters: Some(t.input_schema),
                        strict: None,
                    },
                })
                .collect()
        });

        let tool_choice = req
            .tool_choice
            .as_ref()
            .map(claude_tool_choice_to_chat)
            .transpose()?;

        let stop = match &req.stop_sequences {
            None => None,
            Some(vec) if vec.is_empty() => None,
            Some(vec) if vec.len() == 1 => Some(StopValue::Single(vec[0].clone())),
            Some(vec) => Some(StopValue::Multiple(vec.clone())),
        };

        let reasoning_effort = req
            .thinking
            .as_ref()
            .and_then(reasoning::thinking_to_reasoning_effort);

        let out = OpenAIChatRequest {
            model: req.model,
            messages,
            temperature: req.temperature,
            top_p: req.top_p,
            max_completion_tokens: Some(req.max_tokens),
            max_tokens: None,
            stop,
            seed: None,
            stream: req.stream,
            stream_options: None,
            tools,
            tool_choice,
            response_format: None,
            n: None,
            reasoning_effort,
            reasoning: None,
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: ClaudeResponse = serde_json::from_slice(input)?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut reasoning_content = None;

        for block in &resp.content {
            match block {
                ClaudeContentBlock::Text { text } => text_parts.push(text.clone()),
                ClaudeContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        r#type: "function".into(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(input)?,
                        },
                    });
                }
                ClaudeContentBlock::Thinking { thinking, .. } => {
                    reasoning_content = Some(thinking.clone());
                }
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let id = normalize_id_to_chat(&resp.id);

        let finish_reason = match resp.stop_reason.as_deref() {
            Some("end_turn") | Some("stop_sequence") => "stop",
            Some("max_tokens") => "length",
            Some("tool_use") => "tool_calls",
            _ => "stop",
        };

        let total_tokens = resp.usage.input_tokens + resp.usage.output_tokens;

        let out = OpenAIChatResponse {
            id,
            object: "chat.completion".into(),
            created: now_unix_secs(),
            model: resp.model,
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".into(),
                    content,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    refusal: None,
                    reasoning_content,
                },
                finish_reason: Some(finish_reason.into()),
            }],
            usage: Some(OpenAIUsage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens,
                prompt_tokens_details: None,
            }),
            system_fingerprint: None,
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
                crate::formats::openai_chat::OpenAIChatStreamAdapter::emit_sse_event(
                    ce, state_out,
                )?,
            );
        }
        Ok(output)
    }
}

fn system_to_messages(
    system: Option<serde_json::Value>,
) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    match system {
        None => Ok(vec![]),
        Some(serde_json::Value::String(text)) => Ok(vec![OpenAIChatMessage {
            role: "system".into(),
            content: Some(MessageContent::Text(text)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }]),
        Some(serde_json::Value::Array(arr)) => {
            let text = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![OpenAIChatMessage {
                    role: "system".into(),
                    content: Some(MessageContent::Text(text)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                }])
            }
        }
        _ => Err(ConvertError::InvalidField {
            field: "system".into(),
            reason: "expected string or array".into(),
        }),
    }
}

fn claude_messages_to_openai(
    messages: Vec<ClaudeMessage>,
) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    let mut result = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => result.extend(claude_user_to_openai(&msg.content)?),
            "assistant" => result.push(claude_assistant_to_openai(&msg.content)?),
            other => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }
    Ok(result)
}

fn claude_user_to_openai(
    content: &serde_json::Value,
) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    let blocks = parse_content_value(content)?;
    let mut messages = Vec::new();
    let mut non_tool_blocks = Vec::new();

    for block in blocks {
        if let Some(tool_msg) = block_as_tool_message(&block)? {
            messages.push(tool_msg);
        } else {
            non_tool_blocks.push(block);
        }
    }

    if !non_tool_blocks.is_empty() {
        messages.insert(
            0,
            OpenAIChatMessage {
                role: "user".into(),
                content: blocks_to_message_content(&non_tool_blocks)?,
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        );
    }

    Ok(messages)
}

fn claude_assistant_to_openai(
    content: &serde_json::Value,
) -> Result<OpenAIChatMessage, ConvertError> {
    let blocks = parse_content_value(content)?;
    let mut text_blocks = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning_content = None;

    for block in &blocks {
        match block {
            serde_json::Value::Object(obj) => {
                let block_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => text_blocks.push(block.clone()),
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
                        tool_calls.push(ToolCall {
                            id,
                            r#type: "function".into(),
                            function: FunctionCall {
                                name,
                                arguments: serde_json::to_string(&input)?,
                            },
                        });
                    }
                    "thinking" => {
                        if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str()) {
                            reasoning_content = Some(thinking.to_string());
                        }
                    }
                    other => {
                        return Err(ConvertError::UnsupportedContentType(other.to_string()));
                    }
                }
            }
            _ => {
                return Err(ConvertError::InvalidField {
                    field: "content".into(),
                    reason: "expected object block".into(),
                });
            }
        }
    }

    Ok(OpenAIChatMessage {
        role: "assistant".into(),
        content: blocks_to_message_content(&text_blocks)?,
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        reasoning_content,
    })
}

fn block_as_tool_message(
    block: &serde_json::Value,
) -> Result<Option<OpenAIChatMessage>, ConvertError> {
    let obj = block
        .as_object()
        .ok_or_else(|| ConvertError::InvalidField {
            field: "content".into(),
            reason: "expected object".into(),
        })?;
    if obj.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
        return Ok(None);
    }

    let tool_use_id = obj
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool_result.tool_use_id".into()))?
        .to_string();
    let text = tool_result_text(obj.get("content"))?;

    Ok(Some(OpenAIChatMessage {
        role: "tool".into(),
        content: Some(MessageContent::Text(text)),
        name: None,
        tool_calls: None,
        tool_call_id: Some(tool_use_id),
        reasoning_content: None,
    }))
}

fn blocks_to_message_content(
    blocks: &[serde_json::Value],
) -> Result<Option<MessageContent>, ConvertError> {
    if blocks.is_empty() {
        return Ok(None);
    }

    let has_image = blocks
        .iter()
        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"));

    if has_image {
        let mut parts = Vec::new();
        for block in blocks {
            let obj = block
                .as_object()
                .ok_or_else(|| ConvertError::InvalidField {
                    field: "content".into(),
                    reason: "expected object block".into(),
                })?;
            match obj.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    let text = obj
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    parts.push(ContentPart::Text { text });
                }
                Some("image") => {
                    let url = image_block_to_url(obj)?;
                    parts.push(ContentPart::ImageUrl {
                        image_url: ImageUrlDetail { url, detail: None },
                    });
                }
                other => {
                    return Err(ConvertError::UnsupportedContentType(
                        other.unwrap_or("").to_string(),
                    ));
                }
            }
        }
        Ok(Some(MessageContent::Parts(parts)))
    } else {
        let text: String = blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        Ok(if text.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text))
        })
    }
}

fn claude_tool_choice_to_chat(
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
                "function": { "name": name }
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
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert!(matches!(
            &req.messages[0].content,
            Some(MessageContent::Text(text)) if text == "hello"
        ));
    }

    #[test]
    fn test_convert_request_system_array_joined_with_blank_lines() {
        let input = serde_json::to_vec(&serde_json::json!({
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
        let result = converter.convert_request(&input).unwrap();
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        let system_msg = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .expect("system message expected");
        assert_eq!(
            system_msg.content.as_ref().and_then(|c| match c {
                MessageContent::Text(t) => Some(t.as_str()),
                _ => None,
            }),
            Some("first\n\nsecond")
        );
    }

    #[test]
    fn test_convert_response_id_normalizes_to_chat() {
        let input = serde_json::to_vec(&serde_json::json!({
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
        let result = converter.convert_response(&input).unwrap();
        let resp: OpenAIChatResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.id, "chatcmpl-abc");
    }

    #[test]
    fn test_convert_request_thinking_maps_to_reasoning_effort() {
        let input = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": "hello"}],
            "thinking": {"type": "enabled", "budget_tokens": 8192}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.reasoning_effort.as_deref(), Some("high"));
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
        let resp: OpenAIChatResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.choices[0].message.content.as_deref(), Some("hi"));
    }
}
