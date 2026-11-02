//! Claude Messages → IR.
//!
//! Port of `converters/claude_to_{chat,resp,gemini}.rs`: everything those three
//! moved is moved here. Their disagreements (system separators, id shapes,
//! strict flags) are left to the target format's builder.
//!
//! `read` cannot fail (the pairwise converters returned `ConvertError`); what
//! they rejected has no IR home and is dropped instead — see the `// dropped`
//! notes.

use serde_json::{Map, Value};

use crate::convert::Format;
use crate::converters::shared::image_block_to_url;
use crate::ir::{
    IrMessage, IrPart, IrRequest, IrResponse, IrTool, IrToolChoice, NamespacedExtra, Reasoning,
    Role, Sampling, Signature, StopReason, Usage,
};

use super::{ClaudeContentBlock, ClaudeMessage, ClaudeRequest, ClaudeResponse, ClaudeTool};

pub fn read(req: ClaudeRequest) -> IrRequest {
    let ClaudeRequest {
        model,
        max_tokens,
        messages,
        system,
        temperature,
        top_p,
        top_k,
        stop_sequences,
        stream,
        tools,
        tool_choice,
        metadata,
        thinking,
    } = req;

    let mut extra = NamespacedExtra::default();
    if let Some(metadata) = metadata {
        extra.put(Format::Claude, "metadata", metadata);
    }

    IrRequest {
        model,
        system: read_system(system),
        messages: messages.into_iter().filter_map(read_message).collect(),
        tools: read_tools(tools),
        tool_choice: read_tool_choice(tool_choice.as_ref()),
        sampling: Sampling {
            max_tokens: Some(max_tokens),
            temperature,
            top_p,
            top_k,
            stop: stop_sequences.unwrap_or_default(),
        },
        stream: stream.unwrap_or(false),
        reasoning: match thinking {
            Some(cfg) if cfg.r#type == "enabled" => Reasoning::Budget(cfg.budget_tokens),
            _ => Reasoning::Off,
        },
        extra,
    }
}

pub fn read_response(resp: ClaudeResponse) -> IrResponse {
    let ClaudeResponse {
        id,
        model,
        content,
        stop_reason,
        stop_sequence,
        usage,
        ..
    } = resp;

    let mut parts = Vec::with_capacity(content.len());
    for block in content {
        parts.push(match block {
            ClaudeContentBlock::Text { text } => IrPart::Text {
                text: text,
                signature: None,
            },
            ClaudeContentBlock::ToolUse { id, name, input } => IrPart::ToolCall {
                id,
                name,
                args: input,
                signature: None,
            },
            ClaudeContentBlock::Thinking {
                thinking,
                signature,
            } => IrPart::Thinking {
                text: thinking,
                signature: signature.map(|value| Signature {
                    by: Format::Claude,
                    value,
                }),
            },
        });
    }

    IrResponse {
        id: Some(id),
        model,
        parts,
        stop_reason: StopReason::from_claude(stop_reason.as_deref().unwrap_or_default()),
        stop_sequence,
        usage: Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: None,
        },
    }
}

