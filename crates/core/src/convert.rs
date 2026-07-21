use serde::{Deserialize, Serialize};

use crate::error::ConvertError;
use crate::ir::StreamState;
use crate::sse::SseEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    #[serde(rename = "openai_chat")]
    OpenAIChat,
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "openai_responses")]
    OpenAIResponses,
    #[serde(rename = "gemini")]
    Gemini,
}

impl Format {
    pub fn as_str(&self) -> &'static str {
        match self {
            Format::OpenAIChat => "openai_chat",
            Format::Claude => "claude",
            Format::OpenAIResponses => "openai_responses",
            Format::Gemini => "gemini",
        }
    }

    /// Parse a format from a string with alias support.
    ///
    /// # Examples
    ///
    /// ```
    /// use any_converter_core::convert::Format;
    ///
    /// assert_eq!(Format::parse("openai")?, Format::OpenAIChat);
    /// assert_eq!(Format::parse("claude")?, Format::Claude);
    /// # Ok::<(), any_converter_core::ConvertError>(())
    /// ```
    pub fn parse(s: &str) -> Result<Self, ConvertError> {
        match s {
            "openai_chat" | "openai" => Ok(Format::OpenAIChat),
            "claude" | "anthropic" => Ok(Format::Claude),
            "openai_responses" | "responses" => Ok(Format::OpenAIResponses),
            "gemini" | "google" => Ok(Format::Gemini),
            _ => Err(ConvertError::InvalidField {
                field: "format".into(),
                reason: format!("unknown format: {s}"),
            }),
        }
    }

    /// Deprecated: use `Format::parse` instead.
    #[allow(clippy::should_implement_trait)]
    #[deprecated(since = "0.1.6", note = "use Format::parse instead")]
    pub fn from_str(s: &str) -> Result<Self, ConvertError> {
        Self::parse(s)
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convert a request JSON from one format to another.
///
/// # Examples
///
/// ```
/// use any_converter_core::convert::{Format, convert_request};
///
/// let input = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}"#;
/// let output = convert_request(input.as_bytes(), Format::OpenAIChat, Format::Claude)?;
/// let parsed: serde_json::Value = serde_json::from_slice(&output)?;
/// assert!(parsed["messages"].is_array());
/// # Ok::<(), any_converter_core::ConvertError>(())
/// ```
pub fn convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError> {
    if from == to {
        return Ok(input.to_vec());
    }
    match crate::converters::get_converter(from, to) {
        Some(converter) => converter.convert_request(input),
        None => Err(ConvertError::UnsupportedConversion {
            from: from.to_string(),
            to: to.to_string(),
        }),
    }
}

/// Convert a response JSON from one format to another.
///
/// # Examples
///
/// ```
/// use any_converter_core::convert::{Format, convert_response};
///
/// let input = r#"{"id":"chatcmpl-123","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi!"},"finish_reason":"stop"}]}"#;
/// let output = convert_response(input.as_bytes(), Format::OpenAIChat, Format::Claude)?;
/// let parsed: serde_json::Value = serde_json::from_slice(&output)?;
/// assert!(parsed["content"].is_array());
/// # Ok::<(), any_converter_core::ConvertError>(())
/// ```
pub fn convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError> {
    if from == to {
        return Ok(input.to_vec());
    }
    match crate::converters::get_converter(from, to) {
        Some(converter) => converter.convert_response(input),
        None => Err(ConvertError::UnsupportedConversion {
            from: from.to_string(),
            to: to.to_string(),
        }),
    }
}

/// Convert a single SSE event from one streaming format to another.
///
/// # Examples
///
/// ```
/// use any_converter_core::convert::{Format, convert_stream_event};
/// use any_converter_core::ir::StreamState;
/// use any_converter_core::sse::parse_sse_block;
///
/// let block = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":0,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
/// let event = parse_sse_block(block).unwrap();
/// let mut state_in = StreamState::new();
/// let mut state_out = StreamState::new();
/// let lines = convert_stream_event(&event, Format::OpenAIChat, Format::Claude, &mut state_in, &mut state_out)?;
/// assert!(!lines.is_empty());
/// # Ok::<(), any_converter_core::ConvertError>(())
/// ```
pub fn convert_stream_event(
    event: &SseEvent,
    from: Format,
    to: Format,
    state_in: &mut StreamState,
    state_out: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    if from == to {
        let raw = if let Some(evt) = &event.event {
            crate::sse::format_sse_event(evt, &event.data)
        } else {
            crate::sse::format_sse_data(&event.data)
        };
        return Ok(vec![raw]);
    }
    match crate::converters::get_converter(from, to) {
        Some(converter) => converter.convert_stream_event(event, state_in, state_out),
        None => Err(ConvertError::UnsupportedConversion {
            from: from.to_string(),
            to: to.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::parse_sse_block;

    #[test]
    fn test_format_from_str() {
        assert_eq!(Format::parse("openai_chat").unwrap(), Format::OpenAIChat);
        assert_eq!(Format::parse("openai").unwrap(), Format::OpenAIChat);
        assert_eq!(Format::parse("claude").unwrap(), Format::Claude);
        assert_eq!(Format::parse("anthropic").unwrap(), Format::Claude);
        assert_eq!(
            Format::parse("openai_responses").unwrap(),
            Format::OpenAIResponses
        );
        assert_eq!(Format::parse("responses").unwrap(), Format::OpenAIResponses);
        assert_eq!(Format::parse("gemini").unwrap(), Format::Gemini);
        assert_eq!(Format::parse("google").unwrap(), Format::Gemini);
        assert!(Format::parse("unknown").is_err());
    }

    #[test]
    fn test_format_display() {
        assert_eq!(Format::OpenAIChat.to_string(), "openai_chat");
        assert_eq!(Format::Claude.to_string(), "claude");
    }

    #[test]
    fn test_format_serde_roundtrip() {
        let format = Format::Claude;
        let json = serde_json::to_string(&format).unwrap();
        assert_eq!(json, "\"claude\"");
        let back: Format = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Format::Claude);
    }

    /// Integration test: OpenAI Chat streaming tool_calls → OpenAI Responses format
    /// This simulates what happens when Codex CLI uses any-converter as proxy to an OpenAI Chat backend.
    #[test]
    fn test_stream_openai_chat_tool_calls_to_responses_emits_output_item_done() {
        let sse_blocks = vec![
            // First chunk: role + empty content
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            // Text content
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{"content":"I'll check."},"finish_reason":null}]}"#,
            // Tool call start
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_xyz","type":"function","function":{"name":"shell","arguments":""}}]},"finish_reason":null}]}"#,
            // Tool call argument deltas
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":null}]}"#,
            // Finish reason
            r#"data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4.1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            // Done sentinel
            "data: [DONE]",
        ];

        let mut state_in = StreamState::new();
        let mut state_out = StreamState::new();
        let mut all_output = Vec::new();

        for block in &sse_blocks {
            let event = parse_sse_block(block).unwrap();
            let lines = convert_stream_event(
                &event,
                Format::OpenAIChat,
                Format::OpenAIResponses,
                &mut state_in,
                &mut state_out,
            )
            .unwrap();
            all_output.extend(lines);
        }

        // Must have response.output_item.done with function_call
        let has_fc_item_done = all_output.iter().any(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        });
        assert!(
            has_fc_item_done,
            "Missing response.output_item.done for function_call.\nAll output:\n{}",
            all_output.join("\n")
        );

        // Must have response.function_call_arguments.done
        let has_args_done = all_output
            .iter()
            .any(|line| line.contains("response.function_call_arguments.done"));
        assert!(
            has_args_done,
            "Missing response.function_call_arguments.done"
        );

        // Must have response.completed with function_call in output
        let has_completed_fc = all_output
            .iter()
            .any(|line| line.contains("response.completed") && line.contains("function_call"));
        assert!(
            has_completed_fc,
            "response.completed missing function_call in output"
        );

        // Verify the function_call has correct data
        let fc_done_line = all_output
            .iter()
            .find(|line| {
                line.contains("response.output_item.done") && line.contains("function_call")
            })
            .unwrap();
        assert!(
            fc_done_line.contains("call_xyz"),
            "function_call missing call_id"
        );
        assert!(fc_done_line.contains("shell"), "function_call missing name");
        assert!(
            fc_done_line.contains("cmd") && fc_done_line.contains("ls"),
            "function_call missing arguments"
        );
    }

    /// Integration test: Claude streaming tool_use → OpenAI Responses format
    #[test]
    fn test_stream_claude_tool_use_to_responses_emits_output_item_done() {
        let sse_blocks = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-20250514\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check.\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"read_file\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\"\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"src/main.rs\\\"}\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":30}}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ];

        let mut state_in = StreamState::new();
        let mut state_out = StreamState::new();
        let mut all_output = Vec::new();

        for block in &sse_blocks {
            let event = parse_sse_block(block).unwrap();
            let lines = convert_stream_event(
                &event,
                Format::Claude,
                Format::OpenAIResponses,
                &mut state_in,
                &mut state_out,
            )
            .unwrap();
            all_output.extend(lines);
        }

        // Must have response.output_item.done with function_call
        let has_fc_item_done = all_output.iter().any(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        });
        assert!(
            has_fc_item_done,
            "Missing response.output_item.done for function_call.\nAll output:\n{}",
            all_output.join("\n")
        );

        // Verify function_call data
        let fc_done_line = all_output
            .iter()
            .find(|line| {
                line.contains("response.output_item.done") && line.contains("function_call")
            })
            .unwrap();
        assert!(
            fc_done_line.contains("toolu_01"),
            "function_call missing call_id"
        );
        assert!(
            fc_done_line.contains("read_file"),
            "function_call missing name"
        );
        assert!(
            fc_done_line.contains("path") && fc_done_line.contains("src/main.rs"),
            "function_call missing arguments"
        );
    }

    /// MiniMax-style Claude tool_use stream omits `message_delta` and ends on `message_stop`.
    /// Responses clients require `response.completed` as the terminal event.
    #[test]
    fn test_stream_claude_tool_use_without_message_delta_emits_response_completed() {
        let sse_blocks = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-20250514\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":50,\"output_tokens\":0}}}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"read_file\",\"input\":{}}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"src/main.rs\\\"}\"}}",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
            // Intentionally no message_delta — MiniMax / Claude-compatible providers often omit it.
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ];

        let mut state_in = StreamState::new();
        let mut state_out = StreamState::new();
        let mut all_output = Vec::new();

        for block in &sse_blocks {
            let event = parse_sse_block(block).unwrap();
            let lines = convert_stream_event(
                &event,
                Format::Claude,
                Format::OpenAIResponses,
                &mut state_in,
                &mut state_out,
            )
            .unwrap();
            all_output.extend(lines);
        }

        let has_completed_fc = all_output
            .iter()
            .any(|line| line.contains("response.completed") && line.contains("function_call"));
        assert!(
            has_completed_fc,
            "response.completed missing function_call.\nAll output:\n{}",
            all_output.join("\n")
        );

        let has_args_done = all_output
            .iter()
            .any(|line| line.contains("response.function_call_arguments.done"));
        assert!(
            has_args_done,
            "Missing response.function_call_arguments.done"
        );
    }
}
