use crate::error::ConvertError;
use crate::formats::FormatAdapter;
use crate::ir::*;

use super::tools;
use super::types::*;

pub struct OpenAIChatAdapter;

impl FormatAdapter for OpenAIChatAdapter {
    type Request = OpenAIChatRequest;
    type Response = OpenAIChatResponse;

    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn request_to_canonical(req: OpenAIChatRequest) -> Result<CanonicalRequest, ConvertError> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut turns: Vec<Turn> = Vec::new();

        for msg in req.messages {
            match msg.role.as_str() {
                "system" | "developer" => {
                    if let Some(text) = message_content_as_text(msg.content) {
                        system_parts.push(text);
                    }
                }
                "user" | "assistant" | "tool" => {
                    turns.push(message_to_turn(msg)?);
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
        } else {
            Some(SystemContent::Text(system_parts.join("\n")))
        };

        let tools_defs = req
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                Ok(ToolDef {
                    name: t.function.name,
                    description: t.function.description,
                    input_schema: t
                        .function
                        .parameters
                        .unwrap_or(serde_json::json!({"type": "object"})),
                    strict: t.function.strict,
                })
            })
            .collect::<Result<Vec<_>, ConvertError>>()?;

        let tool_choice = req
            .tool_choice
            .as_ref()
            .and_then(tools::parse_tool_choice);

        let max_output_tokens = req.max_completion_tokens.or(req.max_tokens);
        let stop_sequences = req
            .stop
            .map(tools::stop_to_sequences)
            .unwrap_or_default();

        let response_format = req
            .response_format
            .as_ref()
            .and_then(tools::parse_response_format);

        Ok(CanonicalRequest {
            model: req.model,
            system,
            turns,
            tools: tools_defs,
            tool_choice,
            params: GenerationParams {
                temperature: req.temperature,
                top_p: req.top_p,
                max_output_tokens,
                stop_sequences,
                seed: req.seed,
                response_format,
                ..Default::default()
            },
            stream: req.stream.unwrap_or(false),
            extra: serde_json::Value::Null,
        })
    }

    fn request_from_canonical(req: &CanonicalRequest) -> Result<OpenAIChatRequest, ConvertError> {
        let mut messages: Vec<OpenAIChatMessage> = Vec::new();

        if let Some(system) = &req.system {
            messages.push(OpenAIChatMessage {
                role: "system".into(),
                content: Some(MessageContent::Text(system.as_text())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }

        for turn in &req.turns {
            messages.extend(turn_to_messages(turn)?);
        }

        let tools_out = if req.tools.is_empty() {
            None
        } else {
            Some(
                req.tools
                    .iter()
                    .map(|t| OpenAIChatTool {
                        r#type: "function".into(),
                        function: FunctionDef {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: Some(t.input_schema.clone()),
                            strict: t.strict,
                        },
                    })
                    .collect(),
            )
        };

        let tool_choice = req.tool_choice.as_ref().map(tools::tool_choice_to_json);
        let stop = tools::sequences_to_stop(&req.params.stop_sequences);
        let response_format = req
            .params
            .response_format
            .as_ref()
            .map(tools::response_format_to_json);

        Ok(OpenAIChatRequest {
            model: req.model.clone(),
            messages,
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            max_completion_tokens: req.params.max_output_tokens,
            max_tokens: None,
            stop,
            seed: req.params.seed,
            stream: if req.stream { Some(true) } else { None },
            stream_options: None,
            tools: tools_out,
            tool_choice,
            response_format,
            n: None,
        })
    }

    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn response_to_canonical(resp: OpenAIChatResponse) -> Result<CanonicalResponse, ConvertError> {
        let choice = resp
            .choices
            .first()
            .ok_or_else(|| ConvertError::MissingField("choices".into()))?;

        let mut content = Vec::new();

        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text: text.clone() });
            }
        }

        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let input: serde_json::Value = if tc.function.arguments.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&tc.function.arguments)?
                };
                content.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    input,
                });
            }
        }

        let stop_reason = choice
            .finish_reason
            .as_deref()
            .map(StopReason::from_openai_chat)
            .unwrap_or(StopReason::EndTurn);

        let usage = resp.usage.map(|u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            ..Default::default()
        }).unwrap_or_default();

        Ok(CanonicalResponse {
            id: resp.id,
            model: resp.model,
            content,
            stop_reason,
            usage,
        })
    }

    fn response_from_canonical(resp: &CanonicalResponse) -> Result<OpenAIChatResponse, ConvertError> {
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in &resp.content {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.clone()),
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        r#type: "function".into(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(input)?,
                        },
                    });
                }
                ContentBlock::Thinking { text, .. } => {
                    text_parts.push(text.clone());
                }
                other => {
                    return Err(ConvertError::UnsupportedContentType(format!(
                        "{other:?}"
                    )));
                }
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let total_tokens = resp.usage.input_tokens + resp.usage.output_tokens;

        Ok(OpenAIChatResponse {
            id: if resp.id.is_empty() {
                format!("chatcmpl-{}", uuid::Uuid::new_v4())
            } else {
                resp.id.clone()
            },
            object: "chat.completion".into(),
            created,
            model: resp.model.clone(),
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
                },
                finish_reason: Some(resp.stop_reason.to_openai_chat().into()),
            }],
            usage: Some(OpenAIUsage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens,
            }),
            system_fingerprint: None,
        })
    }
}

