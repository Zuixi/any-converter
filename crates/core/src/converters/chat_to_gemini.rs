use crate::converters::FormatConverter;
use crate::converters::shared::*;
use crate::error::ConvertError;
use crate::formats::gemini::*;
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
        use crate::formats::gemini::GeminiStreamAdapter;
        use crate::formats::openai_chat::OpenAIChatStreamAdapter;

        let canonical = OpenAIChatStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(GeminiStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIChatRequest) -> Result<GeminiRequest, ConvertError> {
    let generation_config = build_generation_config(&req);
    let mut system_parts: Vec<String> = Vec::new();
    let mut contents: Vec<GeminiContent> = Vec::new();

    for msg in req.messages {
        match msg.role.as_str() {
            "system" | "developer" => {
                if let Some(text) = message_content_as_text(msg.content) {
                    system_parts.push(text);
                }
            }
            "user" | "assistant" | "tool" => {
                contents.push(chat_message_to_gemini(msg)?);
            }
            other => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart::text(system_parts.join("\n"))],
        })
    };

    let tool_config = req
        .tool_choice
        .as_ref()
        .and_then(chat_tool_choice_to_gemini);

    let tools = req.tools.map(|tools| {
        vec![GeminiToolDeclaration {
            function_declarations: tools
                .into_iter()
                .map(|t| GeminiFunctionDeclaration {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: t.function.parameters,
                })
                .collect(),
        }]
    });

    Ok(GeminiRequest {
        model: Some(req.model),
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    })
}

fn convert_response(resp: OpenAIChatResponse) -> Result<GeminiResponse, ConvertError> {
    let choice = resp
        .choices
        .first()
        .ok_or_else(|| ConvertError::MissingField("choices".into()))?;

    let mut parts: Vec<GeminiPart> = Vec::new();

    if let Some(text) = &choice.message.content {
        if !text.is_empty() {
            parts.push(GeminiPart::text(text.clone()));
        }
    }

    if let Some(reasoning) = &choice.message.reasoning_content {
        if !reasoning.is_empty() {
            parts.push(GeminiPart::text(reasoning.clone()));
        }
    }

    if let Some(tool_calls) = &choice.message.tool_calls {
        for tc in tool_calls {
            parts.push(GeminiPart::function_call_with_id(
                tc.function.name.clone(),
                parse_arguments(&tc.function.arguments),
                tc.id.clone(),
            ));
        }
    }

    let finish_reason = match choice.finish_reason.as_deref() {
        Some("stop") => "STOP",
        Some("length") => "MAX_TOKENS",
        Some("tool_calls") => "STOP",
        _ => "STOP",
    };

    let usage = resp.usage.map(|u| GeminiUsageMetadata {
        prompt_token_count: Some(u.prompt_tokens),
        candidates_token_count: Some(u.completion_tokens),
        total_token_count: Some(u.total_tokens),
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
        usage_metadata: usage,
        model_version: Some(resp.model),
    })
}

fn chat_message_to_gemini(msg: OpenAIChatMessage) -> Result<GeminiContent, ConvertError> {
    match msg.role.as_str() {
        "user" => Ok(GeminiContent {
            role: Some("user".into()),
            parts: user_content_to_gemini_parts(msg.content)?,
        }),
        "assistant" => {
            let mut parts = Vec::new();

            if let Some(reasoning) = msg.reasoning_content {
                if !reasoning.is_empty() {
                    parts.push(GeminiPart::text(reasoning));
                }
            }

            parts.extend(user_content_to_gemini_parts(msg.content)?);

            if let Some(tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    parts.push(GeminiPart::function_call_with_id(
                        tc.function.name,
                        parse_arguments(&tc.function.arguments),
                        tc.id,
                    ));
                }
            }

            Ok(GeminiContent {
                role: Some("model".into()),
                parts,
            })
        }
        "tool" => {
            let tool_call_id = msg
                .tool_call_id
                .ok_or_else(|| ConvertError::MissingField("tool_call_id".into()))?;
            let text = message_content_as_text(msg.content).unwrap_or_default();
            Ok(GeminiContent {
                role: Some("user".into()),
                parts: vec![GeminiPart::function_response_with_id(
                    "function",
                    serde_json::json!({"result": text}),
                    tool_call_id,
                )],
            })
        }
        other => Err(ConvertError::InvalidField {
            field: "role".into(),
            reason: format!("unsupported role: {other}"),
        }),
    }
}

fn user_content_to_gemini_parts(
    content: Option<MessageContent>,
) -> Result<Vec<GeminiPart>, ConvertError> {
    match content {
        None => Ok(vec![]),
        Some(MessageContent::Text(text)) => {
            if text.is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![GeminiPart::text(text)])
            }
        }
        Some(MessageContent::Parts(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        out.push(GeminiPart::text(text));
                    }
                    ContentPart::ImageUrl { image_url } => {
                        out.push(image_url_to_gemini_part(&image_url));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn image_url_to_gemini_part(image_url: &ImageUrlDetail) -> GeminiPart {
    if let Some((mime_type, data)) = parse_data_url(&image_url.url) {
        GeminiPart::inline_data(mime_type, data)
    } else {
        GeminiPart::text(image_url.url.clone())
    }
}

fn build_generation_config(req: &OpenAIChatRequest) -> Option<GeminiGenerationConfig> {
    let stop_sequences = req.stop.as_ref().map(stop_to_sequences);
    let max_output_tokens = req.max_completion_tokens.or(req.max_tokens);

    if req.temperature.is_none()
        && req.top_p.is_none()
        && max_output_tokens.is_none()
        && stop_sequences.is_none()
        && req.seed.is_none()
    {
        return None;
    }

    Some(GeminiGenerationConfig {
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        max_output_tokens,
        stop_sequences,
        seed: req.seed,
        response_mime_type: None,
        response_schema: None,
    })
}

fn chat_tool_choice_to_gemini(value: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(s) = value.as_str() {
        return Some(match s {
            "auto" => serde_json::json!({"functionCallingConfig": {"mode": "AUTO"}}),
            "none" => serde_json::json!({"functionCallingConfig": {"mode": "NONE"}}),
            "required" => serde_json::json!({"functionCallingConfig": {"mode": "ANY"}}),
            _ => return None,
        });
    }

    if value.get("type").and_then(|v| v.as_str()) == Some("function") {
        let name = value
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converters::FormatConverter;

    #[test]
    fn test_convert_request_simple_user_message() {
        let input = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4.1",
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
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1700000000u64,
            "model": "gpt-4.1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
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
