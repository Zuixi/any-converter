use crate::error::ConvertError;
use crate::formats::StreamAdapter;
use crate::ir::*;
use crate::sse::{SseEvent, format_sse_data};

use super::types::*;

pub struct GeminiStreamAdapter;

impl StreamAdapter for GeminiStreamAdapter {
    fn parse_sse_event(
        event: &SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
        let chunk: GeminiResponse =
            serde_json::from_str(&event.data).map_err(|e| ConvertError::SseParse(e.to_string()))?;

        let mut events = Vec::new();

        if state.response_id.is_none() {
            let id = uuid::Uuid::new_v4().to_string();
            let model = chunk
                .model_version
                .clone()
                .unwrap_or_else(|| "gemini".into());
            state.response_id = Some(id.clone());
            state.model = Some(model.clone());
            events.push(CanonicalStreamEvent::Start { id, model });
        }

        if let Some(usage) = &chunk.usage_metadata {
            state.accumulated_usage = Some(Usage {
                input_tokens: usage.prompt_token_count.unwrap_or(0),
                output_tokens: usage.candidates_token_count.unwrap_or(0),
                ..Default::default()
            });
        }

        if let Some(candidate) = chunk.candidates.first() {
            for part in &candidate.content.parts {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        state.phase = StreamPhase::Content;
                        events.push(CanonicalStreamEvent::TextDelta(text.clone()));
                    }
                }
                if let Some(fc) = &part.function_call {
                    state.phase = StreamPhase::ToolCalls;
                    let index = state.next_tool_call_index();
                    let id = fc
                        .id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    events.push(CanonicalStreamEvent::ToolCallStart {
                        index,
                        id,
                        name: fc.name.clone(),
                    });
                    if !fc.args.is_null() && fc.args != serde_json::json!({}) {
                        let args_str =
                            serde_json::to_string(&fc.args).map_err(ConvertError::Json)?;
                        events.push(CanonicalStreamEvent::ToolCallDelta {
                            index,
                            arguments_delta: args_str,
                        });
                    }
                }
            }

            if let Some(finish_reason) = &candidate.finish_reason {
                state.done = true;
                state.phase = StreamPhase::Done;
                events.push(CanonicalStreamEvent::Done {
                    stop_reason: StopReason::from_gemini(finish_reason),
                    usage: state.accumulated_usage.clone(),
                });
            }
        }

        Ok(events)
    }

    fn emit_sse_event(
        event: &CanonicalStreamEvent,
        state: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        match event {
            CanonicalStreamEvent::Start { id, model } => {
                state.response_id = Some(id.clone());
                state.model = Some(model.clone());
                Ok(vec![])
            }
            CanonicalStreamEvent::TextDelta(text) => {
                let chunk =
                    make_stream_response(state, vec![GeminiPart::text(text.clone())], None, None)?;
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ToolCallStart { index, id, name } => {
                state.accumulated_tool_calls.push(AccumulatedToolCall {
                    index: *index,
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                let chunk = make_stream_response(
                    state,
                    vec![GeminiPart::function_call_with_id(
                        name.clone(),
                        serde_json::json!({}),
                        id.clone(),
                    )],
                    None,
                    None,
                )?;
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ToolCallDelta {
                index,
                arguments_delta,
            } => {
                let entry = state
                    .accumulated_tool_calls
                    .iter_mut()
                    .find(|tc| tc.index == *index);
                let Some(entry) = entry else {
                    return Ok(vec![]);
                };
                entry.arguments.push_str(arguments_delta);

                let args = match serde_json::from_str::<serde_json::Value>(&entry.arguments) {
                    Ok(v) => v,
                    Err(_) => return Ok(vec![]),
                };
                let name = entry.name.clone();
                let id = entry.id.clone();
                let chunk = make_stream_response(
                    state,
                    vec![GeminiPart::function_call_with_id(name, args, id)],
                    None,
                    None,
                )?;
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ThinkingDelta(_) => Ok(vec![]),
            CanonicalStreamEvent::Done { stop_reason, usage } => {
                state.done = true;
                state.phase = StreamPhase::Done;
                if let Some(u) = usage {
                    state.accumulated_usage = Some(u.clone());
                }
                let usage_metadata = usage.as_ref().map(|u| GeminiUsageMetadata {
                    prompt_token_count: Some(u.input_tokens),
                    candidates_token_count: Some(u.output_tokens),
                    total_token_count: Some(u.input_tokens + u.output_tokens),
                });
                let chunk = make_stream_response(
                    state,
                    vec![],
                    Some(stop_reason.to_gemini().into()),
                    usage_metadata,
                )?;
                Ok(vec![format_sse_data(&chunk)])
            }
        }
    }
}

fn make_stream_response(
    state: &StreamState,
    parts: Vec<GeminiPart>,
    finish_reason: Option<String>,
    usage_metadata: Option<GeminiUsageMetadata>,
) -> Result<String, ConvertError> {
    let model_version = state.model.clone();
    let resp = GeminiResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".into()),
                parts,
            },
            finish_reason,
            index: Some(0),
        }],
        usage_metadata,
        model_version,
    };
    Ok(serde_json::to_string(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_data(data: &str, state: &mut StreamState) -> Vec<CanonicalStreamEvent> {
        let event = SseEvent {
            event: None,
            data: data.into(),
        };
        GeminiStreamAdapter::parse_sse_event(&event, state).unwrap()
    }

    #[test]
    fn test_parse_text_part_chunk() {
        let mut state = StreamState::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]},"index":0}],"modelVersion":"gemini-2.0-flash"}"#;
        let events = parse_data(data, &mut state);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, CanonicalStreamEvent::Start { .. }))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            CanonicalStreamEvent::TextDelta(t) if t == "Hello"
        )));
    }

    #[test]
    fn test_parse_function_call_chunk() {
        let mut state = StreamState::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"q":"rust"},"id":"call_1"}}]},"index":0}]}"#;
        let events = parse_data(data, &mut state);
        assert!(events.iter().any(|e| matches!(
            e,
            CanonicalStreamEvent::ToolCallStart { name, id, .. }
                if name == "search" && id == "call_1"
        )));
    }

    #[test]
    fn test_parse_chunk_with_finish_reason() {
        let mut state = StreamState::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":" done"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#;
        let events = parse_data(data, &mut state);
        assert!(events.iter().any(|e| matches!(
            e,
            CanonicalStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            }
        )));
        assert!(state.done);
    }

    #[test]
    fn test_emit_text_delta_as_partial_response() {
        let mut state = StreamState::new();
        state.model = Some("gemini-2.0-flash".into());

        let output = GeminiStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Hi".into()),
            &mut state,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert!(output[0].starts_with("data: "));
        assert!(output[0].contains(r#""text":"Hi""#));
        assert!(output[0].contains(r#""role":"model""#));
    }

    #[test]
    fn test_emit_done_event() {
        let mut state = StreamState::new();
        state.model = Some("gemini-2.0-flash".into());

        let output = GeminiStreamAdapter::emit_sse_event(
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

        assert_eq!(output.len(), 1);
        assert!(output[0].contains(r#""finishReason":"STOP""#));
        assert!(output[0].contains(r#""promptTokenCount":10"#));
        assert!(!output[0].contains("[DONE]"));
        assert!(state.done);
    }
}
