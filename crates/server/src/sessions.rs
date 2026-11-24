//! Sessions as the gateway sees them: one conversation is `client_id` plus the
//! `session_id` the client sent. A request that named neither is a request, not
//! a session, and is never guessed into one.
//!
//! The list reads only `request_logs`. The transcript reads `request_bodies` for
//! the one session that was opened, and only the part of each turn that was not
//! already said.
//!
//! Bodies are read through the core IR rather than through the trace previews:
//! the previews are cut at `trace_max_preview_bytes`, while the captured body
//! holds the whole turn, including thinking, images, and tool arguments. A body
//! that was captured truncated falls back to the trace previews.

use any_converter_core::convert::Format;
use any_converter_core::formats::{StreamAdapter, claude, gemini, openai_chat, openai_resp};
use any_converter_core::ir::{
    CanonicalStreamEvent, IrMessage, IrPart, IrRequest, Role, StreamState,
};
use any_converter_core::sse::{parse_sse_block, split_sse_blocks};
use serde::Serialize;
use serde_json::Value;

use crate::request_log::{RequestLogRecord, ResponseBodyKind};

/// How many sessions one page of the list holds.
pub const SESSION_PAGE: u64 = 50;
/// Turns kept at the end of a transcript. Older ones are counted, then dropped.
const TRANSCRIPT_TURNS: usize = 128;
/// Characters of every part together. Past this the transcript is marked cut.
const TRANSCRIPT_CHARS: usize = 400_000;
/// Characters of one part. The rest is counted in [`Part::cut`].
const PART_CHARS: usize = 64 * 1024;
/// Characters kept of a session's title.
const TITLE_CHARS: usize = 80;

/// One row of the session list. Tokens and cost are the sum of its requests.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub client_id: String,
    pub session_id: String,
    pub title: String,
    pub started: String,
    pub last: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    /// Requests in the session whose status was 4xx or 5xx.
    pub errors: u64,
    pub model: String,
}

/// A page of sessions, newest activity first.
#[derive(Debug, Clone, Serialize)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub total: u64,
}

/// What was said in one session, in order, with replayed history removed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Transcript {
    pub turns: Vec<Turn>,
    /// A turn or a character was left off.
    pub cut: bool,
    /// Turns the bodies still hold. Less than the session's requests once
    /// bodies have aged out.
    pub captured: usize,
}

/// One request: what the client said that was new, what came back, and how.
#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub request_id: String,
    pub timestamp: String,
    pub model: String,
    pub provider: String,
    pub status: u16,
    /// Time to the first byte, when the reply streamed.
    pub ttft_ms: u64,
    /// How long the reply's content took to arrive.
    pub flow_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub parts: Vec<Part>,
}

/// One thing said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Part {
    /// `system`, `user`, `assistant`, or `tool`.
    pub role: String,
    /// `text`, `thinking`, `tool_use`, `tool_result`, or `image`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool call / result id, so a call and its answer can be matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub is_error: bool,
    pub text: String,
    /// Characters cut off the end of `text`.
    #[serde(skip_serializing_if = "is_zero")]
    pub cut: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The first thing the user said in this request, short enough for a list row.
/// `None` when the trace has no user text, so an earlier title is kept.
pub fn session_title(record: &RequestLogRecord) -> Option<String> {
    let trace = record.trace.as_ref()?;
    trace
        .client
        .messages
        .iter()
        .find(|message| message.role == "user" && !message.content_preview.trim().is_empty())
        .map(|message| compact(&message.content_preview, TITLE_CHARS))
}

