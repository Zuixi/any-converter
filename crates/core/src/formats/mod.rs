pub mod claude;
pub mod gemini;
pub mod openai_chat;
pub mod openai_resp;

use crate::error::ConvertError;
use crate::ir::{CanonicalStreamEvent, StreamState};

/// Adapter trait for streaming SSE conversion.
/// Handles the stateful transformation of SSE chunks between formats.
pub trait StreamAdapter {
    fn parse_sse_event(
        event: &crate::sse::SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError>;

    fn emit_sse_event(
        event: &CanonicalStreamEvent,
        state: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError>;
}
