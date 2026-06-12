use crate::converters::FormatConverter;
use crate::converters::shared::{
    blocks_to_user_content, extract_message_text, merge_claude_messages, new_msg_id,
    parse_function_call_fields, *,
};
use crate::error::ConvertError;
use crate::formats::claude::*;
use crate::formats::openai_resp::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: OpenAIResponsesRequest = serde_json::from_slice(input)?;
        let out = convert_request(req)?;
        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: OpenAIResponsesResponse = serde_json::from_slice(input)?;
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
        use crate::formats::openai_resp::OpenAIResponsesStreamAdapter;

        let canonical = OpenAIResponsesStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(ClaudeStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIResponsesRequest) -> Result<ClaudeRequest, ConvertError> {
    let mut system_parts: Vec<String> = Vec::new();
    let messages = parse_input_to_messages(req.input.as_ref(), &mut system_parts)?;

    if let Some(instructions) = &req.instructions {
        system_parts.push(instructions.clone());
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(serde_json::Value::String(system_parts.join("\n\n")))
    };

    let (tools, ns_tool_names) = parse_resp_tools_with_ns(req.tools)?;
    let tool_choice = req
        .tool_choice
        .as_ref()
        .map(|v| resp_tool_choice_to_claude(v, &ns_tool_names))
        .transpose()?;

    let max_tokens = req.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let temperature = req
        .temperature
        .map(|t| t.clamp(0.0, CLAUDE_TEMPERATURE_MAX));

    let thinking = map_reasoning_to_thinking(&req.reasoning, max_tokens);

    Ok(ClaudeRequest {
        model: req.model,
        max_tokens,
        messages,
        system,
        temperature,
        top_p: req.top_p,
        top_k: None,
        stop_sequences: None,
        stream: req.stream,
        tools,
        tool_choice,
        metadata: None,
        thinking,
    })
}

fn convert_response(resp: OpenAIResponsesResponse) -> Result<ClaudeResponse, ConvertError> {
    let content = output_to_claude_content(&resp.output)?;

    let stop_reason = match resp.status.as_str() {
        "completed" => "end_turn",
        "incomplete" => "max_tokens",
        "failed" => "end_turn",
        _ => "end_turn",
    };

    let usage = resp.usage.unwrap_or(ResponsesUsage {
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: None,
    });

    Ok(ClaudeResponse {
        id: if resp.id.is_empty() {
            new_msg_id()
        } else {
            resp.id.clone()
        },
        r#type: "message".into(),
        role: "assistant".into(),
        model: resp.model,
        content,
        stop_reason: Some(stop_reason.into()),
        stop_sequence: None,
        usage: ClaudeUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    })
}

fn parse_input_to_messages(
    input: Option<&serde_json::Value>,
    system_parts: &mut Vec<String>,
) -> Result<Vec<ClaudeMessage>, ConvertError> {
    let Some(input) = input else {
        return Ok(vec![]);
    };

    if let Some(text) = input.as_str() {
        return Ok(vec![ClaudeMessage {
            role: "user".into(),
            content: serde_json::Value::String(text.to_string()),
        }]);
    }

    let items = input.as_array().ok_or_else(|| ConvertError::InvalidField {
        field: "input".into(),
        reason: "expected string or array".into(),
    })?;

    let mut messages = Vec::new();
    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");

        if item_type == ITEM_TYPE_MESSAGE && (role == "developer" || role == "system") {
            let text = extract_message_text(item);
            if !text.is_empty() {
                system_parts.push(text);
            }
            continue;
        }

        messages.push(parse_input_item(item)?);
    }

    Ok(merge_claude_messages(messages))
}