/// Folds the records of one session into the turns the page draws.
///
/// `records` is oldest first. A system prompt is said again only when it
/// changes, and a turn whose input continues the previous turn says only the
/// new part. Equal text found later in the conversation is kept: only a prefix
/// is a replay.
pub fn fold_transcript(records: &[RequestLogRecord]) -> Transcript {
    let mut out = Transcript::default();
    let kept = if records.len() > TRANSCRIPT_TURNS {
        out.cut = true;
        &records[records.len() - TRANSCRIPT_TURNS..]
    } else {
        records
    };

    let mut spent = 0usize;
    let mut system: Vec<IrPart> = Vec::new();
    let mut previous: Vec<MsgKey> = Vec::new();

    for record in kept {
        out.captured += 1;
        let format = Format::parse(&record.client_format).ok();
        let (parsed, context, messages) = read_request(record, format);
        let mut turn = Turn {
            request_id: record.request_id.clone(),
            timestamp: record.timestamp.clone(),
            model: record
                .served_model
                .clone()
                .unwrap_or_else(|| record.upstream_model.clone()),
            provider: record.provider.clone(),
            status: record.response_status,
            ttft_ms: record.ttft_ms,
            flow_ms: record.flow_ms,
            input_tokens: record.usage.input_tokens,
            output_tokens: record.usage.output_tokens,
            cache_read_tokens: record.usage.cache_read_tokens.unwrap_or(0),
            cost_usd: record.cost_usd,
            error: record.error.clone(),
            parts: Vec::new(),
        };
        let mut parts = Vec::new();
        let mut full = true;

        'fill: {
            if context != system {
                for part in &context {
                    if !add(
                        &mut parts,
                        &mut spent,
                        &mut out.cut,
                        ir_part(part, "system"),
                    ) {
                        full = false;
                        break 'fill;
                    }
                }
                system.clone_from(&context);
            }

            // The client replays the conversation it was shown. A reply is that
            // only when the next request carries it back as a message.
            let keys: Vec<MsgKey> = messages.iter().map(message_key).collect();
            let fresh = if !previous.is_empty()
                && keys.len() >= previous.len()
                && keys[..previous.len()] == previous[..]
            {
                &messages[previous.len()..]
            } else {
                &messages[..]
            };
            for message in fresh {
                let role = role_name(message.role);
                for part in &message.parts {
                    if !add(&mut parts, &mut spent, &mut out.cut, ir_part(part, role)) {
                        full = false;
                        break 'fill;
                    }
                }
            }

            let response = read_response(record, format);
            let mut response_keys = response.keys;
            // The trace previews are what a fallback request will replay, so
            // stay in that vocabulary when the body itself could not be read.
            if !parsed {
                let trace_keys = trace_response_keys(record);
                if !trace_keys.is_empty() {
                    response_keys = trace_keys;
                }
            }
            for part in response.parts {
                if !add(&mut parts, &mut spent, &mut out.cut, Some(part)) {
                    full = false;
                    break 'fill;
                }
            }
            previous = keys;
            if !response_keys.is_empty() {
                previous.push(MsgKey {
                    role: Role::Assistant,
                    parts: response_keys,
                });
            }
        }

        turn.parts = parts;
        if !turn.parts.is_empty() || turn.status >= 400 || turn.error.is_some() {
            out.turns.push(turn);
        }
        if !full {
            return out;
        }
    }
    out
}

/// Appends `part` unless the transcript's character budget is spent. `None`
/// parts are said nothing.
fn add(parts: &mut Vec<Part>, spent: &mut usize, cut: &mut bool, part: Option<Part>) -> bool {
    let Some(part) = part else {
        return true;
    };
    if *spent >= TRANSCRIPT_CHARS {
        *cut = true;
        return false;
    }
    let part = bound(part, cut);
    *spent += part.text.len();
    parts.push(part);
    true
}

/// A message as the replay comparison sees it: signatures are ignored, since a
/// client may echo a signature and an API may re-sign the same text.
#[derive(Debug, Clone, PartialEq)]
struct MsgKey {
    role: Role,
    parts: Vec<PartKey>,
}

#[derive(Debug, Clone, PartialEq)]
enum PartKey {
    Text(String),
    Thinking(String),
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        id: String,
        is_error: bool,
        text: String,
    },
    Image(String),
}

fn message_key(message: &IrMessage) -> MsgKey {
    MsgKey {
        role: message.role,
        parts: message.parts.iter().filter_map(part_key).collect(),
    }
}

fn part_key(part: &IrPart) -> Option<PartKey> {
    match part {
        IrPart::Text { text, .. } => Some(PartKey::Text(text.clone())),
        IrPart::Thinking { text, .. } => Some(PartKey::Thinking(text.clone())),
        IrPart::Sealed { .. } => Some(PartKey::Thinking(String::new())),
        IrPart::ToolCall { id, name, args, .. } => Some(PartKey::ToolCall {
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        }),
        IrPart::ToolResult {
            call_id,
            is_error,
            content,
        } => Some(PartKey::ToolResult {
            id: call_id.clone(),
            is_error: *is_error,
            text: content_text(content),
        }),
        IrPart::Image {
            url,
            media_type,
            data,
            ..
        } => Some(PartKey::Image(image_key(url, media_type, data.as_deref()))),
    }
}

