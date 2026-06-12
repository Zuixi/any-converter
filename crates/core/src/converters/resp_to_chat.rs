use crate::converters::FormatConverter;
use crate::converters::shared::{extract_message_text, new_chat_id, now_unix_secs, *};
use crate::error::ConvertError;
use crate::formats::openai_chat::*;
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
        use crate::formats::openai_chat::OpenAIChatStreamAdapter;
        use crate::formats::openai_resp::OpenAIResponsesStreamAdapter;

        let canonical = OpenAIResponsesStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(OpenAIChatStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIResponsesRequest) -> Result<OpenAIChatRequest, ConvertError> {
    let mut system_parts: Vec<String> = Vec::new();
    let input_messages = parse_input_to_messages(req.input.as_ref(), &mut system_parts)?;

    if let Some(instructions) = &req.instructions {
        system_parts.push(instructions.clone());
    }

    let mut messages = Vec::new();
    if !system_parts.is_empty() {
        messages.push(OpenAIChatMessage {
            role: "system".into(),
            content: Some(MessageContent::Text(system_parts.join("\n\n"))),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }
    messages.extend(input_messages);

    let (tools, ns_names) = parse_resp_tools_with_ns(req.tools)?;
    let max_completion_tokens = req.max_output_tokens;

    let tool_choice = qualify_tool_choice(req.tool_choice, &ns_names);

    Ok(OpenAIChatRequest {
        model: req.model,
        messages,
        temperature: req.temperature,
        top_p: req.top_p,
        max_completion_tokens,
        max_tokens: None,
        stop: None,
        seed: None,
        stream: req.stream,
        stream_options: None,
        tools,
        tool_choice,
        response_format: None,
        n: None,
    })
}

fn convert_response(resp: OpenAIResponsesResponse) -> Result<OpenAIChatResponse, ConvertError> {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for item in &resp.output {
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
                                text_parts.push(text);
                            }
                        }
                    }
                }
            }
            ITEM_TYPE_FUNCTION_CALL => {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("function_call call_id".into()))?
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ConvertError::MissingField("function_call name".into()))?
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}")
                    .to_string();
                tool_calls.push(ToolCall {
                    id: call_id,
                    r#type: "function".into(),
                    function: FunctionCall { name, arguments },
                });
            }
            _ => {}
        }
    }

    let content = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    let finish_reason = match resp.status.as_str() {
        "completed" => "stop",
        "incomplete" => "length",
        _ => "stop",
    };

    let usage = resp.usage.map(|u| {
        let total = u.total_tokens.unwrap_or(u.input_tokens + u.output_tokens);
        OpenAIUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: total,
        }
    });

    Ok(OpenAIChatResponse {
        id: if resp.id.is_empty() {
            new_chat_id()
        } else {
            resp.id.clone()
        },
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
                reasoning_content: None,
            },
            finish_reason: Some(finish_reason.into()),
        }],
        usage,
        system_fingerprint: None,
    })
}

