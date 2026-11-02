use crate::convert::Format;
use crate::error::ConvertError;
use crate::formats::StreamAdapter;
use crate::formats::claude::{ClaudeRequest, ClaudeResponse, ClaudeStreamAdapter};
use crate::formats::gemini::{GeminiRequest, GeminiResponse, GeminiStreamAdapter};
use crate::formats::openai_chat::{OpenAIChatRequest, OpenAIChatResponse, OpenAIChatStreamAdapter};
use crate::formats::openai_resp::{
    OpenAIResponsesRequest, OpenAIResponsesResponse, OpenAIResponsesStreamAdapter,
};
use crate::ir::{CanonicalStreamEvent, IrRequest, IrResponse, StreamState};
use crate::sse::SseEvent;

pub(crate) mod reasoning;
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

/// One wire→IR→wire pair. The semantics live in each format's
/// `read`/`build`; this struct only dispatches (docs/design/ir-hub-conversion.md §4).
struct HubConverter {
    from: Format,
    to: Format,
}

fn read_request(from: Format, body: &[u8]) -> Result<IrRequest, ConvertError> {
    Ok(match from {
        Format::Claude => {
            crate::formats::claude::read::read(serde_json::from_slice::<ClaudeRequest>(body)?)
        }
        Format::OpenAIChat => crate::formats::openai_chat::read::read(serde_json::from_slice::<
            OpenAIChatRequest,
        >(body)?),
        Format::OpenAIResponses => crate::formats::openai_resp::read::read(
            serde_json::from_slice::<OpenAIResponsesRequest>(body)?,
        ),
        Format::Gemini => {
            crate::formats::gemini::read::read(serde_json::from_slice::<GeminiRequest>(body)?)
        }
    })
}

fn build_request(to: Format, ir: IrRequest) -> Result<Vec<u8>, ConvertError> {
    // serialize the typed struct: a Value detour widens f32 to f64 and
    // 0.7 comes back as 0.699999988079071
    Ok(match to {
        Format::Claude => serde_json::to_vec(&crate::formats::claude::build::build(ir))?,
        Format::OpenAIChat => serde_json::to_vec(&crate::formats::openai_chat::build::build(ir))?,
        Format::OpenAIResponses => {
            serde_json::to_vec(&crate::formats::openai_resp::build::build(ir))?
        }
        Format::Gemini => serde_json::to_vec(&crate::formats::gemini::build::build(ir))?,
    })
}

fn read_response(from: Format, body: &[u8]) -> Result<IrResponse, ConvertError> {
    Ok(match from {
        Format::Claude => crate::formats::claude::read::read_response(serde_json::from_slice::<
            ClaudeResponse,
        >(body)?),
        Format::OpenAIChat => {
            crate::formats::openai_chat::read::read_response(serde_json::from_slice::<
                OpenAIChatResponse,
            >(body)?)
        }
        Format::OpenAIResponses => {
            crate::formats::openai_resp::read::read_response(serde_json::from_slice::<
                OpenAIResponsesResponse,
            >(body)?)
        }
        Format::Gemini => crate::formats::gemini::read::read_response(serde_json::from_slice::<
            GeminiResponse,
        >(body)?),
    })
}

fn build_response(to: Format, ir: IrResponse) -> Result<Vec<u8>, ConvertError> {
    Ok(match to {
        Format::Claude => serde_json::to_vec(&crate::formats::claude::build::build_response(ir))?,
        Format::OpenAIChat => {
            serde_json::to_vec(&crate::formats::openai_chat::build::build_response(ir))?
        }
        Format::OpenAIResponses => {
            serde_json::to_vec(&crate::formats::openai_resp::build::build_response(ir))?
        }
        Format::Gemini => serde_json::to_vec(&crate::formats::gemini::build::build_response(ir))?,
    })
}