fn display_key(part: &Part) -> PartKey {
    match part.kind.as_str() {
        "tool_use" => PartKey::ToolCall {
            id: part.id.clone().unwrap_or_default(),
            name: part.name.clone().unwrap_or_default(),
            args: serde_json::from_str(&part.text)
                .unwrap_or_else(|_| Value::String(part.text.clone())),
        },
        "tool_result" => PartKey::ToolResult {
            id: part.id.clone().unwrap_or_default(),
            is_error: part.is_error,
            text: part.text.clone(),
        },
        "thinking" => PartKey::Thinking(part.text.clone()),
        "image" => PartKey::Image(part.text.clone()),
        _ => PartKey::Text(part.text.clone()),
    }
}

/// The request's system prompt and messages, from the captured body when it
/// parses, from the trace previews otherwise. The flag says which won.
fn read_request(
    record: &RequestLogRecord,
    format: Option<Format>,
) -> (bool, Vec<IrPart>, Vec<IrMessage>) {
    if let (Some(format), Some(body)) = (format, record.request_body.as_ref()) {
        if let Some(request) = read_request_json(format, &body.text) {
            return (true, request.system, request.messages);
        }
    }
    let (system, messages) = trace_request(record);
    (false, system, messages)
}

fn read_request_json(format: Format, text: &str) -> Option<IrRequest> {
    match format {
        Format::Claude => serde_json::from_str::<claude::ClaudeRequest>(text)
            .ok()
            .map(claude::read::read),
        Format::OpenAIChat => serde_json::from_str::<openai_chat::OpenAIChatRequest>(text)
            .ok()
            .map(openai_chat::read::read),
        Format::OpenAIResponses => {
            serde_json::from_str::<openai_resp::OpenAIResponsesRequest>(text)
                .ok()
                .map(openai_resp::read::read)
        }
        Format::Gemini => serde_json::from_str::<gemini::GeminiRequest>(text)
            .ok()
            .map(gemini::read::read),
    }
}

fn trace_request(record: &RequestLogRecord) -> (Vec<IrPart>, Vec<IrMessage>) {
    let Some(trace) = record.trace.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &trace.client.messages {
        if message.content_preview.trim().is_empty() {
            continue;
        }
        let part = IrPart::Text {
            text: message.content_preview.clone(),
            signature: None,
        };
        if message.role == "system" || message.role == "developer" {
            system.push(part);
        } else {
            messages.push(IrMessage {
                role: if message.role == "assistant" {
                    Role::Assistant
                } else {
                    Role::User
                },
                parts: vec![part],
            });
        }
    }
    for call in &trace.client.tool_calls {
        messages.push(IrMessage {
            role: Role::Assistant,
            parts: vec![IrPart::ToolCall {
                id: call.id.clone().unwrap_or_default(),
                name: tool_name(call.namespace.as_deref(), &call.name),
                args: parse_args(&call.arguments_preview),
                signature: None,
            }],
        });
    }
    for result in &trace.client.tool_results {
        messages.push(IrMessage {
            role: Role::User,
            parts: vec![IrPart::ToolResult {
                call_id: result.id.clone().unwrap_or_default(),
                is_error: false,
                content: vec![IrPart::Text {
                    text: result.content_preview.clone(),
                    signature: None,
                }],
            }],
        });
    }
    (system, messages)
}

/// What the reply said, from the captured response when it parses.
struct Response {
    parts: Vec<Part>,
    keys: Vec<PartKey>,
}

fn read_response(record: &RequestLogRecord, format: Option<Format>) -> Response {
    let parsed = match (&record.response_body, format) {
        (ResponseBodyKind::Json { text }, Some(format)) => read_json_response(format, text),
        (ResponseBodyKind::SseLines { lines }, Some(format)) => Some(fold_stream(format, lines)),
        _ => None,
    };
    if let Some(response) = parsed {
        return response;
    }
    match &record.response_body {
        ResponseBodyKind::Json { text } => raw_response(text),
        ResponseBodyKind::SseLines { lines } => {
            let joined = lines.join("\n");
            if joined.trim().is_empty() {
                trace_response(record)
            } else {
                raw_response(&joined)
            }
        }
    }
}