fn parse_input_item(item: &serde_json::Value) -> Result<ClaudeMessage, ConvertError> {
    let item_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("input item type".into()))?;

    match item_type {
        ITEM_TYPE_MESSAGE => {
            let role_str = item
                .get("role")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("message role".into()))?;
            let blocks = item
                .get("content")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(parse_message_content_part)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();

            match role_str {
                "user" => Ok(ClaudeMessage {
                    role: "user".into(),
                    content: blocks_to_user_content(&blocks),
                }),
                "assistant" => Ok(ClaudeMessage {
                    role: "assistant".into(),
                    content: assistant_blocks_to_content(&blocks),
                }),
                other => Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                }),
            }
        }
        ITEM_TYPE_FUNCTION_CALL => {
            let (call_id, name, input) = parse_function_call_fields(item)?;
            Ok(ClaudeMessage {
                role: "assistant".into(),
                content: serde_json::json!([{
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input,
                }]),
            })
        }
        ITEM_TYPE_FUNCTION_CALL_OUTPUT => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("function_call_output call_id".into()))?
                .to_string();
            let output = item
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ClaudeMessage {
                role: "user".into(),
                content: serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": output,
                    "is_error": false,
                }]),
            })
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn parse_message_content_part(part: &serde_json::Value) -> Result<serde_json::Value, ConvertError> {
    let part_type = part
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("content part type".into()))?;

    match part_type {
        ITEM_TYPE_INPUT_TEXT | ITEM_TYPE_OUTPUT_TEXT => {
            let text = part
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(serde_json::json!({"type": "text", "text": text}))
        }
        ITEM_TYPE_INPUT_IMAGE => input_image_to_claude_block(part),
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn input_image_to_claude_block(
    part: &serde_json::Value,
) -> Result<serde_json::Value, ConvertError> {
    let url = if let Some(url) = part.get("image_url").and_then(|v| v.as_str()) {
        url.to_string()
    } else if let Some(url) = part
        .get("image_url")
        .and_then(|v| v.get("url"))
        .and_then(|v| v.as_str())
    {
        url.to_string()
    } else {
        return Err(ConvertError::MissingField("input_image url".into()));
    };
    Ok(serde_json::json!({
        "type": "image",
        "source": {"type": "url", "url": url}
    }))
}

/// Returns (tools, namespace_tool_names) where namespace_tool_names maps
/// short tool names to their qualified (namespace__name) forms.
fn parse_resp_tools_with_ns(
    tools: Option<Vec<serde_json::Value>>,
) -> Result<
    (
        Option<Vec<ClaudeTool>>,
        std::collections::HashMap<String, String>,
    ),
    ConvertError,
> {
    let mut ns_map = std::collections::HashMap::new();
    let Some(tools) = tools else {
        return Ok((None, ns_map));
    };

    let mut result = Vec::new();
    for tool in tools {
        let tool_type = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match tool_type {
            "function" => result.push(parse_function_tool(&tool)?),
            "namespace" => {
                let ns_tools = parse_namespace_tools(&tool)?;
                let namespace = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(child_tools) = tool.get("tools").and_then(|v| v.as_array()) {
                    for child in child_tools {
                        if let Some(name) = child.get("name").and_then(|v| v.as_str()) {
                            if !namespace.is_empty() {
                                ns_map.insert(name.to_string(), format!("{namespace}__{name}"));
                            }
                        }
                    }
                }
                result.extend(ns_tools);
            }
            _ => {}
        }
    }

    Ok((
        if result.is_empty() {
            None
        } else {
            Some(result)
        },
        ns_map,
    ))
}

fn parse_function_tool(tool: &serde_json::Value) -> Result<ClaudeTool, ConvertError> {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool name".into()))?
        .to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let input_schema = tool
        .get("parameters")
        .cloned()
        .unwrap_or(serde_json::json!({"type": "object"}));
    Ok(ClaudeTool {
        name,
        description,
        input_schema,
    })
}

fn parse_namespace_tools(tool: &serde_json::Value) -> Result<Vec<ClaudeTool>, ConvertError> {
    let namespace = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let ns_description = tool.get("description").and_then(|v| v.as_str());
    let child_tools = tool
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut defs = Vec::new();
    for child in &child_tools {
        let child_type = child.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if child_type != "function" {
            continue;
        }
        let child_name = child.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let qualified_name = if namespace.is_empty() {
            child_name.to_string()
        } else {
            format!("{namespace}__{child_name}")
        };
        let description = child
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| ns_description.map(String::from));
        let input_schema = child
            .get("parameters")
            .cloned()
            .unwrap_or(serde_json::json!({"type": "object"}));
        defs.push(ClaudeTool {
            name: qualified_name,
            description,
            input_schema,
        });
    }
    Ok(defs)
}

