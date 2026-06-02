use serde::{Deserialize, Serialize};

use crate::error::ConvertError;
use crate::formats::{FormatAdapter, StreamAdapter};
use crate::ir::{CanonicalRequest, CanonicalResponse, CanonicalStreamEvent, StreamState};
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

    pub fn from_str(s: &str) -> Result<Self, ConvertError> {
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
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convert a request JSON from one format to another via the canonical IR.
pub fn convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError> {
    let canonical = parse_request_to_canonical(input, from)?;
    serialize_request_from_canonical(&canonical, to)
}

/// Convert a response JSON from one format to another via the canonical IR.
pub fn convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError> {
    let canonical = parse_response_to_canonical(input, from)?;
    serialize_response_from_canonical(&canonical, to)
}

/// Convert a single SSE event from one streaming format to another.
pub fn convert_stream_event(
    event: &SseEvent,
    from: Format,
    to: Format,
    state_in: &mut StreamState,
    state_out: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    let canonical_events = parse_stream_event(event, from, state_in)?;
    let mut output = Vec::new();
    for ce in &canonical_events {
        let lines = emit_stream_event(ce, to, state_out)?;
        output.extend(lines);
    }
    Ok(output)
}

// --- Internal dispatch helpers ---

fn parse_request_to_canonical(input: &[u8], format: Format) -> Result<CanonicalRequest, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => {
            let req = openai_chat::OpenAIChatAdapter::parse_request(input)?;
            openai_chat::OpenAIChatAdapter::request_to_canonical(req)
        }
        Format::Claude => {
            let req = claude::ClaudeAdapter::parse_request(input)?;
            claude::ClaudeAdapter::request_to_canonical(req)
        }
        Format::OpenAIResponses => {
            let req = openai_resp::OpenAIResponsesAdapter::parse_request(input)?;
            openai_resp::OpenAIResponsesAdapter::request_to_canonical(req)
        }
        Format::Gemini => {
            let req = gemini::GeminiAdapter::parse_request(input)?;
            gemini::GeminiAdapter::request_to_canonical(req)
        }
    }
}

fn serialize_request_from_canonical(canonical: &CanonicalRequest, format: Format) -> Result<Vec<u8>, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => {
            let req = openai_chat::OpenAIChatAdapter::request_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&req)?)
        }
        Format::Claude => {
            let req = claude::ClaudeAdapter::request_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&req)?)
        }
        Format::OpenAIResponses => {
            let req = openai_resp::OpenAIResponsesAdapter::request_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&req)?)
        }
        Format::Gemini => {
            let req = gemini::GeminiAdapter::request_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&req)?)
        }
    }
}

fn parse_response_to_canonical(input: &[u8], format: Format) -> Result<CanonicalResponse, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => {
            let resp = openai_chat::OpenAIChatAdapter::parse_response(input)?;
            openai_chat::OpenAIChatAdapter::response_to_canonical(resp)
        }
        Format::Claude => {
            let resp = claude::ClaudeAdapter::parse_response(input)?;
            claude::ClaudeAdapter::response_to_canonical(resp)
        }
        Format::OpenAIResponses => {
            let resp = openai_resp::OpenAIResponsesAdapter::parse_response(input)?;
            openai_resp::OpenAIResponsesAdapter::response_to_canonical(resp)
        }
        Format::Gemini => {
            let resp = gemini::GeminiAdapter::parse_response(input)?;
            gemini::GeminiAdapter::response_to_canonical(resp)
        }
    }
}

fn serialize_response_from_canonical(canonical: &CanonicalResponse, format: Format) -> Result<Vec<u8>, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => {
            let resp = openai_chat::OpenAIChatAdapter::response_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&resp)?)
        }
        Format::Claude => {
            let resp = claude::ClaudeAdapter::response_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&resp)?)
        }
        Format::OpenAIResponses => {
            let resp = openai_resp::OpenAIResponsesAdapter::response_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&resp)?)
        }
        Format::Gemini => {
            let resp = gemini::GeminiAdapter::response_from_canonical(canonical)?;
            Ok(serde_json::to_vec(&resp)?)
        }
    }
}

