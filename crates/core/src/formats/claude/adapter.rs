use crate::error::ConvertError;
use crate::formats::FormatAdapter;
use crate::ir::*;

use super::helpers;
use super::types::*;

pub struct ClaudeAdapter;

const CLAUDE_TEMPERATURE_MAX: f32 = 1.0;

impl FormatAdapter for ClaudeAdapter {
    type Request = ClaudeRequest;
    type Response = ClaudeResponse;

    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn request_to_canonical(req: ClaudeRequest) -> Result<CanonicalRequest, ConvertError> {
        let system = req
            .system
            .map(parse_system)
            .transpose()?;

        let mut turns = Vec::with_capacity(req.messages.len());
        for msg in req.messages {
            turns.push(parse_message(msg)?);
        }

        let tools = req
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|t| ToolDef {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
                strict: None,
            })
            .collect();

        let tool_choice = req
            .tool_choice
            .map(helpers::parse_tool_choice)
            .transpose()?;

        let params = GenerationParams {
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: req.top_k,
            max_output_tokens: Some(req.max_tokens),
            stop_sequences: req.stop_sequences.unwrap_or_default(),
            ..Default::default()
        };

        Ok(CanonicalRequest {
            model: req.model,
            system,
            turns,
            tools,
            tool_choice,
            params,
            stream: req.stream.unwrap_or(false),
            extra: req.metadata.unwrap_or(serde_json::Value::Null),
        })
    }

    fn request_from_canonical(req: &CanonicalRequest) -> Result<ClaudeRequest, ConvertError> {
        let system = req
            .system
            .as_ref()
            .map(system_to_claude)
            .transpose()?;

        let merged_turns = helpers::merge_consecutive_same_role_turns(&req.turns);
        let messages = merged_turns
            .iter()
            .map(turn_to_claude_message)
            .collect::<Result<Vec<_>, _>>()?;

        let tools = if req.tools.is_empty() {
            None
        } else {
            Some(
                req.tools
                    .iter()
                    .map(|t| ClaudeTool {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.input_schema.clone(),
                    })
                    .collect(),
            )
        };

        let tool_choice = req
            .tool_choice
            .as_ref()
            .map(helpers::tool_choice_to_claude)
            .transpose()?;

        let max_tokens = req
            .params
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_TOKENS);

        let stop_sequences = if req.params.stop_sequences.is_empty() {
            None
        } else {
            Some(req.params.stop_sequences.clone())
        };

        Ok(ClaudeRequest {
            model: req.model.clone(),
            max_tokens,
            messages,
            system,
            temperature: req.params.clamped_temperature(CLAUDE_TEMPERATURE_MAX),
            top_p: req.params.top_p,
            top_k: req.params.top_k,
            stop_sequences,
            stream: if req.stream { Some(true) } else { None },
            tools,
            tool_choice,
            metadata: if req.extra.is_null() {
                None
            } else {
                Some(req.extra.clone())
            },
        })
    }

    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn response_to_canonical(resp: ClaudeResponse) -> Result<CanonicalResponse, ConvertError> {
        let content = resp
            .content
            .into_iter()
            .map(claude_response_block_to_ir)
            .collect::<Result<Vec<_>, _>>()?;

        let stop_reason = resp
            .stop_reason
            .as_deref()
            .map(StopReason::from_claude)
            .unwrap_or(StopReason::EndTurn);

        Ok(CanonicalResponse {
            id: resp.id,
            model: resp.model,
            content,
            stop_reason,
            usage: helpers::claude_usage_to_ir(&resp.usage),
        })
    }

    fn response_from_canonical(resp: &CanonicalResponse) -> Result<ClaudeResponse, ConvertError> {
        let content = resp
            .content
            .iter()
            .map(ir_block_to_claude_response)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ClaudeResponse {
            id: if resp.id.is_empty() {
                format!("msg_{}", uuid::Uuid::new_v4())
            } else {
                resp.id.clone()
            },
            r#type: "message".into(),
            role: "assistant".into(),
            model: resp.model.clone(),
            content,
            stop_reason: Some(resp.stop_reason.to_claude().to_string()),
            stop_sequence: None,
            usage: helpers::ir_usage_to_claude(&resp.usage),
        })
    }
}