fn parse_input_to_messages(
    input: Option<&serde_json::Value>,
    system_parts: &mut Vec<String>,
) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    let Some(input) = input else {
        return Ok(vec![]);
    };

    if let Some(text) = input.as_str() {
        return Ok(vec![OpenAIChatMessage {
            role: "user".into(),
            content: Some(MessageContent::Text(text.to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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

    Ok(merge_chat_messages(messages))
}

fn parse_input_item(item: &serde_json::Value) -> Result<OpenAIChatMessage, ConvertError> {
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
            let parts = item
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
                "user" => Ok(OpenAIChatMessage {
                    role: "user".into(),
                    content: parts_to_message_content(&parts)?,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                }),
                "assistant" => {
                    let (text_parts, _) = split_text_and_images(&parts)?;
                    Ok(OpenAIChatMessage {
                        role: "assistant".into(),
                        content: parts_to_message_content(&text_parts)?,
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    })
                }
                other => Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                }),
            }
        }
        ITEM_TYPE_FUNCTION_CALL => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("function_call call_id".into()))?
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("function_call name".into()))?
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}")
                .to_string();
            Ok(OpenAIChatMessage {
                role: "assistant".into(),
                content: None,
                name: None,
                tool_calls: Some(vec![ToolCall {
                    id: call_id,
                    r#type: "function".into(),
                    function: FunctionCall { name, arguments },
                }]),
                tool_call_id: None,
                reasoning_content: None,
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
            Ok(OpenAIChatMessage {
                role: "tool".into(),
                content: Some(MessageContent::Text(output)),
                name: None,
                tool_calls: None,
                tool_call_id: Some(call_id),
                reasoning_content: None,
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
        ITEM_TYPE_INPUT_IMAGE => {
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
                "url": url,
            }))
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn split_text_and_images(
    parts: &[serde_json::Value],
) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), ConvertError> {
    let mut text_parts = Vec::new();
    let mut image_parts = Vec::new();
    for part in parts {
        if part.get("type").and_then(|v| v.as_str()) == Some("image") {
            image_parts.push(part.clone());
        } else {
            text_parts.push(part.clone());
        }
    }
    Ok((text_parts, image_parts))
}

fn parts_to_message_content(
    parts: &[serde_json::Value],
) -> Result<Option<MessageContent>, ConvertError> {
    if parts.is_empty() {
        return Ok(None);
    }

    let has_image = parts
        .iter()
        .any(|p| p.get("type").and_then(|v| v.as_str()) == Some("image"));
    if has_image {
        let mut content_parts = Vec::new();
        for part in parts {
            match part.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    let text = part
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    content_parts.push(ContentPart::Text { text });
                }
                Some("image") => {
                    let url = part
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    content_parts.push(ContentPart::ImageUrl {
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
        Ok(Some(MessageContent::Parts(content_parts)))
    } else {
        let text: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect();
        Ok(if text.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text))
        })
    }
}

fn parse_resp_tools_with_ns(
    tools: Option<Vec<serde_json::Value>>,
) -> Result<
    (
        Option<Vec<OpenAIChatTool>>,
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
            "function" => result.push(parse_function_chat_tool(&tool)?),
            "namespace" => {
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
                result.extend(parse_namespace_chat_tools(&tool)?);
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

fn qualify_tool_choice(
    tool_choice: Option<serde_json::Value>,
    ns_names: &std::collections::HashMap<String, String>,
) -> Option<serde_json::Value> {
    let mut tc = tool_choice?;
    if ns_names.is_empty() {
        return Some(tc);
    }
    if let Some(name) = tc.get("name").and_then(|v| v.as_str()).map(String::from) {
        if let Some(qualified) = ns_names.get(&name) {
            tc.as_object_mut()
                .map(|obj| obj.insert("name".into(), serde_json::json!(qualified)));
        }
    }
    if let Some(func) = tc.get("function").cloned() {
        if let Some(name) = func.get("name").and_then(|v| v.as_str()).map(String::from) {
            if let Some(qualified) = ns_names.get(&name) {
                tc.as_object_mut().map(|obj| {
                    obj.insert("function".into(), serde_json::json!({"name": qualified}))
                });
            }
        }
    }
    Some(tc)
}

fn parse_function_chat_tool(tool: &serde_json::Value) -> Result<OpenAIChatTool, ConvertError> {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool name".into()))?
        .to_string();
    Ok(OpenAIChatTool {
        r#type: "function".into(),
        function: FunctionDef {
            name,
            description: tool
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            parameters: tool.get("parameters").cloned(),
            strict: tool.get("strict").and_then(|v| v.as_bool()),
        },
    })
}

fn parse_namespace_chat_tools(
    tool: &serde_json::Value,
) -> Result<Vec<OpenAIChatTool>, ConvertError> {
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
        defs.push(OpenAIChatTool {
            r#type: "function".into(),
            function: FunctionDef {
                name: qualified_name,
                description,
                parameters: child.get("parameters").cloned(),
                strict: child.get("strict").and_then(|v| v.as_bool()),
            },
        });
    }
    Ok(defs)
}

fn merge_chat_messages(messages: Vec<OpenAIChatMessage>) -> Vec<OpenAIChatMessage> {
    let mut merged: Vec<OpenAIChatMessage> = Vec::with_capacity(messages.len());
    for msg in messages {
        if let Some(last) = merged.last_mut() {
            if last.role == msg.role && msg.role == "assistant" {
                merge_assistant_messages(last, &msg);
                continue;
            }
        }
        merged.push(msg);
    }
    merged
}

fn merge_assistant_messages(last: &mut OpenAIChatMessage, msg: &OpenAIChatMessage) {
    if let Some(content) = &msg.content {
        match &last.content {
            None => last.content = Some(content.clone()),
            Some(MessageContent::Text(existing)) => {
                if let MessageContent::Text(new_text) = content {
                    last.content = Some(MessageContent::Text(format!("{existing}{new_text}")));
                } else {
                    last.content = Some(content.clone());
                }
            }
            Some(MessageContent::Parts(_)) => {
                last.content = Some(content.clone());
            }
        }
    }

    if let Some(tool_calls) = &msg.tool_calls {
        if let Some(existing) = &mut last.tool_calls {
            existing.extend(tool_calls.iter().cloned());
        } else {
            last.tool_calls = Some(tool_calls.clone());
        }
    }

    if msg.reasoning_content.is_some() {
        last.reasoning_content = msg.reasoning_content.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_tool_choice_qualifies_name_in_chat() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4.1",
            "input": "use shell",
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp",
                    "tools": [
                        {"type": "function", "name": "exec", "parameters": {}}
                    ]
                }
            ],
            "tool_choice": {"type": "function", "function": {"name": "exec"}}
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        let tc = req.tool_choice.unwrap();
        let name = tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap();
        assert_eq!(name, "mcp__exec");
    }

    #[test]
    fn test_tool_choice_passthrough_without_namespace() {
        let req_bytes = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4.1",
            "input": "hello",
            "tools": [
                {"type": "function", "name": "search", "parameters": {}}
            ],
            "tool_choice": "auto"
        }))
        .unwrap();
        let converter = Converter;
        let result = converter.convert_request(&req_bytes).unwrap();
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        let tc = req.tool_choice.unwrap();
        assert_eq!(tc.as_str().unwrap(), "auto");
    }
}
