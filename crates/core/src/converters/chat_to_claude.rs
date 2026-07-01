use crate::converters::FormatConverter;
use crate::converters::reasoning;
use crate::converters::shared::*;
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::openai_chat::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: OpenAIChatRequest = serde_json::from_slice(input)?;
        let out = convert_request(req)?;
        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: OpenAIChatResponse = serde_json::from_slice(input)?;
        let out = convert_response(resp)?;
        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_stream_event(
        &self,
        event: &SseEvent,
        state_in: &mut StreamState,
        state_out: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        use crate::formats::StreamAdapter;
        use crate::formats::claude::ClaudeStreamAdapter;
        use crate::formats::openai_chat::OpenAIChatStreamAdapter;

        let canonical = OpenAIChatStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(ClaudeStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIChatRequest) -> Result<ClaudeRequest, ConvertError> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<ClaudeMessage> = Vec::new();

    for msg in req.messages {
        match msg.role.as_str() {
            "system" | "developer" => {
                if let Some(text) = message_content_as_text(msg.content) {
                    system_parts.push(text);
                }
            }
            "user" | "assistant" | "tool" => {
                messages.push(chat_message_to_claude(msg)?);
            }
            other => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else if system_parts.len() == 1 {
        Some(serde_json::Value::String(system_parts[0].clone()))
    } else {
        Some(serde_json::Value::String(system_parts.join("\n")))
    };

    let messages = merge_claude_messages(messages);

    let tools = req.tools.map(|tools| {
        tools
            .into_iter()
            .map(|t| ClaudeTool {
                name: t.function.name,
                description: t.function.description,
                input_schema: t
                    .function
                    .parameters
                    .unwrap_or(serde_json::json!({"type": "object"})),
            })
            .collect()
    });

    let tool_choice = req
        .tool_choice
        .as_ref()
        .map(chat_tool_choice_to_claude)
        .transpose()?;

    let max_tokens = req
        .max_completion_tokens
        .or(req.max_tokens)
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let stop_sequences = req.stop.as_ref().map(stop_to_sequences);

    let temperature = req
        .temperature
        .map(|t| t.clamp(0.0, CLAUDE_TEMPERATURE_MAX));

    let requested_effort = req
        .reasoning_effort
        .as_deref()
        .or_else(|| req.reasoning.as_ref().map(|r| r.effort.as_str()));

    let has_thinking_blocks = messages.iter().any(|m| {
        if let serde_json::Value::Array(blocks) = &m.content {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(|v| v.as_str()) == Some("thinking"))
        } else {
            false
        }
    });

    let thinking =
        if let Some(cfg) = reasoning::reasoning_effort_to_thinking(requested_effort, max_tokens) {
            Some(cfg)
        } else if has_thinking_blocks {
            Some(ThinkingConfig {
                r#type: "enabled".into(),
                budget_tokens: max_tokens.max(1024),
            })
        } else {
            None
        };

    Ok(ClaudeRequest {
        model: req.model,
        max_tokens,
        messages,
        system,
        temperature,
        top_p: req.top_p,
        top_k: None,
        stop_sequences,
        stream: req.stream,
        tools,
        tool_choice,
        metadata: None,
        thinking,
    })
}

fn convert_response(resp: OpenAIChatResponse) -> Result<ClaudeResponse, ConvertError> {
    let choice = resp
        .choices
        .first()
        .ok_or_else(|| ConvertError::MissingField("choices".into()))?;

    let mut content = Vec::new();

    if let Some(text) = &choice.message.content {
        if !text.is_empty() {
            content.push(ClaudeContentBlock::Text { text: text.clone() });
        }
    }

    if let Some(reasoning) = &choice.message.reasoning_content {
        if !reasoning.is_empty() {
            content.push(ClaudeContentBlock::Thinking {
                thinking: reasoning.clone(),
                signature: None,
            });
        }
    }

    if let Some(tool_calls) = &choice.message.tool_calls {
        for tc in tool_calls {
            content.push(ClaudeContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                input: parse_arguments(&tc.function.arguments),
            });
        }
    }

    let stop_reason = match choice.finish_reason.as_deref() {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    };

    let usage = resp.usage.unwrap_or(OpenAIUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        prompt_tokens_details: None,
    });

    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens);
    let input_tokens = cache_read
        .map(|c| usage.prompt_tokens.saturating_sub(c))
        .unwrap_or(usage.prompt_tokens);

    Ok(ClaudeResponse {
        id: normalize_id_to_claude(&resp.id),
        r#type: "message".into(),
        role: "assistant".into(),
        model: resp.model,
        content,
        stop_reason: Some(stop_reason.into()),
        stop_sequence: None,
        usage: ClaudeUsage {
            input_tokens,
            output_tokens: usage.completion_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: cache_read,
        },
    })
}

