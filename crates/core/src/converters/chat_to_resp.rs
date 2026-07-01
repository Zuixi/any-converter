use crate::converters::FormatConverter;
use crate::converters::shared;
use crate::converters::shared::*;
use crate::error::ConvertError;
use crate::formats::openai_chat::*;
use crate::formats::openai_resp::*;
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
        use crate::formats::openai_chat::OpenAIChatStreamAdapter;
        use crate::formats::openai_resp::OpenAIResponsesStreamAdapter;

        let canonical = OpenAIChatStreamAdapter::parse_sse_event(event, state_in)?;
        let mut output = Vec::new();
        for ce in &canonical {
            output.extend(OpenAIResponsesStreamAdapter::emit_sse_event(ce, state_out)?);
        }
        Ok(output)
    }
}

fn convert_request(req: OpenAIChatRequest) -> Result<OpenAIResponsesRequest, ConvertError> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut input_items: Vec<serde_json::Value> = Vec::new();

    for msg in req.messages {
        match msg.role.as_str() {
            "system" | "developer" => {
                if let Some(text) = message_content_as_text(msg.content) {
                    system_parts.push(text);
                }
            }
            "user" => {
                let content_parts = user_message_to_resp_content(msg.content)?;
                if !content_parts.is_empty() {
                    input_items.push(serde_json::json!({
                        "type": ITEM_TYPE_MESSAGE,
                        "role": "user",
                        "content": content_parts,
                    }));
                }
            }
            "assistant" => {
                let text_parts = assistant_text_to_resp_content(msg.content);
                if !text_parts.is_empty() {
                    input_items.push(serde_json::json!({
                        "type": ITEM_TYPE_MESSAGE,
                        "role": "assistant",
                        "content": text_parts,
                    }));
                }
                if let Some(tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        input_items.push(serde_json::json!({
                            "type": ITEM_TYPE_FUNCTION_CALL,
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }));
                    }
                }
            }
            "tool" => {
                let call_id = msg
                    .tool_call_id
                    .ok_or_else(|| ConvertError::MissingField("tool_call_id".into()))?;
                let output = message_content_as_text(msg.content).unwrap_or_default();
                input_items.push(serde_json::json!({
                    "type": ITEM_TYPE_FUNCTION_CALL_OUTPUT,
                    "call_id": call_id,
                    "output": output,
                }));
            }
            other => {
                return Err(ConvertError::InvalidField {
                    field: "role".into(),
                    reason: format!("unsupported role: {other}"),
                });
            }
        }
    }

    let instructions = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };

    let input = if input_items.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(input_items))
    };

    let tools = req.tools.map(|tools| {
        tools
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t
                        .function
                        .parameters
                        .unwrap_or(serde_json::json!({"type": "object"})),
                    "strict": t.function.strict.unwrap_or(true),
                })
            })
            .collect()
    });

    Ok(OpenAIResponsesRequest {
        model: req.model,
        input,
        instructions,
        max_output_tokens: req.max_completion_tokens.or(req.max_tokens),
        temperature: req.temperature,
        top_p: req.top_p,
        stream: req.stream,
        tools,
        tool_choice: req.tool_choice,
        text: None,
        reasoning: None,
        previous_response_id: None,
        store: None,
        extra: Default::default(),
    })
}

fn convert_response(resp: OpenAIChatResponse) -> Result<OpenAIResponsesResponse, ConvertError> {
    let choice = resp
        .choices
        .first()
        .ok_or_else(|| ConvertError::MissingField("choices".into()))?;

    let mut output: Vec<serde_json::Value> = Vec::new();
    let mut text_parts: Vec<serde_json::Value> = Vec::new();

    if let Some(text) = &choice.message.content {
        if !text.is_empty() {
            text_parts.push(serde_json::json!({
                "type": ITEM_TYPE_OUTPUT_TEXT,
                "text": text,
            }));
        }
    }

    if let Some(tool_calls) = &choice.message.tool_calls {
        if !text_parts.is_empty() {
            output.push(serde_json::json!({
                "type": ITEM_TYPE_MESSAGE,
                "role": "assistant",
                "content": text_parts,
            }));
            text_parts = Vec::new();
        }
        for tc in tool_calls {
            output.push(serde_json::json!({
                "type": ITEM_TYPE_FUNCTION_CALL,
                "call_id": tc.id,
                "name": tc.function.name,
                "arguments": tc.function.arguments,
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

    let status = match choice.finish_reason.as_deref() {
        Some("stop") => "completed",
        Some("length") => "incomplete",
        Some("tool_calls") => "completed",
        _ => "completed",
    };

    let usage = resp.usage.map(|u| ResponsesUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        total_tokens: Some(u.total_tokens),
        input_tokens_details: None,
    });

    Ok(OpenAIResponsesResponse {
        id: normalize_id_to_resp(&resp.id),
        object: "response".into(),
        created_at: shared::now_unix_secs(),
        model: resp.model,
        status: status.into(),
        output,
        usage,
    })
}

fn user_message_to_resp_content(
    content: Option<MessageContent>,
) -> Result<Vec<serde_json::Value>, ConvertError> {
    match content {
        None => Ok(vec![]),
        Some(MessageContent::Text(text)) => {
            if text.is_empty() {
                Ok(vec![])
            } else {
                Ok(vec![serde_json::json!({
                    "type": ITEM_TYPE_INPUT_TEXT,
                    "text": text,
                })])
            }
        }
        Some(MessageContent::Parts(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        out.push(serde_json::json!({
                            "type": ITEM_TYPE_INPUT_TEXT,
                            "text": text,
                        }));
                    }
                    ContentPart::ImageUrl { image_url } => {
                        out.push(serde_json::json!({
                            "type": ITEM_TYPE_INPUT_IMAGE,
                            "image_url": image_url.url,
                        }));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn assistant_text_to_resp_content(content: Option<MessageContent>) -> Vec<serde_json::Value> {
    match content {
        None => vec![],
        Some(MessageContent::Text(text)) => {
            if text.is_empty() {
                vec![]
            } else {
                vec![serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                })]
            }
        }
        Some(MessageContent::Parts(parts)) => parts
            .into_iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                })),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect(),
    }
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
        let req: OpenAIResponsesRequest = serde_json::from_slice(&result).unwrap();

        assert_eq!(req.model, "gpt-4.1");
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
    fn test_convert_response_id_normalizes_to_resp() {
        let input = serde_json::to_vec(&serde_json::json!({
            "id": "chatcmpl-abc",
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
        let resp: OpenAIResponsesResponse = serde_json::from_slice(&result).unwrap();

        assert_eq!(resp.id, "resp_abc");
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
