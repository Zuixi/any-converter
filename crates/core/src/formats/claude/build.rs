//! IR → Claude Messages.
//!
//! Port of `converters/{chat,resp,gemini}_to_claude.rs`, plus Claude's own
//! dialect constraints: `max_tokens` is mandatory, `thinking` must be enabled
//! again when the history already carries reasoning, a lone text block
//! collapses to a bare string, only Claude-signed thinking can be replayed.

use serde_json::Value;

use crate::convert::Format;
use crate::converters::reasoning;
use crate::converters::shared::{CLAUDE_TEMPERATURE_MAX, normalize_id_to_claude};
use crate::ir::{IrMessage, IrPart, IrRequest, IrResponse, IrToolChoice, Reasoning, Role};

use super::{
    ClaudeContentBlock, ClaudeMessage, ClaudeRequest, ClaudeResponse, ClaudeTool, ClaudeUsage,
    DEFAULT_MAX_TOKENS, ThinkingConfig,
};

pub fn build(ir: IrRequest) -> ClaudeRequest {
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

    let max_tokens = sampling.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let thinking = build_thinking(reasoning, max_tokens, &messages);

    let mut fields = extra.take(Format::Claude);
    let metadata = fields
        .iter()
        .position(|(name, _)| name == "metadata")
        .map(|i| fields.swap_remove(i).1);

    ClaudeRequest {
        model,
        max_tokens,
        messages: messages.into_iter().map(build_message).collect(),
        system: build_system(system),
        temperature: sampling
            .temperature
            .map(|t| t.clamp(0.0, CLAUDE_TEMPERATURE_MAX)),
        top_p: sampling.top_p,
        top_k: sampling.top_k,
        stop_sequences: (!sampling.stop.is_empty()).then_some(sampling.stop),
        stream: stream.then_some(true),
        tools: (!tools.is_empty()).then(|| {
            tools
                .into_iter()
                .map(|tool| ClaudeTool {
                    name: tool.name,
                    description: (!tool.description.is_empty()).then_some(tool.description),
                    input_schema: tool.schema,
                })
                .collect()
        }),
        tool_choice: build_tool_choice(tool_choice),
        metadata,
        thinking,
    }
}

pub fn build_response(ir: IrResponse) -> ClaudeResponse {
    let IrResponse {
        id,
        model,
        parts,
        stop_reason,
        stop_sequence,
        usage,
    } = ir;

    ClaudeResponse {
        id: normalize_id_to_claude(id.as_deref().unwrap_or_default()),
        r#type: "message".into(),
        role: "assistant".into(),
        model,
        content: parts.into_iter().filter_map(build_response_block).collect(),
        stop_reason: Some(stop_reason.to_claude().to_string()),
        stop_sequence,
        usage: ClaudeUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_write_tokens,
            cache_read_input_tokens: usage.cache_read_tokens,
        },
    }
}

/// Ported from `chat_to_claude`: an explicit budget is written back as-is; an
/// effort goes through the shared mapping; history that already carries
/// reasoning keeps thinking enabled (some Anthropic endpoints reason by
/// default, so not saying anything burns tokens).
fn build_thinking(
    reasoning: Reasoning,
    max_tokens: u32,
    messages: &[IrMessage],
) -> Option<ThinkingConfig> {
    let thinking = match reasoning {
        Reasoning::Off => None,
        Reasoning::Budget(budget_tokens) => Some(ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens,
        }),
        Reasoning::Effort(effort) => {
            reasoning::reasoning_effort_to_thinking(Some(&effort), max_tokens)
        }
    };

    thinking.or_else(|| {
        let has_thinking = messages
            .iter()
            .any(|m| m.parts.iter().any(|p| matches!(p, IrPart::Thinking { .. })));
        has_thinking.then(|| ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: max_tokens.max(1024),
        })
    })
}

