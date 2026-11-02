use crate::convert::Format;
use crate::error::ConvertError;
use crate::formats::StreamAdapter;
use crate::formats::openai_resp::build::{seal_block, seal_thinking};
use crate::ir::*;
use crate::sse::{SseEvent, format_sse_event};

const ITEM_TYPE_REASONING: &str = "reasoning";
const ITEM_TYPE_SUMMARY_TEXT: &str = "summary_text";

// StreamState has one reasoning accumulator, `thinking_signature`: the open
// thinking block's text rides ahead of its signature in it, split by a NUL no
// base64 signature carries (magpie keeps the two in `e.text`/`e.sig`).
// shortcut: a StreamState without a thinking-text field; upgrade when the IR gains one.
const THINKING_TEXT_END: char = '\0';

/// The open thinking block's text and signature, as they accumulated.
fn thinking_parts(state: &StreamState) -> (String, String) {
    match state.thinking_signature.rsplit_once(THINKING_TEXT_END) {
        Some((text, signature)) => (text.to_owned(), signature.to_owned()),
        None => (state.thinking_signature.clone(), String::new()),
    }
}

/// Claude signed the open thinking block, whose text its signature follows.
fn signed_by_claude(state: &StreamState) -> bool {
    state.thinking_signature_by == Some(Format::Claude)
        && state
            .thinking_signature
            .rsplit_once(THINKING_TEXT_END)
            .is_some_and(|(_, signature)| !signature.is_empty())
}

/// Items before the answer: Claude's signed thinking, once its signature has
/// arrived, comes back first as a reasoning item of its own, the way the
/// thinking it reasons with does.
fn reasoning_prefix(state: &StreamState) -> u32 {
    u32::from(signed_by_claude(state))
}

fn status_to_stop_reason(status: &str) -> StopReason {
    match status {
        "completed" => StopReason::EndTurn,
        "incomplete" => StopReason::MaxTokens,
        "failed" => StopReason::ContentFilter,
        _ => StopReason::EndTurn,
    }
}

fn stop_reason_to_status(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::MaxTokens => "incomplete",
        StopReason::ContentFilter => "failed",
        _ => "completed",
    }
}

pub struct OpenAIResponsesStreamAdapter;

impl StreamAdapter for OpenAIResponsesStreamAdapter {
    fn parse_sse_event(
        event: &SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
        parse_sse_event_impl(event, state)
    }

    fn emit_sse_event(
        event: &CanonicalStreamEvent,
        state: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        emit_sse_event_impl(event, state)
    }
}

fn parse_sse_event_impl(
    event: &SseEvent,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    let data: serde_json::Value =
        serde_json::from_str(&event.data).map_err(|e| ConvertError::SseParse(e.to_string()))?;

    let event_type = event
        .event
        .as_deref()
        .or_else(|| data.get("type").and_then(|v| v.as_str()))
        .ok_or_else(|| ConvertError::SseParse("missing event type".into()))?;

    match event_type {
        "response.created" => {
            let response = data
                .get("response")
                .ok_or_else(|| ConvertError::MissingField("response".into()))?;
            let id = response
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("response.id".into()))?
                .to_string();
            let model = response
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(usage) = response.get("usage") {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_read = usage
                    .get("input_tokens_details")
                    .and_then(|v| v.get("cached_tokens"))
                    .and_then(|v| v.as_u64());
                state.accumulated_usage = Some(Usage {
                    input_tokens,
                    output_tokens: usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    cache_read_tokens: cache_read,
                    ..Default::default()
                });
            }
            state.response_id = Some(id.clone());
            state.model = Some(model.clone());
            state.phase = StreamPhase::Content;
            Ok(vec![CanonicalStreamEvent::Start { id, model }])
        }
        "response.output_text.delta" => {
            let delta = data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(vec![CanonicalStreamEvent::TextDelta(delta)])
        }
        "response.function_call_arguments.delta" => {
            let delta = data
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let index = data
                .get("output_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(state.tool_call_index as u64) as u32;
            Ok(vec![CanonicalStreamEvent::ToolCallDelta {
                index,
                arguments_delta: delta,
            }])
        }
        "response.output_item.added" => {
            let item = data.get("item").unwrap_or(&data);
            if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                let id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let index = state.next_tool_call_index();
                state.phase = StreamPhase::ToolCalls;
                Ok(vec![CanonicalStreamEvent::ToolCallStart {
                    index,
                    id,
                    name,
                    signature: None,
                }])
            } else {
                Ok(vec![])
            }
        }
        "response.completed" => {
            let response = data
                .get("response")
                .ok_or_else(|| ConvertError::MissingField("response".into()))?;
            let status = response
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            let stop_reason = status_to_stop_reason(status);
            let usage = response.get("usage").map(|u| {
                let input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let cache_read = u
                    .get("input_tokens_details")
                    .and_then(|v| v.get("cached_tokens"))
                    .and_then(|v| v.as_u64());
                Usage {
                    input_tokens,
                    output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    cache_read_tokens: cache_read,
                    ..Default::default()
                }
            });
            state.done = true;
            state.phase = StreamPhase::Done;
            state.accumulated_usage = usage.clone();
            Ok(vec![CanonicalStreamEvent::Done { stop_reason, usage }])
        }
        _ => Ok(vec![]),
    }
}

