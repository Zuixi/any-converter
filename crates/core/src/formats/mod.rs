pub mod claude;
pub mod gemini;
pub mod openai_chat;
pub mod openai_resp;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::ConvertError;
use crate::ir::{CanonicalRequest, CanonicalResponse, CanonicalStreamEvent, StreamState};

/// Adapter trait for non-streaming request/response conversion.
/// Each LLM API format implements this to convert between its wire types and the canonical IR.
pub trait FormatAdapter {
    type Request: DeserializeOwned + Serialize;
    type Response: DeserializeOwned + Serialize;

    /// Deserialize raw JSON bytes into the format's request type.
    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError>;

    /// Convert format-specific request → canonical IR.
    fn request_to_canonical(req: Self::Request) -> Result<CanonicalRequest, ConvertError>;

    /// Convert canonical IR → format-specific request.
    fn request_from_canonical(req: &CanonicalRequest) -> Result<Self::Request, ConvertError>;

    /// Deserialize raw JSON bytes into the format's response type.
    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError>;

    /// Convert format-specific response → canonical IR.
    fn response_to_canonical(resp: Self::Response) -> Result<CanonicalResponse, ConvertError>;

    /// Convert canonical IR → format-specific response.
    fn response_from_canonical(resp: &CanonicalResponse) -> Result<Self::Response, ConvertError>;
}

/// Adapter trait for streaming SSE conversion.
/// Handles the stateful transformation of SSE chunks between formats.
pub trait StreamAdapter {
    /// Parse a single SSE event block into canonical stream events.
    fn parse_sse_event(
        event: &crate::sse::SseEvent,
        state: &mut StreamState,
    ) -> Result<Vec<CanonicalStreamEvent>, ConvertError>;

    /// Emit a canonical stream event as format-specific SSE text.
    fn emit_sse_event(
        event: &CanonicalStreamEvent,
        state: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError>;
}