fn resp_tool_choice_to_claude(
    value: &serde_json::Value,
    ns_names: &std::collections::HashMap<String, String>,
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
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| ConvertError::MissingField("tool_choice.name".into()))?;
        let qualified = ns_names.get(name).map(|s| s.as_str()).unwrap_or(name);
        return Ok(serde_json::json!({"type": "tool", "name": qualified}));
    }

    Err(ConvertError::InvalidField {
        field: "tool_choice".into(),
        reason: "expected string or function object".into(),
    })
}

fn output_to_claude_content(
    output: &[serde_json::Value],
) -> Result<Vec<ClaudeContentBlock>, ConvertError> {
    let mut content = Vec::new();
    for item in output {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            ITEM_TYPE_MESSAGE => {
                if let Some(arr) = item.get("content").and_then(|v| v.as_array()) {
                    for part in arr {
                        if part.get("type").and_then(|v| v.as_str()) == Some(ITEM_TYPE_OUTPUT_TEXT)
                        {
                            let text = part
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !text.is_empty() {
                                content.push(ClaudeContentBlock::Text { text });
                            }
                        }
                    }
                }
            }
            ITEM_TYPE_FUNCTION_CALL => {
                let (call_id, name, input) = parse_function_call_fields(item)?;
                content.push(ClaudeContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                });
            }
            _ => {}
        }
    }
    Ok(content)
}

fn map_reasoning_to_thinking(
    reasoning: &Option<serde_json::Value>,
    max_tokens: u32,
) -> Option<ThinkingConfig> {
    let reasoning = reasoning.as_ref()?;
    let effort = reasoning
        .get("effort")
        .and_then(|v| v.as_str())
        .unwrap_or("medium");
    if effort == "none" {
        return None;
    }
    let budget = match effort {
        "low" => 1024,
        "high" => max_tokens.max(4096),
        _ => max_tokens.max(2048),
    };
    Some(ThinkingConfig {
        r#type: "enabled".into(),
        budget_tokens: budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reasoning_effort_high_maps_to_thinking_enabled() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "o1",
            "input": "hello",
            "reasoning": {"effort": "high"},
            "max_output_tokens": 8192
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_some());
        let tc = req.thinking.unwrap();
        assert_eq!(tc.r#type, "enabled");
        assert!(tc.budget_tokens >= 4096);
    }

    #[test]
    fn test_reasoning_effort_none_maps_to_no_thinking() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "o1",
            "input": "hello",
            "reasoning": {"effort": "none"}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_none());
    }

    #[test]
    fn test_no_reasoning_field_maps_to_no_thinking() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "o1",
            "input": "hello"
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        assert!(req.thinking.is_none());
    }

    #[test]
    fn test_namespace_tool_choice_qualifies_name() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "input": "use shell",
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp",
                    "tools": [
                        {"type": "function", "name": "shell", "parameters": {}}
                    ]
                }
            ],
            "tool_choice": {"type": "function", "name": "shell"}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: ClaudeRequest = serde_json::from_slice(&result).unwrap();

        let tc = req.tool_choice.unwrap();
        let name = tc.get("name").and_then(|v| v.as_str()).unwrap();
        assert_eq!(name, "mcp__shell");
    }
}
