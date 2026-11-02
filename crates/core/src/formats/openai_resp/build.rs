//! IR → OpenAI Responses wire types.
//!
//! Port of the Responses half of `converters/{chat,claude,gemini}_to_resp.rs`.
//! Dialect constraints live here: a single `instructions` string, `input`
//! items grouped per message, `arguments` serialized as a JSON string, and
//! reasoning items carrying their `summary` text.

use serde_json::{Map, Value};

use crate::convert::Format;
use crate::converters::reasoning::thinking_to_reasoning_json;
use crate::converters::shared::{
    ITEM_TYPE_FUNCTION_CALL, ITEM_TYPE_FUNCTION_CALL_OUTPUT, ITEM_TYPE_INPUT_IMAGE,
    ITEM_TYPE_INPUT_TEXT, ITEM_TYPE_MESSAGE, ITEM_TYPE_OUTPUT_TEXT, new_uuid, normalize_id_to_resp,
    now_unix_secs,
};
use crate::formats::claude::ThinkingConfig;
use crate::formats::openai_resp::types::{
    InputTokensDetails, OpenAIResponsesRequest, OpenAIResponsesResponse, ResponsesUsage,
};
use crate::ir::{
    IrMessage, IrPart, IrRequest, IrResponse, IrTool, IrToolChoice, Reasoning, Role, Signature,
    StopReason,
};

const ITEM_TYPE_REASONING: &str = "reasoning";
const ITEM_TYPE_SUMMARY_TEXT: &str = "summary_text";

pub fn build(ir: IrRequest) -> OpenAIResponsesRequest {
    let IrRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        sampling,
        stream,
        reasoning,
        mut extra,
    } = ir;

    let mut text = None;
    let mut previous_response_id = None;
    let mut store = None;
    let mut reasoning_extra = None;
    let mut rest = Map::new();
    for (name, value) in extra.take(Format::OpenAIResponses) {
        match name.as_str() {
            "text" => text = Some(value),
            "previous_response_id" => {
                previous_response_id = match value {
                    Value::String(value) => Some(value),
                    _ => None,
                }
            }
            "store" => {
                store = match value {
                    Value::Bool(value) => Some(value),
                    _ => None,
                }
            }
            // Superseded by the typed `reasoning` below, which is regenerated
            // from IR so another format's reader can set it.
            "reasoning" => reasoning_extra = Some(value),
            _ => {
                rest.insert(name, value);
            }
        }
    }

    OpenAIResponsesRequest {
        model,
        input: messages_to_input(messages),
        instructions: system_to_instructions(system),
        max_output_tokens: sampling.max_tokens,
        temperature: sampling.temperature,
        top_p: sampling.top_p,
        stream: if stream { Some(true) } else { None },
        tools: if tools.is_empty() {
            None
        } else {
            Some(tools.into_iter().map(tool_to_value).collect())
        },
        tool_choice: tool_choice.map(tool_choice_to_value),
        text,
        reasoning: reasoning_extra.or_else(|| reasoning_to_value(&reasoning)),
        previous_response_id,
        store,
        extra: rest,
    }
}

pub fn build_response(ir: IrResponse) -> OpenAIResponsesResponse {
    let IrResponse {
        id,
        model,
        parts,
        stop_reason,
        usage,
        ..
    } = ir;

    let mut output = Vec::new();
    let mut text_content = Vec::new();
    for part in parts {
        match part {
            IrPart::Text { text, .. } => {
                text_content.push(serde_json::json!({
                    "type": ITEM_TYPE_OUTPUT_TEXT,
                    "text": text,
                }));
            }
            IrPart::ToolCall { id, name, args, .. } => {
                flush_text(&mut output, &mut text_content);
                output.push(serde_json::json!({
                    "type": ITEM_TYPE_FUNCTION_CALL,
                    "call_id": id,
                    "name": name,
                    "arguments": arguments_to_string(&args),
                }));
            }
            IrPart::Thinking { text, signature } => {
                flush_text(&mut output, &mut text_content);
                // Only Claude's own signature may be sealed: another vendor's
                // is meaningless to Anthropic and no Responses client reads it.
                let claude = signature.filter(|s| s.by == Format::Claude);
                output.push(reasoning_item(text, claude));
            }
            // Claude's redacted_thinking goes back whole, in encrypted_content,
            // to come back on the next turn of a tool loop (claudeseal).
            IrPart::Sealed {
                by: Format::Claude,
                raw,
            } => {
                flush_text(&mut output, &mut text_content);
                output.push(sealed_reasoning_item(&raw));
            }
            // Responses output carries text, tool calls and reasoning only.
            _ => {}
        }
    }
    flush_text(&mut output, &mut text_content);

    OpenAIResponsesResponse {
        id: normalize_id_to_resp(id.as_deref().unwrap_or("")),
        object: "response".into(),
        created_at: now_unix_secs(),
        model,
        status: if stop_reason == StopReason::MaxTokens {
            "incomplete"
        } else {
            "completed"
        }
        .into(),
        output,
        usage: Some(ResponsesUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: Some(usage.input_tokens + usage.output_tokens),
            input_tokens_details: usage.cache_read_tokens.map(|cached| InputTokensDetails {
                cached_tokens: Some(cached),
            }),
        }),
    }
}

