use crate::error::ConvertError;
use crate::formats::FormatAdapter;
use crate::ir::*;

use super::helpers;
use super::tools;
use super::types::*;

pub struct OpenAIResponsesAdapter;

impl FormatAdapter for OpenAIResponsesAdapter {
    type Request = OpenAIResponsesRequest;
    type Response = OpenAIResponsesResponse;

    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn request_to_canonical(req: OpenAIResponsesRequest) -> Result<CanonicalRequest, ConvertError> {
        let mut system_parts: Vec<String> = Vec::new();
        if let Some(instructions) = &req.instructions {
            system_parts.push(instructions.clone());
        }

        let (turns, developer_msgs) = parse_input_to_turns_and_developer(req.input.as_ref())?;

        for msg in developer_msgs {
            system_parts.push(msg);
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(SystemContent::Text(system_parts.join("\n\n")))
        };

        let mut tool_defs = Vec::new();
        for t in req.tools.unwrap_or_default() {
            tool_defs.extend(tools::parse_tool_defs(t)?);
        }

        let tool_choice = req
            .tool_choice
            .as_ref()
            .map(tools::parse_tool_choice)
            .transpose()?;

        let response_format = req.text.as_ref().and_then(tools::parse_response_format);

        let params = GenerationParams {
            temperature: req.temperature,
            top_p: req.top_p,
            max_output_tokens: req.max_output_tokens,
            response_format,
            ..Default::default()
        };

        Ok(CanonicalRequest {
            model: req.model,
            system,
            turns,
            tools: tool_defs,
            tool_choice,
            params,
            stream: req.stream.unwrap_or(false),
            extra: serde_json::Value::Null,
        })
    }