fn message_content_as_text(content: Option<MessageContent>) -> Option<String> {
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

fn content_parts_to_blocks(content: Option<MessageContent>) -> Result<Vec<ContentBlock>, ConvertError> {
    match content {
        None => Ok(vec![]),
        Some(MessageContent::Text(text)) => Ok(vec![ContentBlock::Text { text }]),
        Some(MessageContent::Parts(parts)) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        blocks.push(ContentBlock::Text { text });
                    }
                    ContentPart::ImageUrl { image_url } => {
                        blocks.push(ContentBlock::Image {
                            source: ImageSource::Url {
                                url: image_url.url,
                                detail: image_url.detail,
                            },
                        });
                    }
                }
            }
            Ok(blocks)
        }
    }
}

fn blocks_to_message_content(blocks: &[ContentBlock]) -> Result<Option<MessageContent>, ConvertError> {
    if blocks.is_empty() {
        return Ok(None);
    }

    let has_image = blocks.iter().any(|b| matches!(b, ContentBlock::Image { .. }));
    if has_image {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(ContentPart::Text { text: text.clone() });
                }
                ContentBlock::Image { source } => {
                    if let ImageSource::Url { url, detail } = source {
                        parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrlDetail {
                                url: url.clone(),
                                detail: detail.clone(),
                            },
                        });
                    } else {
                        return Err(ConvertError::UnsupportedContentType(
                            "base64 image".into(),
                        ));
                    }
                }
                other => {
                    return Err(ConvertError::UnsupportedContentType(format!(
                        "{other:?}"
                    )));
                }
            }
        }
        Ok(Some(MessageContent::Parts(parts)))
    } else {
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        Ok(if text.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text))
        })
    }
}

fn message_to_turn(msg: OpenAIChatMessage) -> Result<Turn, ConvertError> {
    match msg.role.as_str() {
        "user" => Ok(Turn {
            role: Role::User,
            content: content_parts_to_blocks(msg.content)?,
        }),
        "assistant" => {
            let mut content = content_parts_to_blocks(msg.content)?;
            if let Some(reasoning) = msg.reasoning_content {
                content.insert(
                    0,
                    ContentBlock::Thinking {
                        text: reasoning,
                        signature: None,
                    },
                );
            }
            if let Some(tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    let input: serde_json::Value = if tc.function.arguments.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&tc.function.arguments)?
                    };
                    content.push(ContentBlock::ToolUse {
                        id: tc.id,
                        name: tc.function.name,
                        input,
                    });
                }
            }
            Ok(Turn {
                role: Role::Assistant,
                content,
            })
        }
        "tool" => {
            let tool_call_id = msg.tool_call_id.ok_or_else(|| {
                ConvertError::MissingField("tool_call_id".into())
            })?;
            let text = message_content_as_text(msg.content).unwrap_or_default();
            Ok(Turn {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: vec![ContentBlock::Text { text }],
                    is_error: false,
                }],
            })
        }
        other => Err(ConvertError::InvalidField {
            field: "role".into(),
            reason: format!("unsupported role: {other}"),
        }),
    }
}

fn turn_to_messages(turn: &Turn) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    match turn.role {
        Role::User => {
            let tool_results: Vec<_> = turn
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                .collect();
            let non_tool: Vec<_> = turn
                .content
                .iter()
                .filter(|b| !matches!(b, ContentBlock::ToolResult { .. }))
                .collect();

            let mut messages = Vec::new();

            if !non_tool.is_empty() {
                let blocks: Vec<ContentBlock> = non_tool.into_iter().cloned().collect();
                messages.push(OpenAIChatMessage {
                    role: "user".into(),
                    content: blocks_to_message_content(&blocks)?,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }

            for block in tool_results {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: _,
                } = block
                {
                    let text: String = content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    messages.push(OpenAIChatMessage {
                        role: "tool".into(),
                        content: Some(MessageContent::Text(text)),
                        name: None,
                        tool_calls: None,
                        tool_call_id: Some(tool_use_id.clone()),
                        reasoning_content: None,
                    });
                }
            }

            Ok(messages)
        }
        Role::Assistant => {
            let mut text_blocks = Vec::new();
            let mut tool_calls = Vec::new();
            let mut reasoning_content = None;

            for block in &turn.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                    ContentBlock::Thinking { text, .. } => {
                        reasoning_content = Some(text.clone());
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            r#type: "function".into(),
                            function: FunctionCall {
                                name: name.clone(),
                                arguments: serde_json::to_string(input)?,
                            },
                        });
                    }
                    other => {
                        return Err(ConvertError::UnsupportedContentType(format!(
                            "{other:?}"
                        )));
                    }
                }
            }

            Ok(vec![OpenAIChatMessage {
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
            }])
        }
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