fn parse_system(value: serde_json::Value) -> Result<SystemContent, ConvertError> {
    match value {
        serde_json::Value::String(text) => Ok(SystemContent::Text(text)),
        serde_json::Value::Array(arr) => {
            let blocks = arr
                .into_iter()
                .map(|item| {
                    let obj = item.as_object().ok_or_else(|| ConvertError::InvalidField {
                        field: "system".into(),
                        reason: "expected object block".into(),
                    })?;
                    let text = obj
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ConvertError::InvalidField {
                            field: "system".into(),
                            reason: "text block missing text field".into(),
                        })?
                        .to_string();
                    let cache_control = obj.get("cache_control").cloned();
                    Ok(SystemBlock {
                        text,
                        cache_control,
                    })
                })
                .collect::<Result<Vec<_>, ConvertError>>()?;
            Ok(SystemContent::Blocks(blocks))
        }
        _ => Err(ConvertError::InvalidField {
            field: "system".into(),
            reason: "expected string or array".into(),
        }),
    }
}

fn system_to_claude(system: &SystemContent) -> Result<serde_json::Value, ConvertError> {
    match system {
        SystemContent::Text(text) => Ok(serde_json::Value::String(text.clone())),
        SystemContent::Blocks(blocks) => {
            let arr = blocks
                .iter()
                .map(|b| {
                    let mut obj = serde_json::json!({
                        "type": "text",
                        "text": b.text,
                    });
                    if let Some(cache) = &b.cache_control {
                        obj["cache_control"] = cache.clone();
                    }
                    obj
                })
                .collect::<Vec<_>>();
            Ok(serde_json::Value::Array(arr))
        }
    }
}

fn parse_message(msg: ClaudeMessage) -> Result<Turn, ConvertError> {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => {
            return Err(ConvertError::InvalidField {
                field: "role".into(),
                reason: format!("unsupported role: {other}"),
            });
        }
    };

    let content = parse_message_content(&msg.content)?;
    Ok(Turn { role, content })
}

fn parse_message_content(value: &serde_json::Value) -> Result<Vec<ContentBlock>, ConvertError> {
    match value {
        serde_json::Value::String(text) => Ok(vec![ContentBlock::Text {
            text: text.clone(),
        }]),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(parse_content_block)
            .collect::<Result<Vec<_>, _>>(),
        _ => Err(ConvertError::InvalidField {
            field: "content".into(),
            reason: "expected string or array".into(),
        }),
    }
}

fn parse_content_block(value: &serde_json::Value) -> Result<ContentBlock, ConvertError> {
    let obj = value.as_object().ok_or_else(|| ConvertError::InvalidField {
        field: "content".into(),
        reason: "expected object".into(),
    })?;
    let block_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::InvalidField {
            field: "content.type".into(),
            reason: "missing type field".into(),
        })?;

    match block_type {
        "text" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::InvalidField {
                    field: "content.text".into(),
                    reason: "missing text field".into(),
                })?
                .to_string();
            Ok(ContentBlock::Text { text })
        }
        "image" => {
            let source = obj.get("source").ok_or_else(|| ConvertError::InvalidField {
                field: "content.image.source".into(),
                reason: "missing source field".into(),
            })?;
            Ok(ContentBlock::Image {
                source: parse_image_source(source)?,
            })
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
            let input = obj
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            Ok(ContentBlock::ToolUse { id, name, input })
        }
        "tool_result" => {
            let tool_use_id = obj
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("tool_result.tool_use_id".into()))?
                .to_string();
            let is_error = obj
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content_value = obj
                .get("content")
                .ok_or_else(|| ConvertError::MissingField("tool_result.content".into()))?;
            let content = parse_tool_result_content(content_value)?;
            Ok(ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            })
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn parse_tool_result_content(value: &serde_json::Value) -> Result<Vec<ContentBlock>, ConvertError> {
    match value {
        serde_json::Value::String(text) => Ok(vec![ContentBlock::Text {
            text: text.clone(),
        }]),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(parse_content_block)
            .collect::<Result<Vec<_>, _>>(),
        _ => Err(ConvertError::InvalidField {
            field: "tool_result.content".into(),
            reason: "expected string or array".into(),
        }),
    }
}

