use crate::converters::FormatConverter;
use crate::converters::shared::{extract_function_response_text, new_chat_id, now_unix_secs};
use crate::error::ConvertError;
use crate::formats::gemini::*;
use crate::formats::openai_chat::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: GeminiRequest = serde_json::from_slice(input)?;

        let mut messages = Vec::new();
        if let Some(system) = &req.system_instruction {
            let text = system
                .parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                messages.push(OpenAIChatMessage {
                    role: "system".into(),
                    content: Some(MessageContent::Text(text)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }
        }

        messages.extend(gemini_contents_to_chat_messages(req.contents)?);

        let tools = req.tools.map(|tool_groups| {
            tool_groups
                .into_iter()
                .flat_map(|g| g.function_declarations)
                .map(|fd| OpenAIChatTool {
                    r#type: "function".into(),
                    function: FunctionDef {
                        name: fd.name,
                        description: fd.description,
                        parameters: fd.parameters,
                        strict: None,
                    },
                })
                .collect()
        });

        let tool_choice = req
            .tool_config
            .as_ref()
            .map(gemini_tool_config_to_chat)
            .transpose()?;

        let generation_config = req.generation_config.as_ref();
        let stop = generation_config
            .and_then(|g| g.stop_sequences.as_ref())
            .and_then(|seq| match seq.len() {
                0 => None,
                1 => Some(StopValue::Single(seq[0].clone())),
                _ => Some(StopValue::Multiple(seq.clone())),
            });

        let out = OpenAIChatRequest {
            model: req.model.unwrap_or_default(),
            messages,
            temperature: generation_config.and_then(|g| g.temperature),
            top_p: generation_config.and_then(|g| g.top_p),
            max_completion_tokens: generation_config.and_then(|g| g.max_output_tokens),
            max_tokens: None,
            stop,
            seed: generation_config.and_then(|g| g.seed),
            stream: None,
            stream_options: None,
            tools,
            tool_choice,
            response_format: None,
            n: None,
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: GeminiResponse = serde_json::from_slice(input)?;

        let candidate = resp
            .candidates
            .first()
            .ok_or_else(|| ConvertError::MissingField("candidates".into()))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for part in &candidate.content.parts {
            if let Some(text) = &part.text {
                if !text.is_empty() {
                    text_parts.push(text.clone());
                }
            }
            if let Some(fc) = &part.function_call {
                tool_calls.push(ToolCall {
                    id: fc
                        .id
                        .clone()
                        .unwrap_or_else(crate::converters::shared::new_uuid),
                    r#type: "function".into(),
                    function: FunctionCall {
                        name: fc.name.clone(),
                        arguments: serde_json::to_string(&fc.args)?,
                    },
                });
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("MAX_TOKENS") => "length",
            _ => "stop",
        };

        let usage_meta = resp.usage_metadata.unwrap_or(GeminiUsageMetadata {
            prompt_token_count: None,
            candidates_token_count: None,
            total_token_count: None,
        });
        let prompt_tokens = usage_meta.prompt_token_count.unwrap_or(0);
        let completion_tokens = usage_meta.candidates_token_count.unwrap_or(0);

        let out = OpenAIChatResponse {
            id: new_chat_id(),
            object: "chat.completion".into(),
            created: now_unix_secs(),
            model: resp.model_version.unwrap_or_default(),
            choices: vec![Choice {
                index: candidate.index.unwrap_or(0),
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
            usage: Some(OpenAIUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: usage_meta
                    .total_token_count
                    .unwrap_or(prompt_tokens + completion_tokens),
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
            crate::formats::gemini::GeminiStreamAdapter::parse_sse_event(event, state_in)?;
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

fn gemini_contents_to_chat_messages(
    contents: Vec<GeminiContent>,
) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    let mut messages = Vec::new();
    for content in contents {
        match content.role.as_deref() {
            Some("user") | None => {
                messages.extend(gemini_user_to_chat(&content.parts)?);
            }
            Some("model") => {
                messages.push(gemini_model_to_chat_assistant(&content.parts)?);
            }
            Some(other) => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }
    Ok(messages)
}

fn gemini_user_to_chat(parts: &[GeminiPart]) -> Result<Vec<OpenAIChatMessage>, ConvertError> {
    let mut messages = Vec::new();
    let mut user_parts = Vec::new();

    for part in parts {
        if let Some(fr) = &part.function_response {
            messages.push(OpenAIChatMessage {
                role: "tool".into(),
                content: Some(MessageContent::Text(extract_function_response_text(
                    &fr.response,
                ))),
                name: None,
                tool_calls: None,
                tool_call_id: fr.id.clone(),
                reasoning_content: None,
            });
        } else if let Some(text) = &part.text {
            if !text.is_empty() {
                user_parts.push(ContentPart::Text { text: text.clone() });
            }
        } else if let Some(inline) = &part.inline_data {
            let url = format!("data:{};base64,{}", inline.mime_type, inline.data);
            user_parts.push(ContentPart::ImageUrl {
                image_url: ImageUrlDetail { url, detail: None },
            });
        }
    }

    if !user_parts.is_empty() {
        let content = if user_parts.len() == 1 {
            match &user_parts[0] {
                ContentPart::Text { text } => Some(MessageContent::Text(text.clone())),
                ContentPart::ImageUrl { .. } => Some(MessageContent::Parts(user_parts)),
            }
        } else if user_parts
            .iter()
            .all(|p| matches!(p, ContentPart::Text { .. }))
        {
            let text = user_parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            Some(MessageContent::Text(text))
        } else {
            Some(MessageContent::Parts(user_parts))
        };

        messages.insert(
            0,
            OpenAIChatMessage {
                role: "user".into(),
                content,
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        );
    }

    Ok(messages)
}

fn gemini_model_to_chat_assistant(parts: &[GeminiPart]) -> Result<OpenAIChatMessage, ConvertError> {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for part in parts {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                text_parts.push(text.clone());
            }
        }
        if let Some(fc) = &part.function_call {
            tool_calls.push(ToolCall {
                id: fc
                    .id
                    .clone()
                    .unwrap_or_else(crate::converters::shared::new_uuid),
                r#type: "function".into(),
                function: FunctionCall {
                    name: fc.name.clone(),
                    arguments: serde_json::to_string(&fc.args)?,
                },
            });
        }
    }

    Ok(OpenAIChatMessage {
        role: "assistant".into(),
        content: if text_parts.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text_parts.join("")))
        },
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        reasoning_content: None,
    })
}

fn gemini_tool_config_to_chat(
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
        return Ok(serde_json::json!({
            "type": "function",
            "function": { "name": allowed[0] }
        }));
    }

    match mode {
        "AUTO" | "VALIDATED" => Ok(serde_json::json!("auto")),
        "NONE" => Ok(serde_json::json!("none")),
        "ANY" => Ok(serde_json::json!("required")),
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
        let req: OpenAIChatRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert!(matches!(
            &req.messages[0].content,
            Some(MessageContent::Text(text)) if text == "hello"
        ));
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
        let resp: OpenAIChatResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.choices[0].message.content.as_deref(), Some("hi"));
    }
}
