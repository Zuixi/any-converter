use crate::convert::Format;
use crate::error::ConvertError;
use crate::ir::StreamState;
use crate::sse::SseEvent;

mod chat_to_claude;
mod chat_to_gemini;
mod chat_to_resp;
mod claude_to_chat;
mod claude_to_gemini;
mod claude_to_resp;
mod gemini_to_chat;
mod gemini_to_claude;
mod gemini_to_resp;
pub(crate) mod reasoning;
mod resp_to_chat;
mod resp_to_claude;
mod resp_to_gemini;
pub(crate) mod shared;

pub trait FormatConverter: Send + Sync {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError>;
    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError>;
    fn convert_stream_event(
        &self,
        event: &SseEvent,
        state_in: &mut StreamState,
        state_out: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError>;
}

pub fn get_converter(from: Format, to: Format) -> Option<&'static dyn FormatConverter> {
    match (from, to) {
        (Format::Claude, Format::OpenAIChat) => Some(&claude_to_chat::Converter),
        (Format::Claude, Format::OpenAIResponses) => Some(&claude_to_resp::Converter),
        (Format::Claude, Format::Gemini) => Some(&claude_to_gemini::Converter),
        (Format::Gemini, Format::Claude) => Some(&gemini_to_claude::Converter),
        (Format::Gemini, Format::OpenAIChat) => Some(&gemini_to_chat::Converter),
        (Format::Gemini, Format::OpenAIResponses) => Some(&gemini_to_resp::Converter),
        (Format::OpenAIChat, Format::Claude) => Some(&chat_to_claude::Converter),
        (Format::OpenAIChat, Format::OpenAIResponses) => Some(&chat_to_resp::Converter),
        (Format::OpenAIChat, Format::Gemini) => Some(&chat_to_gemini::Converter),
        (Format::OpenAIResponses, Format::Claude) => Some(&resp_to_claude::Converter),
        (Format::OpenAIResponses, Format::OpenAIChat) => Some(&resp_to_chat::Converter),
        (Format::OpenAIResponses, Format::Gemini) => Some(&resp_to_gemini::Converter),
        _ => None,
    }
}
