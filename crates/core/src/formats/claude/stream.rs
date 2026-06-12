use crate::error::ConvertError;
use crate::formats::StreamAdapter;
use crate::ir::*;
use crate::sse::{SseEvent, format_sse_event};

use super::types::*;

pub struct ClaudeStreamAdapter;

/// StreamState.phase == Init after message_start means no content block has started yet.
impl StreamAdapter for ClaudeStreamAdapter {
    fn parse_sse_event(
        event: &SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
        let event_type = event
            .event
            .as_deref()
            .ok_or_else(|| ConvertError::SseParse("Claude SSE events require event type".into()))?;

        match event_type {
            "message_start" => parse_message_start(&event.data, state),
            "content_block_start" => parse_content_block_start(&event.data, state),
            "content_block_delta" => parse_content_block_delta(&event.data, state),
            "content_block_stop" => Ok(vec![]),
            "message_delta" => parse_message_delta(&event.data, state),
            "message_stop" => parse_message_stop(state),
            "ping" => Ok(vec![]),
            other => Err(ConvertError::SseParse(format!(
                "unknown Claude SSE event: {other}"
            ))),
        }
    }

    fn emit_sse_event(
        event: &CanonicalStreamEvent,
        state: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        match event {
            CanonicalStreamEvent::Start { id, model } => emit_message_start(id, model, state),
            CanonicalStreamEvent::TextDelta(text) => emit_text_delta(text, state),
            CanonicalStreamEvent::ToolCallStart { index, id, name } => {
                emit_tool_call_start(*index, id, name, state)
            }
            CanonicalStreamEvent::ToolCallDelta {
                index: _,
                arguments_delta,
            } => emit_tool_call_delta(arguments_delta, state),
            CanonicalStreamEvent::ThinkingDelta(thinking) => emit_thinking_delta(thinking, state),
            CanonicalStreamEvent::Done { stop_reason, usage } => {
                emit_done(stop_reason, usage.as_ref(), state)
            }
        }
    }
}

fn parse_message_start(
    data: &str,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    let parsed: ClaudeMessageStartEvent = serde_json::from_str(data)
        .map_err(|e| ConvertError::SseParse(format!("message_start parse error: {e}")))?;

    state.response_id = Some(parsed.message.id.clone());
    state.model = Some(parsed.message.model.clone());
    state.phase = StreamPhase::Init;
    state.block_index = 0;
    state.tool_call_index = 0;
    state.done = false;

    if let Some(usage) = parsed.message.usage {
        state.accumulated_usage = Some(Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            ..Default::default()
        });
    }

    Ok(vec![CanonicalStreamEvent::Start {
        id: parsed.message.id,
        model: parsed.message.model,
    }])
}

fn parse_content_block_start(
    data: &str,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    let parsed: ClaudeContentBlockStartEvent = serde_json::from_str(data)
        .map_err(|e| ConvertError::SseParse(format!("content_block_start parse error: {e}")))?;

    state.block_index = parsed.index;
    state.phase = StreamPhase::Content;

    match parsed.content_block {
        ClaudeStreamContentBlock::ToolUse { id, name, .. } => {
            let index = state.next_tool_call_index();
            state.phase = StreamPhase::ToolCalls;
            Ok(vec![CanonicalStreamEvent::ToolCallStart {
                index,
                id,
                name,
            }])
        }
        ClaudeStreamContentBlock::Thinking { .. } => Ok(vec![]),
        ClaudeStreamContentBlock::Text { .. } => Ok(vec![]),
    }
}