fn emit_sse_event_impl(
    event: &CanonicalStreamEvent,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    match event {
        // Claude's signature, or its redacted_thinking block, which only this
        // emitter may write back (claudeseal). Any other vendor's is no part of
        // a response, and Gemini's goes back on its own path.
        CanonicalStreamEvent::ThinkingSignature { by, delta } => {
            if *by != Format::Claude || delta.is_empty() {
                return Ok(vec![]);
            }
            // The first delta ends the text it signs.
            if state.thinking_signature_by != Some(*by) {
                state.thinking_signature_by = Some(*by);
                state.thinking_signature.push(THINKING_TEXT_END);
            }
            state.thinking_signature.push_str(delta);
            Ok(vec![])
        }
        CanonicalStreamEvent::SealedThinking {
            by: Format::Claude,
            raw,
        } => {
            // The next free output index: what already streamed, then the
            // blocks sealed whole. `response.completed` can't repeat it (no
            // StreamState field to keep it in), so the client reads the pair.
            let index = u32::from(!state.accumulated_text.is_empty())
                + state.accumulated_tool_calls.len() as u32
                + state.next_block_index();
            let id = format!("rs_{}", uuid::Uuid::new_v4());
            let added = serde_json::json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": {"id": &id, "type": ITEM_TYPE_REASONING, "status": "in_progress", "summary": []},
            });
            let mut item = serde_json::json!({
                "id": &id, "type": ITEM_TYPE_REASONING, "status": "completed", "summary": [],
            });
            item["encrypted_content"] = seal_block(raw).into();
            let done = serde_json::json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": &item,
            });
            Ok(vec![
                format_sse_event("response.output_item.added", &added.to_string()),
                format_sse_event("response.output_item.done", &done.to_string()),
            ])
        }
        CanonicalStreamEvent::SealedThinking { .. } => Ok(vec![]),
        CanonicalStreamEvent::Start { id, model } => {
            let normalized_id = crate::converters::shared::normalize_id_to_resp(id);
            state.response_id = Some(normalized_id.clone());
            state.model = Some(model.clone());
            state.phase = StreamPhase::Content;
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let response = serde_json::json!({
                "id": normalized_id,
                "object": "response",
                "created_at": created_at,
                "model": model,
                "status": "in_progress",
                "output": [],
            });
            let created = serde_json::json!({
                "type": "response.created",
                "response": &response,
            });
            let in_progress = serde_json::json!({
                "type": "response.in_progress",
                "response": &response,
            });
            Ok(vec![
                format_sse_event("response.created", &created.to_string()),
                format_sse_event("response.in_progress", &in_progress.to_string()),
            ])
        }
        CanonicalStreamEvent::TextDelta(text) => {
            let item_id = state
                .response_id
                .as_deref()
                .map(|id| format!("{id}_msg_0"))
                .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4()));
            let output_index = reasoning_prefix(state);

            let mut events = Vec::new();

            if state.accumulated_text.is_empty() {
                let add_item = serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "type": "message",
                        "id": &item_id,
                        "role": "assistant",
                        "status": "in_progress",
                        "content": [],
                    }
                });
                events.push(format_sse_event(
                    "response.output_item.added",
                    &add_item.to_string(),
                ));

                let add_part = serde_json::json!({
                    "type": "response.content_part.added",
                    "item_id": &item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "",
                    }
                });
                events.push(format_sse_event(
                    "response.content_part.added",
                    &add_part.to_string(),
                ));
            }

            state.accumulated_text.push_str(text);

            let data = serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": &item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": text,
            });
            events.push(format_sse_event(
                "response.output_text.delta",
                &data.to_string(),
            ));

            Ok(events)
        }
        CanonicalStreamEvent::ToolCallStart {
            index, id, name, ..
        } => {
            let item_id = format!("fc_{id}");
            let text_offset: u32 = if state.accumulated_text.is_empty() {
                0
            } else {
                1
            };
            let output_index = index + text_offset + reasoning_prefix(state);
            state.accumulated_tool_calls.push(AccumulatedToolCall {
                index: *index,
                id: id.clone(),
                name: name.clone(),
                arguments: String::new(),
            });
            let data = serde_json::json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": &item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": id,
                    "name": name,
                    "arguments": "",
                }
            });
            Ok(vec![format_sse_event(
                "response.output_item.added",
                &data.to_string(),
            )])
        }
        CanonicalStreamEvent::ToolCallDelta {
            index,
            arguments_delta,
        } => {
            let text_offset: u32 = if state.accumulated_text.is_empty() {
                0
            } else {
                1
            };
            let item_id = state
                .accumulated_tool_calls
                .iter_mut()
                .find(|tc| tc.index == *index)
                .map(|tc| {
                    tc.arguments.push_str(arguments_delta);
                    format!("fc_{}", tc.id)
                })
                .ok_or_else(|| {
                    ConvertError::StreamState(format!(
                        "tool call index {} not found in state",
                        index
                    ))
                })?;
            let data = serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "item_id": &item_id,
                "output_index": index + text_offset + reasoning_prefix(state),
                "delta": arguments_delta,
            });
            Ok(vec![format_sse_event(
                "response.function_call_arguments.delta",
                &data.to_string(),
            )])
        }
        CanonicalStreamEvent::Done { stop_reason, usage } => {
            let status = stop_reason_to_status(stop_reason);
            let response_id = state
                .response_id
                .clone()
                .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4()));
            let model = state.model.clone().unwrap_or_default();
            // The Responses API typically has one assistant message per response.
            // We use a deterministic ID based on the response ID. This is safe
            // because text and tool calls are mutually exclusive in a single response.
            let item_id = format!("{response_id}_msg_0");

            let mut events = Vec::new();
            let mut output_items: Vec<serde_json::Value> = Vec::new();
            let prefix = reasoning_prefix(state);

            // Claude's signed thinking closes here: the reply's first reasoning
            // item carries the block, signature and all, in encrypted_content,
            // which is all a Responses client with store off keeps of it — the
            // next turn gives it back to Anthropic as it wrote it (claudeseal).
            let (thinking_text, thinking_signature) = thinking_parts(state);
            if signed_by_claude(state) {
                let id = format!("rs_{}", uuid::Uuid::new_v4());
                let mut reasoning_item = serde_json::json!({
                    "id": &id,
                    "type": ITEM_TYPE_REASONING,
                    "status": "completed",
                    "summary": [],
                });
                if !thinking_text.is_empty() {
                    reasoning_item["summary"] = serde_json::json!([{
                        "type": ITEM_TYPE_SUMMARY_TEXT,
                        "text": thinking_text,
                    }]);
                }
                reasoning_item["encrypted_content"] =
                    seal_thinking(&thinking_text, &thinking_signature).into();

                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {"id": &id, "type": ITEM_TYPE_REASONING, "status": "in_progress", "summary": []},
                });
                let done = serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": &reasoning_item,
                });
                events.push(format_sse_event(
                    "response.output_item.added",
                    &added.to_string(),
                ));
                events.push(format_sse_event(
                    "response.output_item.done",
                    &done.to_string(),
                ));
                output_items.push(reasoning_item);
            }

            if !state.accumulated_text.is_empty() {
                let text_done = serde_json::json!({
                    "type": "response.output_text.done",
                    "item_id": &item_id,
                    "output_index": prefix,
                    "content_index": 0,
                    "text": &state.accumulated_text,
                });
                events.push(format_sse_event(
                    "response.output_text.done",
                    &text_done.to_string(),
                ));

                let part_done = serde_json::json!({
                    "type": "response.content_part.done",
                    "item_id": &item_id,
                    "output_index": prefix,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": &state.accumulated_text,
                    }
                });
                events.push(format_sse_event(
                    "response.content_part.done",
                    &part_done.to_string(),
                ));

                let msg_item = serde_json::json!({
                    "type": "message",
                    "id": &item_id,
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": &state.accumulated_text,
                    }],
                });
                let item_done = serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": prefix,
                    "item": &msg_item,
                });
                events.push(format_sse_event(
                    "response.output_item.done",
                    &item_done.to_string(),
                ));
                output_items.push(msg_item);
            }

            let text_offset: u32 = if state.accumulated_text.is_empty() {
                0
            } else {
                1
            };
            for tc in &state.accumulated_tool_calls {
                let item_id = format!("fc_{}", tc.id);
                let output_index = tc.index + text_offset + prefix;
                let args_done = serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": &item_id,
                    "output_index": output_index,
                    "arguments": &tc.arguments,
                });
                events.push(format_sse_event(
                    "response.function_call_arguments.done",
                    &args_done.to_string(),
                ));

                let fc_item = serde_json::json!({
                    "id": &item_id,
                    "type": "function_call",
                    "status": "completed",
                    "call_id": &tc.id,
                    "name": &tc.name,
                    "arguments": &tc.arguments,
                });
                let fc_done = serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": &fc_item,
                });
                events.push(format_sse_event(
                    "response.output_item.done",
                    &fc_done.to_string(),
                ));
                output_items.push(fc_item);
            }

            if output_items.is_empty() {
                output_items.push(serde_json::json!({
                    "type": "message",
                    "id": &item_id,
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": "",
                    }],
                }));
            }

            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut response = serde_json::json!({
                "id": &response_id,
                "object": "response",
                "created_at": created_at,
                "model": &model,
                "status": status,
                "output": output_items,
            });
            if let Some(u) = usage {
                response["usage"] = serde_json::json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                    "total_tokens": u.total_tokens(),
                });
            }
            let completed = serde_json::json!({
                "type": "response.completed",
                "response": response,
            });
            events.push(format_sse_event(
                "response.completed",
                &completed.to_string(),
            ));

            state.done = true;
            state.phase = StreamPhase::Done;
            Ok(events)
        }
        CanonicalStreamEvent::ThinkingDelta(delta) => {
            // No summary deltas: the block comes back whole, sealed, when the
            // item closes (claudeseal).
            match state.thinking_signature.rsplit_once(THINKING_TEXT_END) {
                Some((text, _)) => state.thinking_signature.insert_str(text.len(), delta),
                None => state.thinking_signature.push_str(delta),
            }
            Ok(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::StreamAdapter;
    use crate::sse::parse_sse_block;

    fn parse_event(block: &str, state: &mut StreamState) -> Vec<CanonicalStreamEvent> {
        let event = parse_sse_block(block).unwrap();
        OpenAIResponsesStreamAdapter::parse_sse_event(&event, state).unwrap()
    }

    #[test]
    fn test_parse_response_created() {
        let mut state = StreamState::new();
        let events = parse_event(
            r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_123","model":"gpt-4.1","status":"in_progress","output":[]}}"#,
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Start { id, model }
                if id == "resp_123" && model == "gpt-4.1"
        ));
        assert_eq!(state.response_id.as_deref(), Some("resp_123"));
    }

    #[test]
    fn test_parse_response_created_data_only() {
        let mut state = StreamState::new();
        let events = parse_event(
            r#"data: {"type":"response.created","response":{"id":"resp_123","model":"gpt-4.1","status":"in_progress","output":[]}}"#,
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Start { id, model }
                if id == "resp_123" && model == "gpt-4.1"
        ));
    }

    #[test]
    fn test_parse_output_text_delta() {
        let mut state = StreamState::new();
        let events = parse_event(
            r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hello"}"#,
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], CanonicalStreamEvent::TextDelta(s) if s == "Hello"));
    }

    #[test]
    fn test_parse_function_call_arguments_delta() {
        let mut state = StreamState::new();
        let events = parse_event(
            r#"event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"loc"}"#,
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::ToolCallDelta { index: 0, arguments_delta }
                if arguments_delta == r#"{"loc"#
        ));
    }

    #[test]
    fn test_parse_response_completed() {
        let mut state = StreamState::new();
        let events = parse_event(
            r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_123","status":"completed","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Done { stop_reason, usage }
                if *stop_reason == StopReason::EndTurn
                    && usage.as_ref().unwrap().input_tokens == 10
                    && usage.as_ref().unwrap().output_tokens == 5
        ));
        assert!(state.done);
    }

    #[test]
    fn test_emit_start_normalizes_chat_id() {
        let mut state = StreamState::new();
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "chatcmpl-abc".into(),
                model: "gpt-4.1".into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains("\"id\":\"resp_abc\""));
        assert_eq!(state.response_id.as_deref(), Some("resp_abc"));
    }

    #[test]
    fn test_emit_start_emits_created_and_in_progress() {
        let mut state = StreamState::new();
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "resp_123".into(),
                model: "gpt-4.1".into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains("event: response.created"));
        assert!(chunks[0].contains("created_at"));
        assert!(chunks[1].contains("event: response.in_progress"));
        assert_eq!(state.response_id.as_deref(), Some("resp_123"));
    }

    #[test]
    fn test_emit_text_delta_first() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Hi".into()),
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].contains("event: response.output_item.added"));
        assert!(chunks[1].contains("event: response.content_part.added"));
        assert!(chunks[2].contains("event: response.output_text.delta"));
        assert!(chunks[2].contains(r#""delta":"Hi""#));
        assert_eq!(state.accumulated_text, "Hi");
    }

    #[test]
    fn test_emit_text_delta_subsequent() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.accumulated_text = "Hello".into();
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta(" world".into()),
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("event: response.output_text.delta"));
    }

    #[test]
    fn test_emit_done() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.model = Some("gpt-4.1".into());
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                }),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("event: response.completed"));
        assert!(chunks[0].contains(r#""status":"completed""#));
        assert!(chunks[0].contains("created_at"));
        assert!(state.done);
    }

    #[test]
    fn test_emit_tool_call_start_accumulates_state() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallStart {
                index: 0,
                id: "call_123".into(),
                name: "shell".into(),
                signature: None,
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("event: response.output_item.added"));
        assert!(chunks[0].contains("function_call"));
        assert!(chunks[0].contains(r#""id":"fc_call_123""#));
        assert!(chunks[0].contains(r#""call_id":"call_123""#));
        assert!(chunks[0].contains(r#""status":"in_progress""#));
        assert!(chunks[0].contains("shell"));
        assert_eq!(state.accumulated_tool_calls.len(), 1);
        assert_eq!(state.accumulated_tool_calls[0].id, "call_123");
        assert_eq!(state.accumulated_tool_calls[0].name, "shell");
        assert_eq!(state.accumulated_tool_calls[0].arguments, "");
    }

    #[test]
    fn test_emit_tool_call_delta_accumulates_arguments() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.accumulated_tool_calls.push(AccumulatedToolCall {
            index: 0,
            id: "call_123".into(),
            name: "shell".into(),
            arguments: String::new(),
        });
        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"cmd":"#.into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("event: response.function_call_arguments.delta"));
        assert!(chunks[0].contains(r#""item_id":"fc_call_123""#));
        assert_eq!(state.accumulated_tool_calls[0].arguments, r#"{"cmd":"#);

        // Second delta
        OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#""ls"}"#.into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(state.accumulated_tool_calls[0].arguments, r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn test_emit_done_with_tool_calls_emits_output_item_done() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.model = Some("gpt-4.1".into());
        state.accumulated_tool_calls.push(AccumulatedToolCall {
            index: 1,
            id: "call_123".into(),
            name: "shell".into(),
            arguments: r#"{"cmd":"ls -la"}"#.into(),
        });

        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Default::default()
                }),
            },
            &mut state,
        )
        .unwrap();

        assert!(
            chunks.len() >= 3,
            "Expected at least 3 events, got {}",
            chunks.len()
        );

        let has_args_done = chunks
            .iter()
            .any(|c| c.contains("response.function_call_arguments.done"));
        assert!(
            has_args_done,
            "Missing response.function_call_arguments.done"
        );

        let has_item_done = chunks
            .iter()
            .any(|c| c.contains("response.output_item.done") && c.contains("function_call"));
        assert!(
            has_item_done,
            "Missing response.output_item.done for function_call"
        );

        let has_completed = chunks.iter().any(|c| c.contains("response.completed"));
        assert!(has_completed, "Missing response.completed");

        let completed_chunk = chunks
            .iter()
            .find(|c| c.contains("response.completed"))
            .unwrap();
        assert!(
            completed_chunk.contains("function_call"),
            "response.completed output missing function_call"
        );
        assert!(
            completed_chunk.contains(r#""id":"fc_call_123""#),
            "response.completed missing item id"
        );
        assert!(
            completed_chunk.contains(r#""call_id":"call_123""#),
            "response.completed missing call_id"
        );
        assert!(
            completed_chunk.contains("shell"),
            "response.completed missing function name"
        );
        assert!(
            completed_chunk.contains(r#"ls -la"#),
            "response.completed missing arguments"
        );
    }

    #[test]
    fn test_emit_done_with_text_and_tool_calls() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.model = Some("gpt-4.1".into());
        state.accumulated_text = "I'll explore the repo.".into();
        state.accumulated_tool_calls.push(AccumulatedToolCall {
            index: 1,
            id: "call_456".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/main.rs"}"#.into(),
        });

        let chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: None,
            },
            &mut state,
        )
        .unwrap();

        // text_done + part_done + output_item.done(msg) + args_done + output_item.done(fc) + completed
        assert!(
            chunks.len() >= 6,
            "Expected at least 6 events, got {}: {:?}",
            chunks.len(),
            chunks
        );

        let msg_done = chunks
            .iter()
            .any(|c| c.contains("response.output_item.done") && c.contains("message"));
        assert!(msg_done, "Missing response.output_item.done for message");

        let fc_done = chunks
            .iter()
            .any(|c| c.contains("response.output_item.done") && c.contains("function_call"));
        assert!(
            fc_done,
            "Missing response.output_item.done for function_call"
        );

        let completed_chunk = chunks
            .iter()
            .find(|c| c.contains("response.completed"))
            .unwrap();
        assert!(
            completed_chunk.contains("message"),
            "completed missing message"
        );
        assert!(
            completed_chunk.contains("function_call"),
            "completed missing function_call"
        );
        assert!(
            completed_chunk.contains(r#""id":"fc_call_456""#),
            "completed missing item id"
        );
        assert!(
            completed_chunk.contains("I'll explore the repo."),
            "completed missing text"
        );
        assert!(
            completed_chunk.contains("read_file"),
            "completed missing function name"
        );
    }

    #[test]
    fn test_full_tool_call_streaming_flow() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_001".into());
        state.model = Some("gpt-4.1".into());

        // 1. Start
        let start_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "resp_001".into(),
                model: "gpt-4.1".into(),
            },
            &mut state,
        )
        .unwrap();
        assert_eq!(
            start_chunks.len(),
            2,
            "Start should emit response.created + response.in_progress"
        );

        // 2. Text delta
        let _ = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Let me check.".into()),
            &mut state,
        )
        .unwrap();

        // 3. Tool call start
        let tc_start = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallStart {
                index: 1,
                id: "call_abc".into(),
                name: "shell".into(),
                signature: None,
            },
            &mut state,
        )
        .unwrap();
        assert!(
            tc_start[0].contains(r#""id":"fc_call_abc""#),
            "output_item.added missing item id"
        );
        assert!(
            tc_start[0].contains(r#""status":"in_progress""#),
            "output_item.added missing status"
        );

        // 4. Tool call deltas
        let delta1 = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallDelta {
                index: 1,
                arguments_delta: r#"{"command""#.into(),
            },
            &mut state,
        )
        .unwrap();
        assert!(
            delta1[0].contains(r#""item_id":"fc_call_abc""#),
            "delta missing item_id"
        );

        let _ = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallDelta {
                index: 1,
                arguments_delta: r#":"ls"}"#.into(),
            },
            &mut state,
        )
        .unwrap();

        // 5. Done with ToolUse
        let done_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Some(Usage {
                    input_tokens: 50,
                    output_tokens: 20,
                    ..Default::default()
                }),
            },
            &mut state,
        )
        .unwrap();

        assert_eq!(state.accumulated_text, "Let me check.");
        assert_eq!(state.accumulated_tool_calls.len(), 1);
        assert_eq!(
            state.accumulated_tool_calls[0].arguments,
            r#"{"command":"ls"}"#
        );

        let fc_item_done = done_chunks
            .iter()
            .find(|c| c.contains("response.output_item.done") && c.contains("function_call"));
        assert!(
            fc_item_done.is_some(),
            "Missing output_item.done for function_call"
        );

        let fc_str = fc_item_done.unwrap();
        assert!(
            fc_str.contains(r#""id":"fc_call_abc""#),
            "output_item.done missing item id"
        );
        assert!(
            fc_str.contains(r#""call_id":"call_abc""#),
            "output_item.done missing call_id"
        );
        assert!(
            fc_str.contains("shell"),
            "output_item.done missing function name"
        );
        assert!(
            fc_str.contains("command") && fc_str.contains("ls"),
            "output_item.done missing arguments"
        );
    }

    #[test]
    fn test_output_index_offset_with_text_and_tool_call() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_idx".into());
        state.model = Some("gpt-4.1".into());

        // Emit start
        let _ = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "resp_idx".into(),
                model: "gpt-4.1".into(),
            },
            &mut state,
        )
        .unwrap();

        // Emit text (occupies output_index 0)
        let _ = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Thinking...".into()),
            &mut state,
        )
        .unwrap();

        // Tool call start (should be at output_index 1, not 0)
        let tc_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallStart {
                index: 0,
                id: "call_idx".into(),
                name: "shell".into(),
                signature: None,
            },
            &mut state,
        )
        .unwrap();
        assert!(
            tc_chunks[0].contains(r#""output_index":1"#),
            "ToolCallStart output_index should be 1 when text precedes, got: {}",
            tc_chunks[0]
        );

        // Tool call delta (should also use output_index 1)
        let delta_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"cmd":"ls"}"#.into(),
            },
            &mut state,
        )
        .unwrap();
        assert!(
            delta_chunks[0].contains(r#""output_index":1"#),
            "ToolCallDelta output_index should be 1 when text precedes, got: {}",
            delta_chunks[0]
        );

        // Done
        let done_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: None,
            },
            &mut state,
        )
        .unwrap();

        // Verify response.completed output array ordering
        let completed = done_chunks
            .iter()
            .find(|c| c.contains("response.completed"))
            .unwrap();
        let data_start = completed.find("data: ").unwrap() + 6;
        let data_json: serde_json::Value =
            serde_json::from_str(completed[data_start..].trim()).unwrap();
        let output = data_json["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 2, "Should have message + function_call");
        assert_eq!(output[0]["type"], "message", "output[0] should be message");
        assert_eq!(
            output[1]["type"], "function_call",
            "output[1] should be function_call"
        );

        // The function_call output_item.done should also use index 1
        let fc_done = done_chunks
            .iter()
            .find(|c| c.contains("response.output_item.done") && c.contains("function_call"))
            .unwrap();
        assert!(
            fc_done.contains(r#""output_index":1"#),
            "Done fc output_index should be 1, got: {}",
            fc_done
        );
    }

    #[test]
    fn test_output_index_no_text_tool_only() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_to".into());
        state.model = Some("gpt-4.1".into());

        let _ = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "resp_to".into(),
                model: "gpt-4.1".into(),
            },
            &mut state,
        )
        .unwrap();

        // No text — tool call should be at output_index 0
        let tc_chunks = OpenAIResponsesStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallStart {
                index: 0,
                id: "call_only".into(),
                name: "shell".into(),
                signature: None,
            },
            &mut state,
        )
        .unwrap();
        assert!(
            tc_chunks[0].contains(r#""output_index":0"#),
            "ToolCallStart output_index should be 0 without text, got: {}",
            tc_chunks[0]
        );
    }

    // ── claudeseal ─────────────────────────────────────────────────────

    fn emit(event: CanonicalStreamEvent, state: &mut StreamState) -> Vec<String> {
        OpenAIResponsesStreamAdapter::emit_sse_event(&event, state).unwrap()
    }

    fn event_data(chunk: &str) -> serde_json::Value {
        let start = chunk.find("data: ").map(|i| i + 6).unwrap_or(0);
        serde_json::from_str(chunk[start..].trim()).unwrap()
    }

    fn sealed_item(chunks: &[String]) -> serde_json::Value {
        let done = chunks
            .iter()
            .find(|c| c.contains("response.output_item.done") && c.contains("reasoning"))
            .expect("a reasoning item done");
        event_data(done)["item"].clone()
    }

    #[test]
    fn test_emit_done_seals_claude_thinking_into_encrypted_content() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        state.model = Some("claude-sonnet-4".into());

        for delta in ["Let me ", "think."] {
            assert!(
                emit(
                    CanonicalStreamEvent::ThinkingDelta(delta.into()),
                    &mut state
                )
                .is_empty()
            );
        }
        for delta in ["sig_", "abc"] {
            assert!(
                emit(
                    CanonicalStreamEvent::ThinkingSignature {
                        by: Format::Claude,
                        delta: delta.into(),
                    },
                    &mut state,
                )
                .is_empty()
            );
        }
        assert_eq!(state.thinking_signature_by, Some(Format::Claude));

        let chunks = emit(
            CanonicalStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
            &mut state,
        );

        let item = sealed_item(&chunks);
        assert_eq!(
            item["summary"],
            serde_json::json!([{"type": "summary_text", "text": "Let me think."}])
        );
        assert_eq!(
            crate::formats::openai_resp::build::open_seal(
                item["encrypted_content"].as_str().unwrap_or_default()
            ),
            Some(serde_json::json!({
                "type": "thinking",
                "thinking": "Let me think.",
                "signature": "sig_abc",
            }))
        );
        let completed = chunks
            .iter()
            .find(|c| c.contains("response.completed"))
            .expect("response.completed");
        assert!(completed.contains("magpie-claude1:"));
    }

    #[test]
    fn test_emit_sealed_thinking_emits_the_block_whole() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        let block = serde_json::json!({"type": "redacted_thinking", "data": "xyz"});

        let chunks = emit(
            CanonicalStreamEvent::SealedThinking {
                by: Format::Claude,
                raw: block.clone(),
            },
            &mut state,
        );

        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains("response.output_item.added"));
        assert!(chunks[0].contains("in_progress"));
        let item = sealed_item(&chunks);
        assert_eq!(item["summary"], serde_json::json!([]));
        assert_eq!(
            crate::formats::openai_resp::build::open_seal(
                item["encrypted_content"].as_str().unwrap_or_default()
            ),
            Some(block)
        );
    }

    #[test]
    fn test_emit_thinking_of_another_vendor_is_left_alone() {
        let mut state = StreamState::new();
        state.response_id = Some("resp_abc".into());
        let chunks = emit(
            CanonicalStreamEvent::ThinkingSignature {
                by: Format::Gemini,
                delta: "gemini_sig".into(),
            },
            &mut state,
        );
        assert!(chunks.is_empty());
        assert!(state.thinking_signature.is_empty());

        let chunks = emit(
            CanonicalStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
            &mut state,
        );
        assert!(!chunks.join("\n").contains("encrypted_content"));
    }
}