// ── request pieces ─────────────────────────────────────────────────────

fn system_to_instructions(system: Vec<IrPart>) -> Option<String> {
    let texts: Vec<String> = system
        .into_iter()
        .filter_map(|part| match part {
            IrPart::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

fn messages_to_input(messages: Vec<IrMessage>) -> Option<Value> {
    let mut items = Vec::new();
    for message in messages {
        let role = message.role;
        let mut content = Vec::new();
        for part in message.parts {
            match part {
                IrPart::Text { text, .. } => content.push(serde_json::json!({
                    "type": text_part_type(role),
                    "text": text,
                })),
                IrPart::Image { url, detail, .. } => {
                    let mut part = serde_json::json!({
                        "type": ITEM_TYPE_INPUT_IMAGE,
                        "image_url": url,
                    });
                    // chat_to_resp drops `detail`; keeping it preserves the
                    // roundtrip through the hub.
                    if let Some(detail) = detail {
                        part["detail"] = Value::String(detail);
                    }
                    content.push(part);
                }
                IrPart::ToolCall { id, name, args, .. } => {
                    flush_input(&mut items, role, &mut content);
                    items.push(serde_json::json!({
                        "type": ITEM_TYPE_FUNCTION_CALL,
                        "call_id": id,
                        "name": name,
                        "arguments": arguments_to_string(&args),
                    }));
                }
                IrPart::ToolResult {
                    call_id,
                    content: result,
                    ..
                } => {
                    flush_input(&mut items, role, &mut content);
                    items.push(serde_json::json!({
                        "type": ITEM_TYPE_FUNCTION_CALL_OUTPUT,
                        "call_id": call_id,
                        "output": parts_to_text(result),
                    }));
                }
                // Reasoning history is not sent upstream as a synthesized
                // reasoning item; the pairwise builders dropped it too.
                IrPart::Thinking { .. } => {}
                // Sealed reasoning belongs to whichever format sealed it.
                IrPart::Sealed { .. } => {}
            }
        }
        flush_input(&mut items, role, &mut content);
    }

    if items.is_empty() {
        None
    } else {
        Some(Value::Array(items))
    }
}

fn text_part_type(role: Role) -> &'static str {
    match role {
        Role::User => ITEM_TYPE_INPUT_TEXT,
        Role::Assistant => ITEM_TYPE_OUTPUT_TEXT,
    }
}

fn flush_input(items: &mut Vec<Value>, role: Role, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    let role = match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    items.push(serde_json::json!({
        "type": ITEM_TYPE_MESSAGE,
        "role": role,
        "content": std::mem::take(content),
    }));
}

/// Tool results are a single string on the wire; text parts are concatenated
/// the way `claude_to_resp`/`chat_to_resp` do.
fn parts_to_text(parts: Vec<IrPart>) -> String {
    parts
        .into_iter()
        .filter_map(|part| match part {
            IrPart::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn tool_to_value(tool: IrTool) -> Value {
    let mut value = serde_json::json!({
        "type": "function",
        "name": tool.name,
        "parameters": tool.schema,
    });
    // absent stays absent; the wire default is the upstream's call
    // strict defaults to true on Responses: the old pairwise wrote it
    // explicitly, so absent-in must come out as true, not omitted
    value["strict"] = Value::Bool(tool.strict.unwrap_or(true));
    if !tool.description.is_empty() {
        value["description"] = Value::String(tool.description);
    }
    value
}

fn tool_choice_to_value(tool_choice: IrToolChoice) -> Value {
    match tool_choice {
        IrToolChoice::Auto => serde_json::json!("auto"),
        IrToolChoice::None => serde_json::json!("none"),
        IrToolChoice::Required => serde_json::json!("required"),
        IrToolChoice::Named(name) => serde_json::json!({"type": "function", "name": name}),
    }
}

fn reasoning_to_value(reasoning: &Reasoning) -> Option<Value> {
    match reasoning {
        Reasoning::Off => None,
        Reasoning::Effort(effort) => Some(serde_json::json!({ "effort": effort })),
        Reasoning::Budget(budget) => thinking_to_reasoning_json(&ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: *budget,
        }),
    }
}

fn arguments_to_string(args: &Value) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "{}".into())
}

fn reasoning_item(text: String, claude_signature: Option<Signature>) -> Value {
    let mut item = serde_json::json!({
        "type": ITEM_TYPE_REASONING,
        "id": format!("rs_{}", new_uuid()),
        "summary": [{"type": ITEM_TYPE_SUMMARY_TEXT, "text": text}],
    });
    if let Some(signature) = claude_signature {
        item["encrypted_content"] = Value::String(seal_thinking(&text, &signature.value));
    }
    item
}

/// A block an upstream sealed whole: no summary, the block itself in
/// `encrypted_content` (magpie `renderResponses`' `sealedBy(sealAnthropic)`).
fn sealed_reasoning_item(raw: &Value) -> Value {
    serde_json::json!({
        "type": ITEM_TYPE_REASONING,
        "id": format!("rs_{}", new_uuid()),
        "summary": [],
        "encrypted_content": seal_block(raw),
    })
}

// ── claudeseal ─────────────────────────────────────────────────────────
//
// Claude signs each thinking block and seals a redacted_thinking block whole,
// and Anthropic wants both back as they came. A Responses client with store
// off keeps only what a reply's reasoning items carry, and of those only
// `encrypted_content` is opaque, so the block rides there (magpie
// internal/claudeseal, internal/gateway/claude_seal.go).
//
// Deviation from magpie: magpie seals only when the request asked for
// `include: ["reasoning.encrypted_content"]` (`asksSealing`);
// `convert_response` never sees the request here, so we always seal.

/// Begins the `encrypted_content` of a reasoning item carrying Claude's block.
/// Same mark as magpie, so a session survives a gateway switch.
const CLAUDE_SEAL_MARK: &str = "magpie-claude1:";

/// `encrypted_content` for Claude's thinking block with its signature. The
/// field order is Anthropic's, as magpie marshals it (`claudeThinking`).
pub(super) fn seal_thinking(text: &str, signature: &str) -> String {
    let block = format!(
        r#"{{"type":"thinking","thinking":{},"signature":{}}}"#,
        Value::String(text.into()),
        Value::String(signature.into())
    );
    seal(block.as_bytes())
}

/// `encrypted_content` for a block sealed whole, as the upstream wrote it.
pub(super) fn seal_block(raw: &Value) -> String {
    seal(raw.to_string().as_bytes())
}

fn seal(block: &[u8]) -> String {
    format!("{CLAUDE_SEAL_MARK}{}", base64_encode(block))
}

/// The block an `encrypted_content` carries, for one magpie sealed: `None`
/// when it is not ours or can't be read (magpie `claudeseal.Open`).
pub(super) fn open_seal(encrypted: &str) -> Option<Value> {
    let b64 = encrypted.strip_prefix(CLAUDE_SEAL_MARK)?;
    serde_json::from_slice(&base64_decode(b64)?).ok()
}

/// Whether `encrypted_content` is one magpie sealed, readable or not
/// (magpie `claudeseal.Is`).
pub(super) fn is_sealed(encrypted: &str) -> bool {
    encrypted.starts_with(CLAUDE_SEAL_MARK)
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn flush_text(output: &mut Vec<Value>, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    output.push(serde_json::json!({
        "type": ITEM_TYPE_MESSAGE,
        "role": "assistant",
        "content": std::mem::take(content),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded).as_deref(), Some(plain.as_bytes()));
        }
    }

    #[test]
    fn seal_thinking_writes_the_block_in_anthropic_field_order() {
        let seal = seal_thinking("t", "s");
        let b64 = seal.strip_prefix(CLAUDE_SEAL_MARK).unwrap_or_default();
        let block = base64_decode(b64).unwrap_or_default();
        assert_eq!(
            String::from_utf8(block).unwrap_or_default(),
            r#"{"type":"thinking","thinking":"t","signature":"s"}"#
        );
    }
}
