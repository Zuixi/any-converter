use crate::converters::FormatConverter;
use crate::converters::shared::{
    extract_message_text, parse_data_url, parse_function_call_fields, *,
};
use crate::error::ConvertError;
use crate::formats::gemini::*;
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
        use crate::formats::gemini::GeminiStreamAdapter;
        use crate::formats::openai_resp::OpenAIResponsesStreamAdapter;

        let canonical = OpenAIResponsesStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(GeminiStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIResponsesRequest) -> Result<GeminiRequest, ConvertError> {
    let mut system_parts: Vec<String> = Vec::new();
    let contents = parse_input_to_contents(req.input.as_ref(), &mut system_parts)?;

    if let Some(instructions) = &req.instructions {
        system_parts.push(instructions.clone());
    }

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart::text(system_parts.join("\n\n"))],
        })
    };

    let generation_config = build_generation_config(&req);
    let tools = parse_resp_tools(req.tools)?;
    let tool_config = req
        .tool_choice
        .as_ref()
        .and_then(resp_tool_choice_to_gemini);

    Ok(GeminiRequest {
        model: Some(req.model),
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    })
}

fn convert_response(resp: OpenAIResponsesResponse) -> Result<GeminiResponse, ConvertError> {
    let parts = output_to_gemini_parts(&resp.output)?;

    let finish_reason = match resp.status.as_str() {
        "completed" => "STOP",
        "incomplete" => "MAX_TOKENS",
        _ => "STOP",
    };

    let usage_metadata = resp.usage.map(|u| {
        let total = u.total_tokens.unwrap_or(u.input_tokens + u.output_tokens);
        GeminiUsageMetadata {
            prompt_token_count: Some(u.input_tokens),
            candidates_token_count: Some(u.output_tokens),
            total_token_count: Some(total),
        }
    });

    Ok(GeminiResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".into()),
                parts,
            },
            finish_reason: Some(finish_reason.into()),
            index: Some(0),
        }],
        usage_metadata,
        model_version: Some(resp.model),
    })
}

fn parse_input_to_contents(
    input: Option<&serde_json::Value>,
    system_parts: &mut Vec<String>,
) -> Result<Vec<GeminiContent>, ConvertError> {
    let Some(input) = input else {
        return Ok(vec![]);
    };

    if let Some(text) = input.as_str() {
        return Ok(vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart::text(text.to_string())],
        }]);
    }

    let items = input.as_array().ok_or_else(|| ConvertError::InvalidField {
        field: "input".into(),
        reason: "expected string or array".into(),
    })?;

    let mut contents = Vec::new();
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

        contents.push(parse_input_item(item)?);
    }

    Ok(contents)
}

fn parse_input_item(item: &serde_json::Value) -> Result<GeminiContent, ConvertError> {
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

            let role = match role_str {
                "user" => Some("user".into()),
                "assistant" => Some("model".into()),
                other => {
                    return Err(ConvertError::InvalidField {
                        field: "role".into(),
                        reason: format!("unsupported role: {other}"),
                    });
                }
            };

            Ok(GeminiContent { role, parts })
        }
        ITEM_TYPE_FUNCTION_CALL => {
            let (call_id, name, input) = parse_function_call_fields(item)?;
            Ok(GeminiContent {
                role: Some("model".into()),
                parts: vec![GeminiPart::function_call_with_id(name, input, call_id)],
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
            Ok(GeminiContent {
                role: Some("user".into()),
                parts: vec![GeminiPart::function_response_with_id(
                    "function",
                    serde_json::json!({"result": output}),
                    call_id,
                )],
            })
        }
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn parse_message_content_part(part: &serde_json::Value) -> Result<GeminiPart, ConvertError> {
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
            Ok(GeminiPart::text(text))
        }
        ITEM_TYPE_INPUT_IMAGE => input_image_to_gemini_part(part),
        other => Err(ConvertError::UnsupportedContentType(other.to_string())),
    }
}

fn input_image_to_gemini_part(part: &serde_json::Value) -> Result<GeminiPart, ConvertError> {
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

    if let Some((mime_type, data)) = parse_data_url(&url) {
        Ok(GeminiPart::inline_data(mime_type, data))
    } else {
        Ok(GeminiPart::text(url))
    }
}

fn parse_resp_tools(
    tools: Option<Vec<serde_json::Value>>,
) -> Result<Option<Vec<GeminiToolDeclaration>>, ConvertError> {
    let Some(tools) = tools else {
        return Ok(None);
    };

    let mut declarations = Vec::new();
    for tool in tools {
        let tool_type = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match tool_type {
            "function" => declarations.push(parse_function_declaration(&tool)?),
            "namespace" => declarations.extend(parse_namespace_declarations(&tool)?),
            _ => {}
        }
    }

    Ok(if declarations.is_empty() {
        None
    } else {
        Some(vec![GeminiToolDeclaration {
            function_declarations: declarations,
        }])
    })
}

fn parse_function_declaration(
    tool: &serde_json::Value,
) -> Result<GeminiFunctionDeclaration, ConvertError> {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool name".into()))?
        .to_string();
    Ok(GeminiFunctionDeclaration {
        name,
        description: tool
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        parameters: tool.get("parameters").cloned(),
    })
}

fn parse_namespace_declarations(
    tool: &serde_json::Value,
) -> Result<Vec<GeminiFunctionDeclaration>, ConvertError> {
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
        defs.push(GeminiFunctionDeclaration {
            name: qualified_name,
            description,
            parameters: child.get("parameters").cloned(),
        });
    }
    Ok(defs)
}

fn resp_tool_choice_to_gemini(value: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(s) = value.as_str() {
        return Some(match s {
            "auto" => serde_json::json!({"functionCallingConfig": {"mode": "AUTO"}}),
            "none" => serde_json::json!({"functionCallingConfig": {"mode": "NONE"}}),
            "required" => serde_json::json!({"functionCallingConfig": {"mode": "ANY"}}),
            _ => return None,
        });
    }

    if value.get("type").and_then(|v| v.as_str()) == Some("function") {
        let name = value.get("name").and_then(|n| n.as_str());
        if let Some(name) = name {
            return Some(serde_json::json!({
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": [name]
                }
            }));
        }
    }

    None
}

fn build_generation_config(req: &OpenAIResponsesRequest) -> Option<GeminiGenerationConfig> {
    if req.temperature.is_none() && req.top_p.is_none() && req.max_output_tokens.is_none() {
        return None;
    }

    Some(GeminiGenerationConfig {
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        max_output_tokens: req.max_output_tokens,
        stop_sequences: None,
        seed: None,
        response_mime_type: None,
        response_schema: None,
    })
}

fn output_to_gemini_parts(output: &[serde_json::Value]) -> Result<Vec<GeminiPart>, ConvertError> {
    let mut parts = Vec::new();
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
                                parts.push(GeminiPart::text(text));
                            }
                        }
                    }
                }
            }
            ITEM_TYPE_FUNCTION_CALL => {
                let (call_id, name, input) = parse_function_call_fields(item)?;
                parts.push(GeminiPart::function_call_with_id(name, input, call_id));
            }
            _ => {}
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converters::FormatConverter;

    #[test]
    fn test_convert_request_simple_user_message() {
        let input = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4.1",
            "input": "hello"
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
            "id": "resp_1",
            "object": "response",
            "created_at": 1700000000u64,
            "model": "gpt-4.1",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
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