fn parse_content_block_delta(
    data: &str,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    let parsed: ClaudeContentBlockDeltaEvent = serde_json::from_str(data)
        .map_err(|e| ConvertError::SseParse(format!("content_block_delta parse error: {e}")))?;

    state.block_index = parsed.index;

    match parsed.delta {
        ClaudeStreamDelta::TextDelta { text } => {
            state.phase = StreamPhase::Content;
            Ok(vec![CanonicalStreamEvent::TextDelta(text)])
        }
        ClaudeStreamDelta::InputJsonDelta { partial_json } => {
            state.phase = StreamPhase::ToolCalls;
            Ok(vec![CanonicalStreamEvent::ToolCallDelta {
                index: state.tool_call_index.saturating_sub(1),
                arguments_delta: partial_json,
            }])
        }
        ClaudeStreamDelta::ThinkingDelta { thinking } => {
            Ok(vec![CanonicalStreamEvent::ThinkingDelta(thinking)])
        }
    }
}

fn parse_message_delta(
    data: &str,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    let parsed: ClaudeMessageDeltaEvent = serde_json::from_str(data)
        .map_err(|e| ConvertError::SseParse(format!("message_delta parse error: {e}")))?;

    if let Some(usage) = parsed.usage {
        let mut accumulated = state.accumulated_usage.take().unwrap_or_default();
        accumulated.output_tokens = usage.output_tokens;
        state.accumulated_usage = Some(accumulated);
    }

    let stop_reason = parsed
        .delta
        .stop_reason
        .as_deref()
        .map(StopReason::from_claude)
        .unwrap_or(StopReason::EndTurn);

    // Stash stop_reason in model field suffix for message_stop (hack-free: use accumulated + return Done here)
    state.phase = StreamPhase::Done;

    Ok(vec![CanonicalStreamEvent::Done {
        stop_reason,
        usage: state.accumulated_usage.clone(),
    }])
}

fn parse_message_stop(state: &mut StreamState) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    state.done = true;
    Ok(vec![])
}

fn emit_message_start(
    id: &str,
    model: &str,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    state.response_id = Some(id.to_string());
    state.model = Some(model.to_string());
    state.phase = StreamPhase::Init;
    state.block_index = 0;
    state.tool_call_index = 0;
    state.done = false;
    state.accumulated_usage = Some(Usage::default());

    let data = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }
    });
    Ok(vec![format_sse_event("message_start", &data.to_string())])
}

fn emit_text_delta(text: &str, state: &mut StreamState) -> Result<Vec<String>, ConvertError> {
    let mut out = Vec::new();

    if state.phase == StreamPhase::Init {
        let data = serde_json::json!({
            "type": "content_block_start",
            "index": state.block_index,
            "content_block": { "type": "text", "text": "" }
        });
        out.push(format_sse_event("content_block_start", &data.to_string()));
        state.phase = StreamPhase::Content;
    }

    let data = serde_json::json!({
        "type": "content_block_delta",
        "index": state.block_index,
        "delta": { "type": "text_delta", "text": text }
    });
    out.push(format_sse_event("content_block_delta", &data.to_string()));

    Ok(out)
}

fn emit_tool_call_start(
    index: u32,
    id: &str,
    name: &str,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    let block_index = state.next_block_index();

    let start_data = serde_json::json!({
        "type": "content_block_start",
        "index": block_index,
        "content_block": {
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": {}
        }
    });

    state.tool_call_index = index + 1;
    state.phase = StreamPhase::ToolCalls;

    Ok(vec![format_sse_event(
        "content_block_start",
        &start_data.to_string(),
    )])
}

fn emit_tool_call_delta(
    arguments_delta: &str,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    let block_index = state.block_index.saturating_sub(1);
    let data = serde_json::json!({
        "type": "content_block_delta",
        "index": block_index,
        "delta": { "type": "input_json_delta", "partial_json": arguments_delta }
    });
    Ok(vec![format_sse_event(
        "content_block_delta",
        &data.to_string(),
    )])
}

fn emit_thinking_delta(
    thinking: &str,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    let mut out = Vec::new();

    if state.phase == StreamPhase::Init {
        let start_data = serde_json::json!({
            "type": "content_block_start",
            "index": state.block_index,
            "content_block": { "type": "thinking", "thinking": "" }
        });
        out.push(format_sse_event(
            "content_block_start",
            &start_data.to_string(),
        ));
        state.phase = StreamPhase::Content;
    }

    let data = serde_json::json!({
        "type": "content_block_delta",
        "index": state.block_index,
        "delta": { "type": "thinking_delta", "thinking": thinking }
    });
    out.push(format_sse_event("content_block_delta", &data.to_string()));

    Ok(out)
}

