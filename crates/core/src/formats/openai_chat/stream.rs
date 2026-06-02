use crate::error::ConvertError;
use crate::formats::StreamAdapter;
use crate::ir::*;
use crate::sse::{format_sse_data, is_openai_done, SseEvent};

use super::types::*;

pub struct OpenAIChatStreamAdapter;

impl StreamAdapter for OpenAIChatStreamAdapter {
    fn parse_sse_event(
        event: &SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
        if is_openai_done(&event.data) {
            state.done = true;
            state.phase = StreamPhase::Done;
            let stop_reason = match state.block_index {
                1 => StopReason::EndTurn,
                2 => StopReason::MaxTokens,
                3 => StopReason::ToolUse,
                4 => StopReason::ContentFilter,
                _ => StopReason::EndTurn,
            };
            return Ok(vec![CanonicalStreamEvent::Done {
                stop_reason,
                usage: state.accumulated_usage.clone(),
            }]);
        }

        let chunk: OpenAIChatStreamChunk = serde_json::from_str(&event.data)
            .map_err(|e| ConvertError::SseParse(e.to_string()))?;

        let mut events = Vec::new();

        if state.response_id.is_none() {
            state.response_id = Some(chunk.id.clone());
            state.model = Some(chunk.model.clone());
            events.push(CanonicalStreamEvent::Start {
                id: chunk.id.clone(),
                model: chunk.model.clone(),
            });
        }

        if let Some(usage) = chunk.usage {
            state.accumulated_usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                ..Default::default()
            });
        }

        for choice in &chunk.choices {
            if let Some(content) = &choice.delta.content {
                if !content.is_empty() {
                    state.phase = StreamPhase::Content;
                    events.push(CanonicalStreamEvent::TextDelta(content.clone()));
                }
            }

            if let Some(tool_calls) = &choice.delta.tool_calls {
                state.phase = StreamPhase::ToolCalls;
                for tc in tool_calls {
                    if let Some(id) = &tc.id {
                        if let Some(func) = &tc.function {
                            if let Some(name) = &func.name {
                                events.push(CanonicalStreamEvent::ToolCallStart {
                                    index: tc.index,
                                    id: id.clone(),
                                    name: name.clone(),
                                });
                            }
                        }
                    }
                    if let Some(func) = &tc.function {
                        if let Some(args) = &func.arguments {
                            if !args.is_empty() {
                                events.push(CanonicalStreamEvent::ToolCallDelta {
                                    index: tc.index,
                                    arguments_delta: args.clone(),
                                });
                            }
                        }
                    }
                }
            }

            if let Some(finish_reason) = &choice.finish_reason {
                state.block_index = match finish_reason.as_str() {
                    "stop" => 1,
                    "length" => 2,
                    "tool_calls" => 3,
                    "content_filter" => 4,
                    _ => 1,
                };
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
                let chunk = make_stream_chunk(
                    id,
                    model,
                    Delta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                    },
                    None,
                    None,
                );
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::TextDelta(text) => {
                let (id, model) = stream_ids(state)?;
                let chunk = make_stream_chunk(
                    &id,
                    &model,
                    Delta {
                        role: None,
                        content: Some(text.clone()),
                        tool_calls: None,
                    },
                    None,
                    None,
                );
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ToolCallStart { index, id, name } => {
                let (resp_id, model) = stream_ids(state)?;
                let chunk = make_stream_chunk(
                    &resp_id,
                    &model,
                    Delta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![StreamToolCall {
                            index: *index,
                            id: Some(id.clone()),
                            r#type: Some("function".into()),
                            function: Some(StreamFunctionCall {
                                name: Some(name.clone()),
                                arguments: None,
                            }),
                        }]),
                    },
                    None,
                    None,
                );
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ToolCallDelta {
                index,
                arguments_delta,
            } => {
                let (id, model) = stream_ids(state)?;
                let chunk = make_stream_chunk(
                    &id,
                    &model,
                    Delta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![StreamToolCall {
                            index: *index,
                            id: None,
                            r#type: None,
                            function: Some(StreamFunctionCall {
                                name: None,
                                arguments: Some(arguments_delta.clone()),
                            }),
                        }]),
                    },
                    None,
                    None,
                );
                Ok(vec![format_sse_data(&chunk)])
            }
            CanonicalStreamEvent::ThinkingDelta(_) => Ok(vec![]),
            CanonicalStreamEvent::Done { stop_reason, usage } => {
                let (id, model) = stream_ids(state)?;
                state.done = true;
                state.phase = StreamPhase::Done;
                if let Some(u) = usage {
                    state.accumulated_usage = Some(u.clone());
                }
                let mut output = Vec::new();
                let finish_chunk = make_stream_chunk(
                    &id,
                    &model,
                    Delta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    Some(stop_reason.to_openai_chat().into()),
                    usage.as_ref().map(|u| OpenAIUsage {
                        prompt_tokens: u.input_tokens,
                        completion_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                    }),
                );
                output.push(format_sse_data(&finish_chunk));
                output.push(format_sse_data("[DONE]"));
                Ok(output)
            }
        }
    }
}