fn parse_image_source(value: &serde_json::Value) -> Result<ImageSource, ConvertError> {
    let obj = value.as_object().ok_or_else(|| ConvertError::InvalidField {
        field: "image.source".into(),
        reason: "expected object".into(),
    })?;
    let source_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("image.source.type".into()))?;

    match source_type {
        "base64" => {
            let media_type = obj
                .get("media_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.media_type".into()))?
                .to_string();
            let data = obj
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.data".into()))?
                .to_string();
            Ok(ImageSource::Base64 { media_type, data })
        }
        "url" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("image.source.url".into()))?
                .to_string();
            Ok(ImageSource::Url { url, detail: None })
        }
        other => Err(ConvertError::UnsupportedContentType(format!(
            "image source: {other}"
        ))),
    }
}

fn turn_to_claude_message(turn: &Turn) -> Result<ClaudeMessage, ConvertError> {
    let role = match turn.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
    .to_string();

    let content = if turn.content.len() == 1 {
        if let ContentBlock::Text { text } = &turn.content[0] {
            serde_json::Value::String(text.clone())
        } else {
            serde_json::Value::Array(
                turn.content
                    .iter()
                    .map(ir_block_to_claude_message)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
    } else {
        serde_json::Value::Array(
            turn.content
                .iter()
                .map(ir_block_to_claude_message)
                .collect::<Result<Vec<_>, _>>()?,
        )
    };

    Ok(ClaudeMessage { role, content })
}

fn ir_block_to_claude_message(block: &ContentBlock) -> Result<serde_json::Value, ConvertError> {
    match block {
        ContentBlock::Text { text } => Ok(serde_json::json!({"type": "text", "text": text})),
        ContentBlock::Image { source } => Ok(serde_json::json!({
            "type": "image",
            "source": image_source_to_claude(source)?,
        })),
        ContentBlock::ToolUse { id, name, input } => Ok(serde_json::json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let content_value = if content.len() == 1 {
                if let ContentBlock::Text { text } = &content[0] {
                    serde_json::Value::String(text.clone())
                } else {
                    serde_json::Value::Array(
                        content
                            .iter()
                            .map(ir_block_to_claude_message)
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
            } else {
                serde_json::Value::Array(
                    content
                        .iter()
                        .map(ir_block_to_claude_message)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            };
            Ok(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content_value,
                "is_error": is_error,
            }))
        }
        ContentBlock::Thinking { .. } => Err(ConvertError::UnsupportedContentType(
            "thinking in request messages".into(),
        )),
    }
}

fn image_source_to_claude(source: &ImageSource) -> Result<serde_json::Value, ConvertError> {
    match source {
        ImageSource::Base64 { media_type, data } => Ok(serde_json::json!({
            "type": "base64",
            "media_type": media_type,
            "data": data,
        })),
        ImageSource::Url { url, .. } => Ok(serde_json::json!({
            "type": "url",
            "url": url,
        })),
    }
}

fn claude_response_block_to_ir(block: ClaudeContentBlock) -> Result<ContentBlock, ConvertError> {
    match block {
        ClaudeContentBlock::Text { text } => Ok(ContentBlock::Text { text }),
        ClaudeContentBlock::ToolUse { id, name, input } => {
            Ok(ContentBlock::ToolUse { id, name, input })
        }
        ClaudeContentBlock::Thinking {
            thinking,
            signature,
        } => Ok(ContentBlock::Thinking {
            text: thinking,
            signature,
        }),
    }
}

fn ir_block_to_claude_response(block: &ContentBlock) -> Result<ClaudeContentBlock, ConvertError> {
    match block {
        ContentBlock::Text { text } => Ok(ClaudeContentBlock::Text { text: text.clone() }),
        ContentBlock::ToolUse { id, name, input } => Ok(ClaudeContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        }),
        ContentBlock::Thinking { text, signature } => Ok(ClaudeContentBlock::Thinking {
            thinking: text.clone(),
            signature: signature.clone(),
        }),
        other => Err(ConvertError::UnsupportedContentType(format!(
            "response content: {other:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