    fn request_from_canonical(
        req: &CanonicalRequest,
    ) -> Result<OpenAIResponsesRequest, ConvertError> {
        let instructions = req.system.as_ref().map(SystemContent::as_text);
        let input = turns_to_input(&req.turns)?;

        let tool_list = if req.tools.is_empty() {
            None
        } else {
            Some(
                req.tools
                    .iter()
                    .map(tools::tool_def_to_json)
                    .collect::<Vec<_>>(),
            )
        };

        let tool_choice = req.tool_choice.as_ref().map(tools::tool_choice_to_json);

        let text = req
            .params
            .response_format
            .as_ref()
            .map(tools::response_format_to_text);

        Ok(OpenAIResponsesRequest {
            model: req.model.clone(),
            input: Some(input),
            instructions,
            max_output_tokens: req.params.max_output_tokens,
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            stream: if req.stream { Some(true) } else { None },
            tools: tool_list,
            tool_choice,
            text,
            reasoning: None,
            previous_response_id: None,
            store: None,
        })
    }

    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn response_to_canonical(
        resp: OpenAIResponsesResponse,
    ) -> Result<CanonicalResponse, ConvertError> {
        let mut content = Vec::new();
        for item in &resp.output {
            content.extend(parse_output_item(item)?);
        }

        let stop_reason =
            if content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })) {
                StopReason::ToolUse
            } else {
                helpers::status_to_stop_reason(&resp.status)
            };

        let usage = resp
            .usage
            .map(|u| Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                ..Default::default()
            })
            .unwrap_or_default();

        Ok(CanonicalResponse {
            id: resp.id,
            model: resp.model,
            content,
            stop_reason,
            usage,
        })
    }

    fn response_from_canonical(
        resp: &CanonicalResponse,
    ) -> Result<OpenAIResponsesResponse, ConvertError> {
        let output = content_blocks_to_output(&resp.content)?;
        let status = helpers::stop_reason_to_status(&resp.stop_reason).to_string();

        Ok(OpenAIResponsesResponse {
            id: if resp.id.is_empty() {
                format!("resp_{}", uuid::Uuid::new_v4())
            } else {
                resp.id.clone()
            },
            object: "response".into(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            model: resp.model.clone(),
            status,
            output,
            usage: Some(ResponsesUsage {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                total_tokens: Some(resp.usage.total_tokens()),
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

/// Parse input items, separating developer messages (system-level) from conversation turns.
fn parse_input_to_turns_and_developer(
    input: Option<&serde_json::Value>,
) -> Result<(Vec<Turn>, Vec<String>), ConvertError> {
    let Some(input) = input else {
        return Ok((vec![], vec![]));
    };

    if let Some(text) = input.as_str() {
        return Ok((
            vec![Turn {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                }],
            }],
            vec![],
        ));
    }

    let items = input
        .as_array()
        .ok_or_else(|| ConvertError::InvalidField {
            field: "input".into(),
            reason: "expected string or array".into(),
        })?;

    let mut turns = Vec::new();
    let mut developer_msgs = Vec::new();

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if item_type == "message" && (role == "developer" || role == "system") {
            let text = extract_message_text(item);
            if !text.is_empty() {
                developer_msgs.push(text);
            }
        } else {
            turns.push(parse_input_item(item)?);
        }
    }

    Ok((turns, developer_msgs))
}

fn extract_message_text(item: &serde_json::Value) -> String {
    if let Some(content) = item.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(arr) = content.as_array() {
            return arr
                .iter()
                .filter_map(|part| {
                    let t = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if t == "input_text" || t == "output_text" || t == "text" {
                        part.get("text").and_then(|v| v.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    String::new()
}

fn parse_input_item(item: &serde_json::Value) -> Result<Turn, ConvertError> {
    let item_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("input item type".into()))?;

    match item_type {
        "message" => {
            let role_str = item
                .get("role")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("message role".into()))?;
            let role = match role_str {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                other => {
                    return Err(ConvertError::InvalidField {
                        field: "role".into(),
                        reason: format!("unsupported role: {other}"),
                    });
                }
            };

            let content = item
                .get("content")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(parse_message_content)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();

            Ok(Turn { role, content })
        }
        "function_call" => {
            let (call_id, name, input) = helpers::parse_function_call_fields(item)?;
            Ok(Turn {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                }],
            })
        }
        "function_call_output" => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ConvertError::MissingField("function_call_output call_id".into())
                })?
                .to_string();
            let output = item
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            Ok(Turn {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: call_id,
                    content: vec![ContentBlock::Text { text: output }],
                    is_error: false,
                }],
            })
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn parse_message_content(part: &serde_json::Value) -> Result<ContentBlock, ConvertError> {
    let part_type = part
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("content part type".into()))?;

    match part_type {
        "input_text" | "output_text" => {
            let text = part
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ContentBlock::Text { text })
        }
        "input_image" => {
            if let Some(url) = part.get("image_url").and_then(|v| v.as_str()) {
                Ok(ContentBlock::Image {
                    source: ImageSource::Url {
                        url: url.to_string(),
                        detail: part
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    },
                })
            } else if let Some(data) = part
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
            {
                Ok(ContentBlock::Image {
                    source: ImageSource::Url {
                        url: data.to_string(),
                        detail: part
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    },
                })
            } else {
                Err(ConvertError::MissingField("input_image url".into()))
            }
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Input / output serialization
// ---------------------------------------------------------------------------

fn turns_to_input(turns: &[Turn]) -> Result<serde_json::Value, ConvertError> {
    let mut items = Vec::new();

    for turn in turns {
        let mut tool_results = Vec::new();
        let mut message_content = Vec::new();
        let mut function_calls = Vec::new();

        for block in &turn.content {
            match block {
                ContentBlock::Text { text } => {
                    let content_type = if turn.role == Role::Assistant {
                        "output_text"
                    } else {
                        "input_text"
                    };
                    message_content.push(serde_json::json!({
                        "type": content_type,
                        "text": text,
                    }));
                }
                ContentBlock::Image { source } => {
                    let (url, detail) = match source {
                        ImageSource::Url { url, detail } => (url.clone(), detail.clone()),
                        ImageSource::Base64 { media_type, data } => {
                            (format!("data:{media_type};base64,{data}"), None)
                        }
                    };
                    let mut part = serde_json::json!({
                        "type": "input_image",
                        "image_url": url,
                    });
                    if let Some(d) = detail {
                        part["detail"] = serde_json::Value::String(d);
                    }
                    message_content.push(part);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    function_calls.push(helpers::emit_function_call_json(id, name, input));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let output = content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    tool_results.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output,
                    }));
                }
                ContentBlock::Thinking { .. } => {}
            }
        }

        if !message_content.is_empty() {
            let role = match turn.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            items.push(serde_json::json!({
                "type": "message",
                "role": role,
                "content": message_content,
            }));
        }

        items.extend(function_calls);
        items.extend(tool_results);
    }

    Ok(serde_json::Value::Array(items))
}

fn parse_output_item(item: &serde_json::Value) -> Result<Vec<ContentBlock>, ConvertError> {
    let item_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("output item type".into()))?;

    match item_type {
        "message" => {
            let content = item
                .get("content")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|part| {
                            let part_type = part.get("type").and_then(|v| v.as_str())?;
                            if part_type == "output_text" {
                                let text =
                                    part.get("text").and_then(|v| v.as_str())?.to_string();
                                Some(ContentBlock::Text { text })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(content)
        }
        "function_call" => {
            let (call_id, name, input) = helpers::parse_function_call_fields(item)?;
            Ok(vec![ContentBlock::ToolUse {
                id: call_id,
                name,
                input,
            }])
        }
        _ => Ok(vec![]),
    }
}

fn content_blocks_to_output(
    blocks: &[ContentBlock],
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut output = Vec::new();
    let mut text_parts = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(serde_json::json!({
                    "type": "output_text",
                    "text": text,
                }));
            }
            ContentBlock::ToolUse { id, name, input } => {
                if !text_parts.is_empty() {
                    output.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": std::mem::take(&mut text_parts),
                    }));
                }
                output.push(helpers::emit_function_call_json(id, name, input));
            }
            _ => {}
        }
    }

    if !text_parts.is_empty() {
        output.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": text_parts,
        }));
    }

    Ok(output)
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