fn stream_ids(state: &StreamState) -> Result<(String, String), ConvertError> {
    let id = state
        .response_id
        .clone()
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4()));
    let model = state
        .model
        .clone()
        .unwrap_or_else(|| "gpt-4o".into());
    Ok((id, model))
}

fn make_stream_chunk(
    id: &str,
    model: &str,
    delta: Delta,
    finish_reason: Option<String>,
    usage: Option<OpenAIUsage>,
) -> String {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let chunk = OpenAIChatStreamChunk {
        id: id.into(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.into(),
        choices: vec![StreamChoice {
            index: 0,
            delta,
            finish_reason,
        }],
        usage,
    };
    serde_json::to_string(&chunk).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::parse_sse_block;

    fn parse_data(data: &str, state: &mut StreamState) -> Vec<CanonicalStreamEvent> {
        let event = SseEvent {
            event: None,
            data: data.into(),
        };
        OpenAIChatStreamAdapter::parse_sse_event(&event, state).unwrap()
    }

    #[test]
    fn test_parse_text_delta_chunk() {
        let mut state = StreamState::new();
        let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let events = parse_data(data, &mut state);
        assert!(events.iter().any(|e| matches!(e, CanonicalStreamEvent::Start { .. })));
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], CanonicalStreamEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn test_parse_tool_call_start_and_delta() {
        let mut state = StreamState::new();

        let start_data = r#"{"id":"chatcmpl-2","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":""}}]},"finish_reason":null}]}"#;
        let start_events = parse_data(start_data, &mut state);
        assert!(start_events.iter().any(|e| matches!(
            e,
            CanonicalStreamEvent::ToolCallStart { id, name, .. }
                if id == "call_1" && name == "search"
        )));

        let delta_data = r#"{"id":"chatcmpl-2","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#;
        let delta_events = parse_data(delta_data, &mut state);
        assert!(matches!(
            &delta_events[0],
            CanonicalStreamEvent::ToolCallDelta { arguments_delta, .. }
                if arguments_delta == r#"{"q":"#
        ));
    }

    #[test]
    fn test_parse_finish_reason_chunk() {
        let mut state = StreamState::new();
        let data = r#"{"id":"chatcmpl-3","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        parse_data(data, &mut state);
        assert_eq!(state.block_index, 1);
    }

    #[test]
    fn test_parse_done_sentinel() {
        let mut state = StreamState::new();
        state.block_index = 3; // tool_calls finish reason
        let events = parse_data("[DONE]", &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            CanonicalStreamEvent::Done { stop_reason: StopReason::ToolUse, .. }
        ));
        assert!(state.done);
    }

    #[test]
    fn test_emit_text_delta() {
        let mut state = StreamState::new();
        state.response_id = Some("chatcmpl-emit".into());
        state.model = Some("gpt-4o".into());

        let output = OpenAIChatStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::TextDelta("Hi".into()),
            &mut state,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert!(output[0].starts_with("data: "));
        assert!(output[0].contains(r#""content":"Hi""#));
    }

    #[test]
    fn test_emit_done_with_sentinel() {
        let mut state = StreamState::new();
        state.response_id = Some("chatcmpl-done".into());
        state.model = Some("gpt-4o".into());

        let output = OpenAIChatStreamAdapter::emit_sse_event(
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

        assert_eq!(output.len(), 2);
        assert!(output[0].contains(r#""finish_reason":"stop""#));
        assert!(output[1].contains("[DONE]"));
        assert!(state.done);
    }

    #[test]
    fn test_emit_tool_call_start() {
        let mut state = StreamState::new();
        state.response_id = Some("chatcmpl-tc".into());
        state.model = Some("gpt-4o".into());

        let output = OpenAIChatStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::ToolCallStart {
                index: 0,
                id: "call_abc".into(),
                name: "get_weather".into(),
            },
            &mut state,
        )
        .unwrap();

        assert_eq!(output.len(), 1);
        assert!(output[0].contains(r#""id":"call_abc""#));
        assert!(output[0].contains(r#""name":"get_weather""#));
    }

    #[test]
    fn test_roundtrip_sse_block_parse() {
        let mut state = StreamState::new();
        state.response_id = Some("chatcmpl-rt".into());
        state.model = Some("gpt-4o".into());

        let emitted = OpenAIChatStreamAdapter::emit_sse_event(
            &CanonicalStreamEvent::Start {
                id: "chatcmpl-rt".into(),
                model: "gpt-4o".into(),
            },
            &mut state,
        )
        .unwrap();

        let block = emitted[0].trim();
        let event = parse_sse_block(block).unwrap();
        let parsed = OpenAIChatStreamAdapter::parse_sse_event(&event, &mut StreamState::new()).unwrap();
        assert!(parsed.iter().any(|e| matches!(e, CanonicalStreamEvent::Start { .. })));
    }
}