fn emit_done(
    stop_reason: &StopReason,
    usage: Option<&Usage>,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    let output_tokens = usage.map(|u| u.output_tokens).unwrap_or(0);
    if let Some(u) = usage {
        state.accumulated_usage = Some(u.clone());
    }

    let delta_data = serde_json::json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": stop_reason.to_claude(),
            "stop_sequence": null
        },
        "usage": { "output_tokens": output_tokens }
    });

    let stop_data = serde_json::json!({ "type": "message_stop" });

    state.done = true;
    state.phase = StreamPhase::Done;

    Ok(vec![
        format_sse_event("message_delta", &delta_data.to_string()),
        format_sse_event("message_stop", &stop_data.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_message_start() {
        let event = SseEvent {
            event: Some("message_start".into()),
            data: r#"{"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}"#.into(),
        };
        let mut state = StreamState::new();
        let events = ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Start { id, model }
                if id == "msg_123" && model == "claude-sonnet-4-20250514"
        ));
        assert_eq!(state.accumulated_usage.as_ref().unwrap().input_tokens, 10);
    }

    #[test]
    fn test_parse_content_block_delta_text_delta() {
        let event = SseEvent {
            event: Some("content_block_delta".into()),
            data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#.into(),
        };
        let mut state = StreamState::new();
        let events = ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], CanonicalStreamEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn test_parse_content_block_delta_input_json_delta() {
        let event = SseEvent {
            event: Some("content_block_start".into()),
            data: r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"search","input":{}}}"#.into(),
        };
        let mut state = StreamState::new();
        ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();

        let event = SseEvent {
            event: Some("content_block_delta".into()),
            data: r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#.into(),
        };
        let events = ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::ToolCallDelta { arguments_delta, .. }
                if arguments_delta == r#"{"q":"#
        ));
    }

    #[test]
    fn test_parse_message_delta_with_stop_reason() {
        let event = SseEvent {
            event: Some("message_delta".into()),
            data: r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#.into(),
        };
        let mut state = StreamState::new();
        state.accumulated_usage = Some(Usage {
            input_tokens: 10,
            ..Default::default()
        });
        let events = ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Done { stop_reason: StopReason::EndTurn, usage: Some(u) }
                if u.output_tokens == 15 && u.input_tokens == 10
        ));
    }

    #[test]
    fn test_parse_message_stop() {
        let mut state = StreamState::new();
        state.phase = StreamPhase::Done;
        let event = SseEvent {
            event: Some("message_stop".into()),
            data: r#"{"type":"message_stop"}"#.into(),
        };
        let events = ClaudeStreamAdapter::parse_sse_event(&event, &mut state).unwrap();
        assert!(events.is_empty());
        assert!(state.done);
    }

    #[test]
    fn test_emit_text_delta_as_content_block_delta() {
        let mut state = StreamState::new();
        state.response_id = Some("msg_123".into());
        state.model = Some("claude-sonnet-4-20250514".into());
        state.phase = StreamPhase::Init;

        let events = ClaudeStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Hi".into()),
            &mut state,
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("event: content_block_start"));
        assert!(events[1].contains("event: content_block_delta"));
        assert!(events[1].contains("text_delta"));
        assert!(events[1].contains("Hi"));
    }

    #[test]
    fn test_emit_start_as_message_start() {
        let mut state = StreamState::new();
        let events = ClaudeStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "msg_abc".into(),
                model: "claude-sonnet-4-20250514".into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].starts_with("event: message_start"));
        assert!(events[0].contains("msg_abc"));
        assert_eq!(state.response_id.as_deref(), Some("msg_abc"));
    }
}
