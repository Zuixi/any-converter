use serde::{Deserialize, Serialize};

use super::{StopReason, Usage};

/// Canonical stream event — format-agnostic representation of SSE deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CanonicalStreamEvent {
    Start { id: String, model: String },
    TextDelta(String),
    ToolCallStart { index: u32, id: String, name: String },
    ToolCallDelta { index: u32, arguments_delta: String },
    ThinkingDelta(String),
    Done { stop_reason: StopReason, usage: Option<Usage> },
}

/// Accumulated tool call data during streaming.
#[derive(Debug, Clone, Default)]
pub struct AccumulatedToolCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Tracks conversion state across SSE chunks.
#[derive(Debug, Clone, Default)]
pub struct StreamState {
    pub response_id: Option<String>,
    pub model: Option<String>,
    pub block_index: u32,
    pub tool_call_index: u32,
    pub accumulated_usage: Option<Usage>,
    pub accumulated_text: String,
    pub accumulated_tool_calls: Vec<AccumulatedToolCall>,
    pub phase: StreamPhase,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum StreamPhase {
    #[default]
    Init,
    Content,
    ToolCalls,
    Done,
}

impl StreamState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_block_index(&mut self) -> u32 {
        let idx = self.block_index;
        self.block_index += 1;
        idx
    }

    pub fn next_tool_call_index(&mut self) -> u32 {
        let idx = self.tool_call_index;
        self.tool_call_index += 1;
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_state_default() {
        let state = StreamState::new();
        assert_eq!(state.phase, StreamPhase::Init);
        assert_eq!(state.block_index, 0);
        assert!(!state.done);
    }

    #[test]
    fn test_stream_state_index_increment() {
        let mut state = StreamState::new();
        assert_eq!(state.next_block_index(), 0);
        assert_eq!(state.next_block_index(), 1);
        assert_eq!(state.next_tool_call_index(), 0);
        assert_eq!(state.next_tool_call_index(), 1);
    }

    #[test]
    fn test_canonical_stream_event_variants() {
        let start = CanonicalStreamEvent::Start {
            id: "resp_1".into(),
            model: "gpt-4".into(),
        };
        assert!(matches!(start, CanonicalStreamEvent::Start { .. }));

        let delta = CanonicalStreamEvent::TextDelta("Hello".into());
        assert!(matches!(delta, CanonicalStreamEvent::TextDelta(_)));

        let done = CanonicalStreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() }),
        };
        if let CanonicalStreamEvent::Done { stop_reason, usage } = &done {
            assert_eq!(stop_reason, &StopReason::EndTurn);
            assert!(usage.is_some());
        }
    }

    #[test]
    fn test_stream_event_serialization() {
        let event = CanonicalStreamEvent::ToolCallStart {
            index: 0,
            id: "call_abc".into(),
            name: "search".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: CanonicalStreamEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}