/// `system` is a bare string or an array of text blocks; both become parts so
/// each target keeps its own separator (`"\n\n"`, `""`, ...).
fn read_system(system: Option<Value>) -> Vec<IrPart> {
    match system {
        Some(Value::String(text)) => vec![IrPart::Text {
            text: text,
            signature: None,
        }],
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                // An empty `text` still counts: it keeps its separator slot.
                Value::Object(mut obj) => match obj.remove("text") {
                    Some(Value::String(text)) => Some(IrPart::Text {
                        text,
                        signature: None,
                    }),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn read_message(msg: ClaudeMessage) -> Option<IrMessage> {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        // dropped: a role no target format models.
        _ => return None,
    };
    Some(IrMessage {
        role,
        parts: read_parts(msg.content),
    })
}

fn read_parts(content: Value) -> Vec<IrPart> {
    match content {
        Value::String(text) => vec![IrPart::Text {
            text: text,
            signature: None,
        }],
        Value::Array(blocks) => blocks.into_iter().filter_map(read_block).collect(),
        _ => Vec::new(),
    }
}

fn read_block(block: Value) -> Option<IrPart> {
    let Value::Object(mut obj) = block else {
        return None;
    };
    let block_type = obj.get("type").and_then(Value::as_str)?.to_string();

    match block_type.as_str() {
        "text" => Some(IrPart::Text {
            text: take_string(&mut obj, "text"),
            signature: None,
        }),
        "image" => read_image(&obj),
        "tool_use" => Some(IrPart::ToolCall {
            id: take_string(&mut obj, "id"),
            name: take_string(&mut obj, "name"),
            signature: None,
            args: obj
                .remove("input")
                .unwrap_or_else(|| Value::Object(Map::new())),
        }),
        "tool_result" => Some(IrPart::ToolResult {
            call_id: take_string(&mut obj, "tool_use_id"),
            is_error: obj
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            content: read_parts(obj.remove("content").unwrap_or(Value::Null)),
        }),
        "thinking" => Some(IrPart::Thinking {
            text: take_string(&mut obj, "thinking"),
            // Anthropic signed it; only Claude's builder may replay it.
            signature: match obj.remove("signature") {
                Some(Value::String(value)) => Some(Signature {
                    by: Format::Claude,
                    value,
                }),
                _ => None,
            },
        }),
        // Anthropic's sealed reasoning crosses the hub byte-for-byte.
        "redacted_thinking" => Some(IrPart::Sealed {
            by: Format::Claude,
            raw: Value::Object(obj),
        }),
        // dropped: block types no target format models today (`document`, ...).
        _ => None,
    }
}

fn read_image(obj: &Map<String, Value>) -> Option<IrPart> {
    let url = image_block_to_url(obj).ok()?;
    let source = obj.get("source")?.as_object()?;
    let base64 = source.get("type").and_then(Value::as_str) == Some("base64");
    Some(IrPart::Image {
        url,
        media_type: if base64 {
            text_field(source, "media_type")
        } else {
            String::new()
        },
        data: base64.then(|| text_field(source, "data")),
        detail: None,
    })
}

fn read_tools(tools: Option<Vec<ClaudeTool>>) -> Vec<IrTool> {
    tools
        .unwrap_or_default()
        .into_iter()
        .map(|tool| IrTool {
            name: tool.name,
            description: tool.description.unwrap_or_default(),
            schema: tool.input_schema,
            // Claude states no strictness switch for tool arguments.
            strict: None,
        })
        .collect()
}

fn read_tool_choice(value: Option<&Value>) -> Option<IrToolChoice> {
    let value = value?;
    Some(match value.get("type").and_then(Value::as_str) {
        Some("none") => IrToolChoice::None,
        Some("any") => IrToolChoice::Required,
        Some("tool") => match value.get("name").and_then(Value::as_str) {
            Some(name) => IrToolChoice::Named(name.to_string()),
            None => IrToolChoice::Auto,
        },
        // `auto`, plus unnamed/unknown types the pairwise converters rejected.
        _ => IrToolChoice::Auto,
    })
}

fn take_string(obj: &mut Map<String, Value>, key: &str) -> String {
    match obj.remove(key) {
        Some(Value::String(text)) => text,
        _ => String::new(),
    }
}

fn text_field(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::claude::build::{build, build_response};

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/claude/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
    }

    fn request_roundtrip(name: &str) -> IrRequest {
        let wire: ClaudeRequest = serde_json::from_str(&fixture(name)).unwrap();
        let ir1 = read(wire);
        let ir2 = read(build(ir1.clone()));
        assert_eq!(ir1, ir2, "IR roundtrip diverged for {name}");
        ir1
    }

    fn response_roundtrip(name: &str) -> IrResponse {
        let wire: ClaudeResponse = serde_json::from_str(&fixture(name)).unwrap();
        let ir1 = read_response(wire);
        let ir2 = read_response(build_response(ir1.clone()));
        assert_eq!(ir1, ir2, "IR roundtrip diverged for {name}");
        ir1
    }

    fn first_user_text(ir: &IrRequest) -> Option<&str> {
        ir.messages
            .iter()
            .find(|m| m.role == Role::User)
            .and_then(|m| {
                m.parts.iter().find_map(|p| match p {
                    IrPart::Text { text: text, .. } => Some(text.as_str()),
                    _ => None,
                })
            })
    }

    fn tool_names(ir: &IrRequest) -> Vec<&str> {
        ir.tools.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn request_simple_roundtrip() {
        let ir = request_roundtrip("request_simple.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("Hello, how are you?"));
        assert_eq!(tool_names(&ir), Vec::<&str>::new());
    }

    #[test]
    fn request_system_message_roundtrip() {
        let ir = request_roundtrip("request_system_message.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("Hello!"));
        assert_eq!(tool_names(&ir), Vec::<&str>::new());
        assert_eq!(
            ir.system,
            vec![IrPart::Text {
                text: "You are a helpful assistant.".into(),
                signature: None
            }]
        );
    }

    #[test]
    fn request_multi_turn_roundtrip() {
        let ir = request_roundtrip("request_multi_turn.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("What is 2+2?"));
        assert_eq!(tool_names(&ir), Vec::<&str>::new());
        assert_eq!(ir.messages.len(), 3);
    }

    #[test]
    fn request_image_roundtrip() {
        let ir = request_roundtrip("request_image.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("What is in this image?"));
        assert_eq!(tool_names(&ir), Vec::<&str>::new());
        assert_eq!(
            ir.messages[0].parts[1],
            IrPart::Image {
                url: "https://example.com/image.png".into(),
                media_type: String::new(),
                data: None,
                detail: None,
            }
        );
    }

    #[test]
    fn request_tools_roundtrip() {
        let ir = request_roundtrip("request_tools.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("What is the weather in Tokyo?"));
        assert_eq!(tool_names(&ir), vec!["get_weather"]);
        assert_eq!(ir.tool_choice, Some(IrToolChoice::Auto));
    }

    #[test]
    fn request_thinking_roundtrip() {
        let ir = request_roundtrip("request_thinking.json");
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(first_user_text(&ir), Some("hello"));
        assert_eq!(tool_names(&ir), Vec::<&str>::new());
        assert_eq!(ir.reasoning, Reasoning::Budget(4096));
    }

    #[test]
    fn response_simple_roundtrip() {
        let ir = response_roundtrip("response_simple.json");
        assert_eq!(ir.id.as_deref(), Some("msg_test123"));
        assert_eq!(ir.model, "claude-sonnet-4-20250514");
        assert_eq!(
            ir.parts,
            vec![IrPart::Text {
                text: "I'm doing well, thank you for asking!".into(),
                signature: None
            }]
        );
        assert_eq!(ir.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn response_tools_roundtrip() {
        let ir = response_roundtrip("response_tools.json");
        assert_eq!(ir.id.as_deref(), Some("msg_tools456"));
        assert_eq!(
            ir.parts,
            vec![IrPart::ToolCall {
                id: "toolu_abc123".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"location": "Tokyo"}),
                signature: None,
            }]
        );
        assert_eq!(ir.stop_reason, StopReason::ToolUse);
    }
}