fn build_message(msg: IrMessage) -> ClaudeMessage {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    // A user message with no blocks was a bare empty string; an assistant one
    // stayed an empty block array (what the pairwise builders emitted).
    let empty = match msg.role {
        Role::User => Value::String(String::new()),
        Role::Assistant => Value::Array(Vec::new()),
    };
    ClaudeMessage {
        role: role.into(),
        content: text_or_blocks(msg.parts, empty),
    }
}

/// Claude collapses a lone text block to a bare string, as every pairwise
/// builder did; `empty` is what no blocks at all become.
fn text_or_blocks(mut parts: Vec<IrPart>, empty: Value) -> Value {
    if parts.len() == 1 {
        if let IrPart::Text(_) = parts[0] {
            if let IrPart::Text(text) = parts.remove(0) {
                return Value::String(text);
            }
        }
    }
    if parts.is_empty() {
        return empty;
    }
    Value::Array(parts.into_iter().filter_map(build_part).collect())
}

fn build_part(part: IrPart) -> Option<Value> {
    Some(match part {
        IrPart::Text(text) => text_block(text),
        IrPart::Image {
            url,
            media_type,
            data,
            ..
        } => {
            let source = match data {
                Some(data) => serde_json::json!({
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                }),
                None => serde_json::json!({ "type": "url", "url": url }),
            };
            serde_json::json!({ "type": "image", "source": source })
        }
        IrPart::ToolCall { id, name, args } => serde_json::json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": args,
        }),
        IrPart::ToolResult {
            call_id,
            is_error,
            content,
        } => serde_json::json!({
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": text_or_blocks(content, Value::String(String::new())),
            "is_error": is_error,
        }),
        IrPart::Thinking { text, signature } => {
            let mut block = serde_json::json!({ "type": "thinking", "thinking": text });
            // Only Claude may replay its own signature; another vendor's is
            // meaningless to Anthropic.
            if let Some(sig) = signature.filter(|s| s.by == Format::Claude) {
                block["signature"] = Value::String(sig.value);
            }
            block
        }
        // Sealed reasoning is replayed verbatim by its sealing API only.
        IrPart::Sealed {
            by: Format::Claude,
            raw,
        } => raw,
        IrPart::Sealed { .. } => return None,
    })
}

fn build_system(parts: Vec<IrPart>) -> Option<Value> {
    let mut texts: Vec<String> = Vec::new();
    for part in parts {
        if let IrPart::Text(text) = part {
            texts.push(text);
        }
    }
    match texts.len() {
        0 => None,
        1 => texts.pop().map(Value::String),
        _ => Some(Value::Array(texts.into_iter().map(text_block).collect())),
    }
}

fn build_tool_choice(choice: Option<IrToolChoice>) -> Option<Value> {
    match choice? {
        // An explicit `auto` stays explicit; an absent `tool_choice` stays
        // absent.
        IrToolChoice::Auto => Some(serde_json::json!({ "type": "auto" })),
        IrToolChoice::None => Some(serde_json::json!({ "type": "none" })),
        IrToolChoice::Required => Some(serde_json::json!({ "type": "any" })),
        IrToolChoice::Named(name) => Some(serde_json::json!({ "type": "tool", "name": name })),
    }
}

fn build_response_block(part: IrPart) -> Option<ClaudeContentBlock> {
    Some(match part {
        // Every pairwise response builder dropped empty text/thinking.
        IrPart::Text(text) if !text.is_empty() => ClaudeContentBlock::Text { text },
        IrPart::Thinking { text, signature } if !text.is_empty() => ClaudeContentBlock::Thinking {
            thinking: text,
            signature: signature
                .filter(|s| s.by == Format::Claude)
                .map(|s| s.value),
        },
        IrPart::ToolCall { id, name, args } => ClaudeContentBlock::ToolUse {
            id,
            name,
            input: args,
        },
        // dropped: response blocks for images, tool results and sealed
        // reasoning have no Claude content-block variant.
        _ => return None,
    })
}

fn text_block(text: String) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}
