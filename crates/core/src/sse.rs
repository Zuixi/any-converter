/// SSE (Server-Sent Events) parsing and emitting utilities.
///
/// Handles all four SSE dialects:
/// - OpenAI Chat: anonymous `data:` lines, terminated by `data: [DONE]`
/// - Claude: named `event:` + `data:` pairs, terminated by `event: message_stop`
/// - OpenAI Responses: named `event:` + `data:` pairs, terminated by `event: response.completed`
/// - Gemini: anonymous `data:` lines, terminated by connection close
///
/// A parsed SSE event with optional event type and data payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Parse a raw SSE block (one or more lines separated by blank line) into an SseEvent.
///
/// SSE spec: lines starting with `event:` set the event type,
/// lines starting with `data:` append to the data buffer.
/// A blank line dispatches the event.
///
/// # Examples
///
/// ```
/// use any_converter_core::sse::parse_sse_block;
///
/// let block = "data: {\"id\":\"123\"}";
/// let event = parse_sse_block(block).unwrap();
/// assert_eq!(event.data, "{\"id\":\"123\"}");
/// assert!(event.event.is_none());
/// ```
pub fn parse_sse_block(block: &str) -> Option<SseEvent> {
    let mut event_type: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.lines() {
        if let Some(val) = line.strip_prefix("event:") {
            event_type = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("data:") {
            data_lines.push(val.trim_start_matches(' '));
        } else if line.starts_with(':') {
            // SSE comment, skip
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    let data = data_lines.join("\n");
    Some(SseEvent {
        event: event_type,
        data,
    })
}

/// Split a raw SSE byte stream into individual event blocks.
/// Events are separated by double newlines (`\n\n` or `\r\n\r\n`).
///
/// # Examples
///
/// ```
/// use any_converter_core::sse::split_sse_blocks;
///
/// let input = "data: first\n\ndata: second\n\n";
/// let blocks = split_sse_blocks(input);
/// assert_eq!(blocks.len(), 2);
/// ```
pub fn split_sse_blocks(input: &str) -> Vec<String> {
    let input = if input.contains('\r') {
        input.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        input.to_string()
    };
    input
        .split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Format an SSE event for emission (without the event type line).
pub fn format_sse_data(data: &str) -> String {
    format!("data: {data}\n\n")
}

/// Format an SSE event with a named event type.
pub fn format_sse_event(event_type: &str, data: &str) -> String {
    format!("event: {event_type}\ndata: {data}\n\n")
}

/// Check if a data line is the OpenAI `[DONE]` sentinel.
pub fn is_openai_done(data: &str) -> bool {
    data.trim() == "[DONE]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_anonymous_data() {
        let block = "data: {\"id\":\"chatcmpl-123\"}";
        let event = parse_sse_block(block).unwrap();
        assert!(event.event.is_none());
        assert_eq!(event.data, "{\"id\":\"chatcmpl-123\"}");
    }

    #[test]
    fn test_parse_named_event() {
        let block = "event: content_block_delta\ndata: {\"type\":\"text_delta\"}";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.event.as_deref(), Some("content_block_delta"));
        assert_eq!(event.data, "{\"type\":\"text_delta\"}");
    }

    #[test]
    fn test_parse_empty_block() {
        assert!(parse_sse_block("").is_none());
        assert!(parse_sse_block(": comment only").is_none());
    }

    #[test]
    fn test_parse_multi_data_lines() {
        let block = "data: line1\ndata: line2";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.data, "line1\nline2");
    }

    #[test]
    fn test_split_sse_blocks() {
        let input = "data: first\n\ndata: second\n\ndata: [DONE]\n\n";
        let blocks = split_sse_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "data: first");
        assert_eq!(blocks[2], "data: [DONE]");
    }

    #[test]
    fn test_format_sse_data() {
        assert_eq!(format_sse_data("{\"a\":1}"), "data: {\"a\":1}\n\n");
    }

    #[test]
    fn test_format_sse_event() {
        let result = format_sse_event("message_start", "{\"type\":\"message_start\"}");
        assert_eq!(
            result,
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n"
        );
    }

    #[test]
    fn test_is_openai_done() {
        assert!(is_openai_done("[DONE]"));
        assert!(is_openai_done("  [DONE]  "));
        assert!(!is_openai_done("{\"id\":\"123\"}"));
    }

    #[test]
    fn test_parse_with_sse_comment() {
        let block = ": this is a comment\ndata: actual_data";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.data, "actual_data");
    }

    #[test]
    fn test_split_sse_blocks_crlf() {
        let input = "data: first\r\n\r\ndata: second\r\n\r\ndata: [DONE]\r\n\r\n";
        let blocks = split_sse_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "data: first");
        assert_eq!(blocks[1], "data: second");
        assert_eq!(blocks[2], "data: [DONE]");
    }

    #[test]
    fn test_data_with_leading_space() {
        let block = "data: {\"key\": \"value\"}";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.data, "{\"key\": \"value\"}");
    }
}