fn read_json_response(format: Format, text: &str) -> Option<Response> {
    let parts = match format {
        Format::Claude => {
            let resp: claude::ClaudeResponse = serde_json::from_str(text).ok()?;
            claude::read::read_response(resp).parts
        }
        Format::OpenAIChat => {
            let resp: openai_chat::OpenAIChatResponse = serde_json::from_str(text).ok()?;
            openai_chat::read::read_response(resp).parts
        }
        Format::OpenAIResponses => {
            let resp: openai_resp::OpenAIResponsesResponse = serde_json::from_str(text).ok()?;
            openai_resp::read::read_response(resp).parts
        }
        Format::Gemini => {
            let resp: gemini::GeminiResponse = serde_json::from_str(text).ok()?;
            gemini::read::read_response(resp).parts
        }
    };
    Some(response_from_parts(&parts))
}

fn response_from_parts(parts: &[IrPart]) -> Response {
    let mut said = Vec::new();
    let mut keys = Vec::new();
    for part in parts {
        if let Some(part) = ir_part(part, "assistant") {
            keys.push(display_key(&part));
            said.push(part);
        }
    }
    Response { parts: said, keys }
}

/// Rebuilds a streamed reply from the captured SSE blocks, one display part per
/// content block, keeping text, thinking, and tool calls apart.
fn fold_stream(format: Format, lines: &[String]) -> Response {
    let mut parts: Vec<Part> = Vec::new();
    let mut text_at: Option<usize> = None;
    let mut thinking_at: Option<usize> = None;
    let mut tools: Vec<(u32, usize)> = Vec::new();
    let mut state = StreamState::default();

    for block in split_sse_blocks(&lines.join("\n\n")) {
        let Some(event) = parse_sse_block(&block) else {
            continue;
        };
        let Ok(events) = stream_events(format, &event, &mut state) else {
            continue;
        };
        for event in events {
            match event {
                CanonicalStreamEvent::TextDelta(delta) => {
                    let at = *text_at.get_or_insert_with(|| {
                        parts.push(text_part("assistant"));
                        parts.len() - 1
                    });
                    parts[at].text.push_str(&delta);
                }
                CanonicalStreamEvent::ThinkingDelta(delta) => {
                    let at = *thinking_at.get_or_insert_with(|| {
                        parts.push(Part {
                            role: "assistant".to_string(),
                            kind: "thinking".to_string(),
                            name: None,
                            id: None,
                            media_type: None,
                            is_error: false,
                            text: String::new(),
                            cut: 0,
                        });
                        parts.len() - 1
                    });
                    parts[at].text.push_str(&delta);
                }
                CanonicalStreamEvent::ToolCallStart {
                    index, id, name, ..
                } => {
                    text_at = None;
                    thinking_at = None;
                    parts.push(Part {
                        role: "assistant".to_string(),
                        kind: "tool_use".to_string(),
                        name: Some(name),
                        id: Some(id),
                        media_type: None,
                        is_error: false,
                        text: String::new(),
                        cut: 0,
                    });
                    tools.push((index, parts.len() - 1));
                }
                CanonicalStreamEvent::ToolCallDelta {
                    index,
                    arguments_delta,
                } => {
                    if let Some((_, at)) = tools.iter().find(|(i, _)| *i == index) {
                        parts[*at].text.push_str(&arguments_delta);
                    }
                }
                CanonicalStreamEvent::SealedThinking { by, .. } => {
                    text_at = None;
                    thinking_at = None;
                    parts.push(Part {
                        role: "assistant".to_string(),
                        kind: "thinking".to_string(),
                        name: Some(by.to_string()),
                        id: None,
                        media_type: None,
                        is_error: false,
                        text: "[sealed reasoning]".to_string(),
                        cut: 0,
                    });
                }
                CanonicalStreamEvent::Start { .. }
                | CanonicalStreamEvent::Done { .. }
                | CanonicalStreamEvent::ThinkingSignature { .. } => {}
            }
        }
    }

    parts.retain(|part| !part.text.is_empty());
    Response {
        keys: parts.iter().map(display_key).collect(),
        parts,
    }
}

fn stream_events(
    format: Format,
    event: &any_converter_core::sse::SseEvent,
    state: &mut StreamState,
) -> Result<Vec<CanonicalStreamEvent>, any_converter_core::error::ConvertError> {
    match format {
        Format::Claude => claude::ClaudeStreamAdapter::parse_sse_event(event, state),
        Format::OpenAIChat => openai_chat::OpenAIChatStreamAdapter::parse_sse_event(event, state),
        Format::OpenAIResponses => {
            openai_resp::OpenAIResponsesStreamAdapter::parse_sse_event(event, state)
        }
        Format::Gemini => gemini::GeminiStreamAdapter::parse_sse_event(event, state),
    }
}