fn chat_message_to_claude(msg: OpenAIChatMessage) -> Result<ClaudeMessage, ConvertError> {
    match msg.role.as_str() {
        "user" => Ok(ClaudeMessage {
            role: "user".into(),
            content: user_content_to_claude(msg.content)?,
        }),
        "assistant" => {
            let mut blocks = Vec::new();

            if let Some(reasoning) = msg.reasoning_content {
                if !reasoning.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": reasoning,
                    }));
                }
            }

            match msg.content {
                None => {}
                Some(MessageContent::Text(text)) => {
                    if !text.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": text}));
                    }
                }
                Some(MessageContent::Parts(parts)) => {
                    for part in parts {
                        blocks.push(content_part_to_claude(part)?);
                    }
                }
            }

            if let Some(tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": parse_arguments(&tc.function.arguments),
                    }));
                }
            }

            let content = assistant_blocks_to_content(&blocks);
            Ok(ClaudeMessage {
                role: "assistant".into(),
                content,
            })
        }
        "tool" => {
            let tool_call_id = msg
                .tool_call_id
                .ok_or_else(|| ConvertError::MissingField("tool_call_id".into()))?;
            let text = message_content_as_text(msg.content).unwrap_or_default();
            Ok(ClaudeMessage {
                role: "user".into(),
                content: serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": text,
                    "is_error": false,
                }]),
            })
        }
        other => Err(ConvertError::InvalidField {
            field: "role".into(),
            reason: format!("unsupported role: {other}"),
        }),
    }
}

fn user_content_to_claude(
    content: Option<MessageContent>,
) -> Result<serde_json::Value, ConvertError> {
    match content {
        None => Ok(serde_json::Value::String(String::new())),
        Some(MessageContent::Text(text)) => Ok(serde_json::Value::String(text)),
        Some(MessageContent::Parts(parts)) => {
            let blocks: Vec<serde_json::Value> = parts
                .into_iter()
                .map(content_part_to_claude)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(serde_json::Value::Array(blocks))
        }
    }
}

fn content_part_to_claude(part: ContentPart) -> Result<serde_json::Value, ConvertError> {
    match part {
        ContentPart::Text { text } => Ok(serde_json::json!({"type": "text", "text": text})),
        ContentPart::ImageUrl { image_url } => Ok(image_url_to_claude_source(&image_url.url)),
    }
}

fn chat_tool_choice_to_claude(
    value: &serde_json::Value,
) -> Result<serde_json::Value, ConvertError> {
    if let Some(s) = value.as_str() {
        return Ok(match s {
            "auto" => serde_json::json!({"type": "auto"}),
            "none" => serde_json::json!({"type": "none"}),
            "required" => serde_json::json!({"type": "any"}),
            other => {
                return Err(ConvertError::InvalidField {
                    field: "tool_choice".into(),
                    reason: format!("unsupported value: {other}"),
                });
            }
        });
    }

    if value.get("type").and_then(|v| v.as_str()) == Some("function") {
        let name = value
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| ConvertError::MissingField("tool_choice.function.name".into()))?;
        return Ok(serde_json::json!({"type": "tool", "name": name}));
    }

    Err(ConvertError::InvalidField {
        field: "tool_choice".into(),
        reason: "expected string or function object".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request_bytes(messages: Vec<serde_json::Value>, model: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "model": model,
            "messages": messages,
        }))
        .unwrap()
    }

    #[test]
    fn test_thinking_config_injected_when_history_has_reasoning() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": "hi",
                "reasoning_content": "let me think carefully..."
            }),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];
        let input = make_request_bytes(messages, "claude-sonnet-4-20250514");
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_some());
        let tc = req.thinking.unwrap();
        assert_eq!(tc.r#type, "enabled");
        assert!(tc.budget_tokens >= 1024);
    }

    #[test]
    fn test_no_thinking_config_without_thinking_history() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "assistant", "content": "hi"}),
        ];
        let input = make_request_bytes(messages, "claude-sonnet-4-20250514");
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_none());
    }

    #[test]
    fn test_reasoning_effort_overrides_history_heuristic() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": "hi",
                "reasoning_content": "let me think carefully..."
            }),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];
        let mut input = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": messages,
            "reasoning_effort": "low"
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_some());
        let tc = req.thinking.unwrap();
        assert_eq!(tc.r#type, "enabled");
        assert_eq!(tc.budget_tokens, 1024);

        input = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": messages,
            "reasoning": {"effort": "high"}
        }))
        .unwrap();
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();
        assert!(req.thinking.unwrap().budget_tokens >= 4096);
    }

    #[test]
    fn test_data_url_image_mapped_to_base64_source() {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,abc123"}
                }
            ]
        })];
        let input = make_request_bytes(messages, "claude-sonnet-4-20250514");
        let converter = Converter;
        let result = converter.convert_request(&input).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        let user_content = req.messages[0].content.as_array().unwrap();
        let image_block = user_content
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("image"))
            .unwrap();
        let source = image_block.get("source").unwrap();
        assert_eq!(source.get("type").and_then(|v| v.as_str()), Some("base64"));
        assert_eq!(
            source.get("media_type").and_then(|v| v.as_str()),
            Some("image/png")
        );
        assert_eq!(source.get("data").and_then(|v| v.as_str()), Some("abc123"));
    }

    #[test]
    fn test_convert_response_id_normalizes_to_claude() {
        let resp_bytes = serde_json::to_vec(&serde_json::json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "created": 1700000000u64,
            "model": "o1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&resp_bytes).unwrap();
        let resp: ClaudeResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.id, "msg_abc");
    }

    #[test]
    fn test_reasoning_content_mapped_to_thinking_block() {
        let resp_bytes = serde_json::to_vec(&serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1700000000u64,
            "model": "o1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "deep thought"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_response(&resp_bytes).unwrap();
        let resp: ClaudeResponse = serde_json::from_slice(&result).unwrap();

        let has_thinking = resp.content.iter().any(|b| {
            matches!(b, ClaudeContentBlock::Thinking { thinking, .. } if thinking == "deep thought")
        });
        assert!(has_thinking);
    }
}
