use crate::converters::FormatConverter;
use crate::converters::shared::{extract_function_response_text, new_resp_id, now_unix_secs, *};
use crate::error::ConvertError;
use crate::formats::gemini::*;
use crate::formats::openai_resp::*;
use crate::ir::StreamState;
use crate::sse::SseEvent;

pub(super) struct Converter;

impl FormatConverter for Converter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let req: GeminiRequest = serde_json::from_slice(input)?;

        let instructions = req.system_instruction.as_ref().map(|content| {
            content
                .parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("")
        });

        let input_items = gemini_contents_to_resp_input(req.contents)?;

        let tools = req.tools.map(|tool_groups| {
            tool_groups
                .into_iter()
                .flat_map(|g| g.function_declarations)
                .map(|fd| {
                    serde_json::json!({
                        "type": "function",
                        "name": fd.name,
                        "description": fd.description,
                        "parameters": fd.parameters,
                    })
                })
                .collect()
        });

        let tool_choice = req
            .tool_config
            .as_ref()
            .map(gemini_tool_config_to_resp)
            .transpose()?;

        let generation_config = req.generation_config.as_ref();

        let out = OpenAIResponsesRequest {
            model: req.model.unwrap_or_default(),
            input: Some(serde_json::Value::Array(input_items)),
            instructions,
            max_output_tokens: generation_config.and_then(|g| g.max_output_tokens),
            temperature: generation_config.and_then(|g| g.temperature),
            top_p: generation_config.and_then(|g| g.top_p),
            stream: None,
            tools,
            tool_choice,
            text: None,
            reasoning: None,
            previous_response_id: None,
            store: None,
            extra: Default::default(),
        };

        Ok(serde_json::to_vec(&out)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        let resp: GeminiResponse = serde_json::from_slice(input)?;

        let candidate = resp
            .candidates
            .first()
            .ok_or_else(|| ConvertError::MissingField("candidates".into()))?;

        let output = gemini_parts_to_resp_output(&candidate.content.parts)?;

        let status = match candidate.finish_reason.as_deref() {
            Some("MAX_TOKENS") => "incomplete",
            _ => "completed",
        };

        let usage_meta = resp.usage_metadata.unwrap_or(GeminiUsageMetadata {
            prompt_token_count: None,
            candidates_token_count: None,
            total_token_count: None,
        });
        let input_tokens = usage_meta.prompt_token_count.unwrap_or(0);
        let output_tokens = usage_meta.candidates_token_count.unwrap_or(0);

        let out = OpenAIResponsesResponse {
            id: new_resp_id(),
            object: "response".into(),
            created_at: now_unix_secs(),
            model: resp.model_version.unwrap_or_default(),
            status: status.into(),
            output,
            usage: Some(ResponsesUsage {
                input_tokens,
                output_tokens,
                total_tokens: Some(
                    usage_meta
                        .total_token_count
                        .unwrap_or(input_tokens + output_tokens),
                ),
            }),
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
                crate::formats::openai_resp::OpenAIResponsesStreamAdapter::emit_sse_event(
                    ce, state_out,
                )?,
            );
        }
        Ok(output)
    }
}

fn gemini_contents_to_resp_input(
    contents: Vec<GeminiContent>,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut items = Vec::new();
    for content in contents {
        match content.role.as_deref() {
            Some("user") | None => {
                items.extend(gemini_user_to_resp_input(&content.parts)?);
            }
            Some("model") => {
                items.extend(gemini_model_to_resp_input(&content.parts)?);
            }
            Some(other) => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }
    Ok(items)
}

fn gemini_user_to_resp_input(parts: &[GeminiPart]) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut items = Vec::new();
    let mut message_parts = Vec::new();

    for part in parts {
        if let Some(fr) = &part.function_response {
            items.push(serde_json::json!({
                "type": ITEM_TYPE_FUNCTION_CALL_OUTPUT,
                "call_id": fr.id.clone().unwrap_or_else(crate::converters::shared::new_uuid),
                "output": extract_function_response_text(&fr.response),
            }));
        } else if let Some(text) = &part.text {
            if !text.is_empty() {
                message_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_INPUT_TEXT,
                    "text": text,
                }));
            }
        } else if let Some(inline) = &part.inline_data {
            let url = format!("data:{};base64,{}", inline.mime_type, inline.data);
            message_parts.push(serde_json::json!({
                "type": ITEM_TYPE_INPUT_IMAGE,
                "image_url": url,
            }));
        }
    }

    if !message_parts.is_empty() {
        items.insert(
            0,
            serde_json::json!({
                "type": ITEM_TYPE_MESSAGE,
                "role": "user",
                "content": message_parts,
            }),
        );
    }

    Ok(items)
}

fn gemini_model_to_resp_input(
    parts: &[GeminiPart],
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut items = Vec::new();
    let mut message_parts = Vec::new();

    for part in parts {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                message_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                }));
            }
        }
        if let Some(fc) = &part.function_call {
            items.push(serde_json::json!({
                "type": ITEM_TYPE_FUNCTION_CALL,
                "call_id": fc.id.clone().unwrap_or_else(crate::converters::shared::new_uuid),
                "name": fc.name,
                "arguments": serde_json::to_string(&fc.args).unwrap_or_else(|_| "{}".into()),
            }));
        }
    }

    if !message_parts.is_empty() {
        items.insert(
            0,
            serde_json::json!({
                "type": ITEM_TYPE_MESSAGE,
                "role": "assistant",
                "content": message_parts,
            }),
        );
    }

    Ok(items)
}

fn gemini_parts_to_resp_output(
    parts: &[GeminiPart],
) -> Result<Vec<serde_json::Value>, ConvertError> {
    let mut output = Vec::new();
    let mut text_parts = Vec::new();

    for part in parts {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                text_parts.push(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                }));
            }
        }
        if let Some(fc) = &part.function_call {
            if !text_parts.is_empty() {
                output.push(serde_json::json!({
                    "type": ITEM_TYPE_MESSAGE,
                    "role": "assistant",
                    "content": std::mem::take(&mut text_parts),
                }));
            }
            output.push(serde_json::json!({
                "type": ITEM_TYPE_FUNCTION_CALL,
                "call_id": fc.id.clone().unwrap_or_else(crate::converters::shared::new_uuid),
                "name": fc.name,
                "arguments": serde_json::to_string(&fc.args).unwrap_or_else(|_| "{}".into()),
            }));
        }
    }

    if !text_parts.is_empty() {
        output.push(serde_json::json!({
            "type": ITEM_TYPE_MESSAGE,
            "role": "assistant",
            "content": text_parts,
        }));
    }

    Ok(output)
}

fn gemini_tool_config_to_resp(
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
            "name": allowed[0],
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
        let req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        let input_arr = req.input.as_ref().and_then(|v| v.as_array()).unwrap();
        assert_eq!(input_arr.len(), 1);
        assert_eq!(
            input_arr[0].get("role").and_then(|v| v.as_str()),
            Some("user")
        );
        let content = input_arr[0]
            .get("content")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(
            content[0].get("text").and_then(|v| v.as_str()),
            Some("hello")
        );
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
        let resp: OpenAIResponsesResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.status, "completed");
        let output = &resp.output[0];
        assert_eq!(
            output.get("role").and_then(|v| v.as_str()),
            Some("assistant")
        );
        let content = output.get("content").and_then(|v| v.as_array()).unwrap();
        assert_eq!(content[0].get("text").and_then(|v| v.as_str()), Some("hi"));
    }
}