fn parse_stream_event(
    event: &SseEvent,
    format: Format,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => openai_chat::OpenAIChatStreamAdapter::parse_sse_event(event, state),
        Format::Claude => claude::ClaudeStreamAdapter::parse_sse_event(event, state),
        Format::OpenAIResponses => openai_resp::OpenAIResponsesStreamAdapter::parse_sse_event(event, state),
        Format::Gemini => gemini::GeminiStreamAdapter::parse_sse_event(event, state),
    }
}

fn emit_stream_event(
    event: &CanonicalStreamEvent,
    format: Format,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    use crate::formats::{claude, gemini, openai_chat, openai_resp};
    match format {
        Format::OpenAIChat => openai_chat::OpenAIChatStreamAdapter::emit_sse_event(event, state),
        Format::Claude => claude::ClaudeStreamAdapter::emit_sse_event(event, state),
        Format::OpenAIResponses => openai_resp::OpenAIResponsesStreamAdapter::emit_sse_event(event, state),
        Format::Gemini => gemini::GeminiStreamAdapter::emit_sse_event(event, state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::parse_sse_block;

    #[test]
    fn test_format_from_str() {
        assert_eq!(Format::from_str("openai_chat").unwrap(), Format::OpenAIChat);
        assert_eq!(Format::from_str("openai").unwrap(), Format::OpenAIChat);
        assert_eq!(Format::from_str("claude").unwrap(), Format::Claude);
        assert_eq!(Format::from_str("anthropic").unwrap(), Format::Claude);
        assert_eq!(Format::from_str("openai_responses").unwrap(), Format::OpenAIResponses);
        assert_eq!(Format::from_str("responses").unwrap(), Format::OpenAIResponses);
        assert_eq!(Format::from_str("gemini").unwrap(), Format::Gemini);
        assert_eq!(Format::from_str("google").unwrap(), Format::Gemini);
        assert!(Format::from_str("unknown").is_err());
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
            ).unwrap();
            all_output.extend(lines);
        }

        // Must have response.output_item.done with function_call
        let has_fc_item_done = all_output.iter().any(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        });
        assert!(has_fc_item_done, "Missing response.output_item.done for function_call.\nAll output:\n{}", all_output.join("\n"));

        // Must have response.function_call_arguments.done
        let has_args_done = all_output.iter().any(|line| {
            line.contains("response.function_call_arguments.done")
        });
        assert!(has_args_done, "Missing response.function_call_arguments.done");

        // Must have response.completed with function_call in output
        let has_completed_fc = all_output.iter().any(|line| {
            line.contains("response.completed") && line.contains("function_call")
        });
        assert!(has_completed_fc, "response.completed missing function_call in output");

        // Verify the function_call has correct data
        let fc_done_line = all_output.iter().find(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        }).unwrap();
        assert!(fc_done_line.contains("call_xyz"), "function_call missing call_id");
        assert!(fc_done_line.contains("shell"), "function_call missing name");
        assert!(fc_done_line.contains("cmd") && fc_done_line.contains("ls"), "function_call missing arguments");
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
            ).unwrap();
            all_output.extend(lines);
        }

        // Must have response.output_item.done with function_call
        let has_fc_item_done = all_output.iter().any(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        });
        assert!(has_fc_item_done, "Missing response.output_item.done for function_call.\nAll output:\n{}", all_output.join("\n"));

        // Verify function_call data
        let fc_done_line = all_output.iter().find(|line| {
            line.contains("response.output_item.done") && line.contains("function_call")
        }).unwrap();
        assert!(fc_done_line.contains("toolu_01"), "function_call missing call_id");
        assert!(fc_done_line.contains("read_file"), "function_call missing name");
        assert!(fc_done_line.contains("path") && fc_done_line.contains("src/main.rs"), "function_call missing arguments");
    }
}