fn parse_stream(
    from: Format,
    event: &SseEvent,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, ConvertError> {
    Ok(match from {
        Format::Claude => ClaudeStreamAdapter::parse_sse_event(event, state)?,
        Format::OpenAIChat => OpenAIChatStreamAdapter::parse_sse_event(event, state)?,
        Format::OpenAIResponses => OpenAIResponsesStreamAdapter::parse_sse_event(event, state)?,
        Format::Gemini => GeminiStreamAdapter::parse_sse_event(event, state)?,
    })
}

fn emit_stream(
    to: Format,
    event: &CanonicalStreamEvent,
    state: &mut StreamState,
) -> Result<Vec<String>, ConvertError> {
    Ok(match to {
        Format::Claude => ClaudeStreamAdapter::emit_sse_event(event, state)?,
        Format::OpenAIChat => OpenAIChatStreamAdapter::emit_sse_event(event, state)?,
        Format::OpenAIResponses => OpenAIResponsesStreamAdapter::emit_sse_event(event, state)?,
        Format::Gemini => GeminiStreamAdapter::emit_sse_event(event, state)?,
    })
}

impl FormatConverter for HubConverter {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        build_request(self.to, read_request(self.from, input)?)
    }

    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError> {
        build_response(self.to, read_response(self.from, input)?)
    }

    fn convert_stream_event(
        &self,
        event: &SseEvent,
        state_in: &mut StreamState,
        state_out: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError> {
        let canonical = parse_stream(self.from, event, state_in)?;
        let mut lines = Vec::new();
        for ce in &canonical {
            lines.extend(emit_stream(self.to, ce, state_out)?);
        }
        Ok(lines)
    }
}

macro_rules! hub {
    ($name:ident, $from:expr, $to:expr) => {
        static $name: HubConverter = HubConverter {
            from: $from,
            to: $to,
        };
    };
}

hub!(CLAUDE_TO_CHAT, Format::Claude, Format::OpenAIChat);
hub!(CLAUDE_TO_RESP, Format::Claude, Format::OpenAIResponses);
hub!(CLAUDE_TO_GEMINI, Format::Claude, Format::Gemini);
hub!(CHAT_TO_CLAUDE, Format::OpenAIChat, Format::Claude);
hub!(CHAT_TO_RESP, Format::OpenAIChat, Format::OpenAIResponses);
hub!(CHAT_TO_GEMINI, Format::OpenAIChat, Format::Gemini);
hub!(RESP_TO_CLAUDE, Format::OpenAIResponses, Format::Claude);
hub!(RESP_TO_CHAT, Format::OpenAIResponses, Format::OpenAIChat);
hub!(RESP_TO_GEMINI, Format::OpenAIResponses, Format::Gemini);
hub!(GEMINI_TO_CLAUDE, Format::Gemini, Format::Claude);
hub!(GEMINI_TO_CHAT, Format::Gemini, Format::OpenAIChat);
hub!(GEMINI_TO_RESP, Format::Gemini, Format::OpenAIResponses);

pub fn get_converter(from: Format, to: Format) -> Option<&'static dyn FormatConverter> {
    match (from, to) {
        (Format::Claude, Format::OpenAIChat) => Some(&CLAUDE_TO_CHAT),
        (Format::Claude, Format::OpenAIResponses) => Some(&CLAUDE_TO_RESP),
        (Format::Claude, Format::Gemini) => Some(&CLAUDE_TO_GEMINI),
        (Format::OpenAIChat, Format::Claude) => Some(&CHAT_TO_CLAUDE),
        (Format::OpenAIChat, Format::OpenAIResponses) => Some(&CHAT_TO_RESP),
        (Format::OpenAIChat, Format::Gemini) => Some(&CHAT_TO_GEMINI),
        (Format::OpenAIResponses, Format::Claude) => Some(&RESP_TO_CLAUDE),
        (Format::OpenAIResponses, Format::OpenAIChat) => Some(&RESP_TO_CHAT),
        (Format::OpenAIResponses, Format::Gemini) => Some(&RESP_TO_GEMINI),
        (Format::Gemini, Format::Claude) => Some(&GEMINI_TO_CLAUDE),
        (Format::Gemini, Format::OpenAIChat) => Some(&GEMINI_TO_CHAT),
        (Format::Gemini, Format::OpenAIResponses) => Some(&GEMINI_TO_RESP),
        _ => None,
    }
}