/// The trace's preview of the reply, used to keep replay comparison in the same
/// vocabulary as a fallback request.
fn trace_response(record: &RequestLogRecord) -> Response {
    let Some(trace) = record.trace.as_ref() else {
        return Response {
            parts: Vec::new(),
            keys: Vec::new(),
        };
    };
    let mut said = Vec::new();
    let mut keys = Vec::new();
    for call in &trace.response.tool_calls {
        let part = Part {
            role: "assistant".to_string(),
            kind: "tool_use".to_string(),
            name: Some(tool_name(call.namespace.as_deref(), &call.name)),
            id: call.id.clone(),
            media_type: None,
            is_error: false,
            text: call.arguments_preview.clone(),
            cut: 0,
        };
        keys.push(display_key(&part));
        said.push(part);
    }
    for message in &trace.response.messages {
        if message.content_preview.trim().is_empty() {
            continue;
        }
        let part = Part {
            role: "assistant".to_string(),
            kind: "text".to_string(),
            name: None,
            id: None,
            media_type: None,
            is_error: false,
            text: message.content_preview.clone(),
            cut: 0,
        };
        keys.push(display_key(&part));
        said.push(part);
    }
    Response { parts: said, keys }
}

fn trace_response_keys(record: &RequestLogRecord) -> Vec<PartKey> {
    trace_response(record)
        .keys
        .into_iter()
        .filter(|key| !matches!(key, PartKey::Text(text) if text.is_empty()))
        .collect()
}

fn raw_response(text: &str) -> Response {
    let text = text.trim();
    if text.is_empty() {
        return Response {
            parts: Vec::new(),
            keys: Vec::new(),
        };
    }
    let part = Part {
        role: "assistant".to_string(),
        kind: "text".to_string(),
        name: None,
        id: None,
        media_type: None,
        is_error: false,
        text: text.to_string(),
        cut: 0,
    };
    Response {
        keys: vec![display_key(&part)],
        parts: vec![part],
    }
}

fn ir_part(part: &IrPart, role: &str) -> Option<Part> {
    let part = match part {
        IrPart::Text { text, .. } if !text.trim().is_empty() => Part {
            role: role.to_string(),
            kind: "text".to_string(),
            name: None,
            id: None,
            media_type: None,
            is_error: false,
            text: text.clone(),
            cut: 0,
        },
        IrPart::Thinking { text, .. } if !text.trim().is_empty() => Part {
            role: role.to_string(),
            kind: "thinking".to_string(),
            name: None,
            id: None,
            media_type: None,
            is_error: false,
            text: text.clone(),
            cut: 0,
        },
        IrPart::Sealed { by, .. } => Part {
            role: role.to_string(),
            kind: "thinking".to_string(),
            name: Some(by.to_string()),
            id: None,
            media_type: None,
            is_error: false,
            text: "[sealed reasoning]".to_string(),
            cut: 0,
        },
        IrPart::ToolCall { id, name, args, .. } => Part {
            role: "assistant".to_string(),
            kind: "tool_use".to_string(),
            name: Some(name.clone()),
            id: Some(id.clone()),
            media_type: None,
            is_error: false,
            text: pretty_args(args),
            cut: 0,
        },
        IrPart::ToolResult {
            call_id,
            is_error,
            content,
        } => {
            let text = content_text(content);
            if text.trim().is_empty() {
                return None;
            }
            Part {
                role: "tool".to_string(),
                kind: "tool_result".to_string(),
                name: None,
                id: Some(call_id.clone()),
                media_type: None,
                is_error: *is_error,
                text,
                cut: 0,
            }
        }
        IrPart::Image {
            url,
            media_type,
            data,
            ..
        } => Part {
            role: role.to_string(),
            kind: "image".to_string(),
            name: Some(media_type.clone()),
            id: None,
            media_type: Some(media_type.clone()),
            is_error: false,
            text: image_text(url, data.as_deref()),
            cut: 0,
        },
        _ => return None,
    };
    Some(part)
}

fn text_part(role: &str) -> Part {
    Part {
        role: role.to_string(),
        kind: "text".to_string(),
        name: None,
        id: None,
        media_type: None,
        is_error: false,
        text: String::new(),
        cut: 0,
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn content_text(parts: &[IrPart]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            IrPart::Text { text, .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            IrPart::Image {
                url,
                media_type,
                data,
                ..
            } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!(
                    "[image {media_type}] {}",
                    image_text(url, data.as_deref())
                ));
            }
            _ => {}
        }
    }
    out
}

fn image_text(url: &str, data: Option<&str>) -> String {
    match data {
        Some(data) => format!("[inline image, {} KiB]", base64_bytes(data) / 1024),
        None => url.to_string(),
    }
}

fn image_key(url: &str, media_type: &str, data: Option<&str>) -> String {
    match data {
        Some(data) => format!("{media_type}:{}", data.len()),
        None => format!("{media_type}:{url}"),
    }
}

fn base64_bytes(data: &str) -> usize {
    data.len() / 4 * 3
}

fn pretty_args(args: &Value) -> String {
    match args {
        Value::Null => "{}".to_string(),
        value => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn parse_args(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn tool_name(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}.{name}"),
        _ => name.to_string(),
    }
}

fn bound(mut part: Part, cut: &mut bool) -> Part {
    let mut end = PART_CHARS.min(part.text.len());
    while end > 0 && !part.text.is_char_boundary(end) {
        end -= 1;
    }
    if end < part.text.len() {
        part.cut = part.text[end..].chars().count();
        part.text.truncate(end);
        *cut = true;
    }
    part
}

/// Keeps the first `max_chars` characters, collapsing runs of whitespace to one
/// space, and marks the cut with an ellipsis.
fn compact(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            if count + 1 >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count + 1 >= max_chars {
            break;
        }
        out.push(ch);
        count += 1;
    }
    if text.chars().count() > out.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::{RequestTraceSummary, TraceBodySummary, TraceMessage};
    use any_converter_core::ir::Usage;

    fn record(session: &str, user: &str, reply: &str) -> RequestLogRecord {
        let mut record = RequestLogRecord {
            request_id: format!("req-{session}-{user}"),
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            client_format: "claude".to_string(),
            client_id: Some("claude".to_string()),
            session_id: Some(session.to_string()),
            provider_account: None,
            served_model: None,
            ttft_ms: 0,
            cache_unknown: false,
            cost_usd: None,
            price_version: None,
            decode_ms: 0,
            decode_out: 0,
            own_ms: 0,
            sent_ms: 0,
            retried_ms: 0,
            queued_ms: 0,
            masking_ms: 0,
            flow_ms: 0,
            attempt: 0,
            handed_on: false,
            error: None,
            err_type: None,
            upstream: None,
            effort: None,
            tier_asked: None,
            tier_sent: None,
            tier_served: None,
            subagent: None,
            parent_agent: None,
            kind: None,
            provider: "anthropic".to_string(),
            client_model: "claude".to_string(),
            upstream_model: "claude".to_string(),
            streaming: false,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            request_body: None,
            upstream_request_body: None,
            response_status: 200,
            response_body: ResponseBodyKind::Json {
                text: reply.to_string(),
            },
            latency_ms: 10,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cache_write_1h: None,
                reasoning_tokens: None,
            },
            trace: Some(RequestTraceSummary {
                client: TraceBodySummary {
                    messages: vec![
                        TraceMessage {
                            role: "system".to_string(),
                            content_preview: "be brief".to_string(),
                        },
                        TraceMessage {
                            role: "user".to_string(),
                            content_preview: user.to_string(),
                        },
                    ],
                    tool_definitions: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                },
                upstream: TraceBodySummary::default(),
                response: TraceBodySummary::default(),
            }),
            truncated: false,
        };
        record.response_body = ResponseBodyKind::Json {
            text: format!(
                r#"{{"id":"msg","type":"message","role":"assistant","model":"claude","content":[{{"type":"text","text":{}}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":1,"output_tokens":1}}}}"#,
                serde_json::to_string(reply).unwrap()
            ),
        };
        record
    }

    fn texts(transcript: &Transcript) -> Vec<&str> {
        transcript
            .turns
            .iter()
            .flat_map(|turn| turn.parts.iter())
            .map(|part| part.text.as_str())
            .collect()
    }

    #[test]
    fn a_replayed_turn_says_only_what_is_new() {
        let mut second = record("s", "hello", "");
        let trace = second.trace.as_mut().unwrap();
        // The client replays the first turn, reply included, then says more.
        trace.client.messages.insert(
            2,
            TraceMessage {
                role: "assistant".to_string(),
                content_preview: "hi".to_string(),
            },
        );
        trace.client.messages.push(TraceMessage {
            role: "user".to_string(),
            content_preview: "again".to_string(),
        });

        let transcript = fold_transcript(&[record("s", "hello", "hi"), second]);

        assert_eq!(texts(&transcript), vec!["be brief", "hello", "hi", "again"]);
        assert!(!transcript.cut);
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(
            session_title(&record("s", "hello", "hi")).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn equal_text_later_in_the_chat_is_kept() {
        let mut second = record("s", "hello", "");
        let trace = second.trace.as_mut().unwrap();
        trace.client.messages.insert(
            2,
            TraceMessage {
                role: "assistant".to_string(),
                content_preview: "hi".to_string(),
            },
        );
        // "hello" comes back after the reply, so it is a new turn, not a replay.
        trace.client.messages.push(TraceMessage {
            role: "user".to_string(),
            content_preview: "hello".to_string(),
        });

        let transcript = fold_transcript(&[record("s", "hello", "hi"), second]);
        let repeated = texts(&transcript)
            .iter()
            .filter(|text| **text == "hello")
            .count();
        assert_eq!(repeated, 2);
    }

    #[test]
    fn a_captured_body_beats_the_trace_preview() {
        let mut record = record("s", "short", "ok");
        record.request_body = Some(crate::request_log::redactor::SanitizedBody {
            text: serde_json::json!({
                "model": "claude",
                "max_tokens": 100,
                "system": "you are a tool",
                "messages": [{"role": "user", "content": "the whole question"}]
            })
            .to_string(),
            truncated: false,
        });

        let transcript = fold_transcript(&[record]);
        assert_eq!(
            texts(&transcript),
            vec!["you are a tool", "the whole question", "ok"]
        );
    }

    #[test]
    fn a_tool_call_and_its_error_result_are_parts() {
        let mut record = record("s", "", "");
        record.request_body = Some(crate::request_log::redactor::SanitizedBody {
            text: serde_json::json!({
                "model": "claude",
                "max_tokens": 100,
                "messages": [
                    {"role": "user", "content": "run it"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "t1", "name": "bash", "input": {"cmd": "ls"}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "no such file"}
                    ]}
                ]
            })
            .to_string(),
            truncated: false,
        });
        record.response_body = ResponseBodyKind::Json {
            text: serde_json::json!({
                "id": "msg", "type": "message", "role": "assistant", "model": "claude",
                "content": [{"type": "text", "text": "fixed"}],
                "stop_reason": "end_turn", "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })
            .to_string(),
        };

        let transcript = fold_transcript(&[record]);
        let parts: Vec<_> = transcript.turns[0].parts.iter().collect();
        let call = parts.iter().find(|part| part.kind == "tool_use").unwrap();
        assert_eq!(call.name.as_deref(), Some("bash"));
        assert_eq!(call.id.as_deref(), Some("t1"));
        assert!(call.text.contains("\"cmd\""));
        let result = parts
            .iter()
            .find(|part| part.kind == "tool_result")
            .unwrap();
        assert_eq!(result.role, "tool");
        assert!(result.is_error);
        assert_eq!(result.id.as_deref(), Some("t1"));
        assert_eq!(result.text, "no such file");
    }

    #[test]
    fn a_streamed_reply_keeps_thinking_and_tool_calls() {
        let mut record = record("s", "", "");
        record.streaming = true;
        record.response_body = ResponseBodyKind::SseLines {
            lines: vec![
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me look\"}}".to_string(),
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"looking\"}}".to_string(),
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"read\",\"input\":{}}}".to_string(),
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a\\\"}\"}}".to_string(),
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}".to_string(),
            ],
        };

        let transcript = fold_transcript(&[record]);
        let parts = &transcript.turns[0].parts;
        assert!(
            parts
                .iter()
                .any(|part| part.kind == "thinking" && part.text == "let me look")
        );
        assert!(
            parts
                .iter()
                .any(|part| part.kind == "text" && part.text == "looking")
        );
        let call = parts.iter().find(|part| part.kind == "tool_use").unwrap();
        assert_eq!(call.name.as_deref(), Some("read"));
        assert_eq!(call.text, "{\"path\":\"a\"}");
    }
}
