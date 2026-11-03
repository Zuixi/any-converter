//! `openai_chat` wire → IR.
//!
//! Ported from the pairwise chat readers (`chat_to_claude`, `chat_to_resp`,
//! `chat_to_gemini`), which stay the semantic reference until the flip.
//!
//! Unlike a pairwise converter this cannot fail: the wire types already
//! guarantee the shape, so an unknown role is kept as a user turn and a
//! `tool_calls` entry without an id carries an empty id instead of erroring.

use serde_json::Value;

use crate::convert::Format;
use crate::converters::shared::{message_content_as_text, parse_arguments};
use crate::formats::openai_chat::*;
use crate::ir::*;

pub fn read(req: OpenAIChatRequest) -> IrRequest {
    let OpenAIChatRequest {
        model,
        messages,
        temperature,
        top_p,
        max_completion_tokens,
        max_tokens,
        stop,
        seed,
        stream,
        stream_options,
        tools,
        tool_choice,
        response_format,
        n,
        reasoning_effort,
        reasoning,
    } = req;

    let mut system = Vec::new();
    let mut turns = Vec::with_capacity(messages.len());
    for msg in messages {
        read_message(msg, &mut system, &mut turns);
    }

    let mut extra = NamespacedExtra::default();
    for (name, value) in [
        ("seed", seed.map(Value::from)),
        ("response_format", response_format),
        ("n", n.map(Value::from)),
        (
            "stream_options",
            stream_options.and_then(|o| serde_json::to_value(o).ok()),
        ),
    ] {
        if let Some(value) = value {
            extra.put(Format::OpenAIChat, name, value);
        }
    }

    IrRequest {
        model,
        system,
        messages: turns,
        tools: tools
            .unwrap_or_default()
            .into_iter()
            .map(read_tool)
            .collect(),
        tool_choice: read_tool_choice(tool_choice),
        sampling: Sampling {
            // every pairwise reader prefers `max_completion_tokens`
            max_tokens: max_completion_tokens.or(max_tokens),
            temperature,
            top_p,
            top_k: None,
            stop: stop.map(stop_values).unwrap_or_default(),
        },
        stream: stream.unwrap_or(false),
        // both spellings arrive; `reasoning_effort` wins as in the readers
        reasoning: match reasoning_effort.or(reasoning.map(|r| r.effort)) {
            Some(effort) => Reasoning::Effort(effort),
            None => Reasoning::Off,
        },
        extra,
    }
}

fn read_message(msg: OpenAIChatMessage, system: &mut Vec<IrPart>, out: &mut Vec<IrMessage>) {
    let OpenAIChatMessage {
        role,
        content,
        tool_call_id,
        tool_calls,
        reasoning_content,
        ..
    } = msg;
    match role.as_str() {
        "system" | "developer" => {
            if let Some(text) = message_content_as_text(content) {
                system.push(IrPart::Text {
                    text,
                    signature: None,
                });
            }
        }
        "assistant" => out.push(IrMessage {
            role: Role::Assistant,
            parts: assistant_parts(content, tool_calls, reasoning_content),
        }),
        "tool" => out.push(IrMessage {
            role: Role::User,
            parts: vec![IrPart::ToolResult {
                call_id: tool_call_id.unwrap_or_default(),
                is_error: false,
                content: tool_result_content(content),
            }],
        }),
        // "user" and anything non-standard, which a pairwise reader rejects
        _ => out.push(IrMessage {
            role: Role::User,
            parts: user_parts(content),
        }),
    }
}

fn user_parts(content: Option<MessageContent>) -> Vec<IrPart> {
    match content {
        None => Vec::new(),
        Some(MessageContent::Text(text)) => vec![IrPart::Text {
            text,
            signature: None,
        }],
        Some(MessageContent::Parts(parts)) => parts.into_iter().map(content_part).collect(),
    }
}

/// Assistant blocks come out in the order the pairwise readers emit them:
/// reasoning first, then content, then tool calls.
fn assistant_parts(
    content: Option<MessageContent>,
    tool_calls: Option<Vec<ToolCall>>,
    reasoning_content: Option<String>,
) -> Vec<IrPart> {
    let mut parts = Vec::new();

    if let Some(text) = reasoning_content.filter(|t| !t.is_empty()) {
        parts.push(IrPart::Thinking {
            text,
            signature: None,
        });
    }

    // empty text is dropped so `build` → `read` cannot lose it
    match content {
        None => {}
        Some(MessageContent::Text(text)) => {
            if !text.is_empty() {
                parts.push(IrPart::Text {
                    text,
                    signature: None,
                });
            }
        }
        Some(MessageContent::Parts(items)) => {
            parts.extend(items.into_iter().filter_map(|p| match p {
                ContentPart::Text { text } if text.is_empty() => None,
                other => Some(content_part(other)),
            }));
        }
    }

    for tc in tool_calls.unwrap_or_default() {
        parts.push(IrPart::ToolCall {
            id: tc.id,
            name: tc.function.name,
            signature: None,
            args: parse_arguments(&tc.function.arguments),
        });
    }

    parts
}

fn tool_result_content(content: Option<MessageContent>) -> Vec<IrPart> {
    match message_content_as_text(content) {
        Some(text) if !text.is_empty() => vec![IrPart::Text {
            text,
            signature: None,
        }],
        _ => Vec::new(),
    }
}

fn content_part(part: ContentPart) -> IrPart {
    match part {
        ContentPart::Text { text } => IrPart::Text {
            text,
            signature: None,
        },
        ContentPart::ImageUrl { image_url } => image_part(image_url),
    }
}

/// A `data:` URL is split so the base64 payload is stored once; any other URL
/// stays in `url`.
fn image_part(ImageUrlDetail { url, detail }: ImageUrlDetail) -> IrPart {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((media_type, data)) = rest.split_once(";base64,") {
            return IrPart::Image {
                url: String::new(),
                media_type: media_type.to_string(),
                data: Some(data.to_string()),
                detail,
            };
        }
    }
    IrPart::Image {
        url,
        media_type: String::new(),
        data: None,
        detail,
    }
}

fn read_tool(tool: OpenAIChatTool) -> IrTool {
    IrTool {
        name: tool.function.name,
        description: tool.function.description.unwrap_or_default(),
        schema: tool
            .function
            .parameters
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        // absent stays absent: the Responses builder's own default is its
        // dialect rule, not the reader's
        strict: tool.function.strict,
    }
}

fn read_tool_choice(choice: Option<Value>) -> Option<IrToolChoice> {
    let choice = choice?;
    Some(match choice {
        Value::String(s) => match s.as_str() {
            "none" => IrToolChoice::None,
            "required" => IrToolChoice::Required,
            _ => IrToolChoice::Auto,
        },
        Value::Object(obj) => {
            let name = (obj.get("type").and_then(Value::as_str) == Some("function"))
                .then(|| obj.get("function")?.get("name")?.as_str())
                .flatten();
            match name {
                Some(name) => IrToolChoice::Named(name.to_string()),
                None => IrToolChoice::Auto,
            }
        }
        _ => IrToolChoice::Auto,
    })
}

fn stop_values(stop: StopValue) -> Vec<String> {
    match stop {
        StopValue::Single(s) => vec![s],
        StopValue::Multiple(v) => v,
    }
}

pub fn read_response(resp: OpenAIChatResponse) -> IrResponse {
    let OpenAIChatResponse {
        id,
        model,
        choices,
        usage,
        ..
    } = resp;

    let mut ir = IrResponse {
        // kept as sent: each target builder normalizes ids to its own prefix
        id: (!id.is_empty()).then_some(id),
        model,
        stop_reason: StopReason::EndTurn,
        usage: usage.map(read_usage).unwrap_or_default(),
        ..Default::default()
    };

    let Some(choice) = choices.into_iter().next() else {
        return ir;
    };

    let mut parts = Vec::with_capacity(2);
    if let Some(text) = choice.message.content.filter(|t| !t.is_empty()) {
        parts.push(IrPart::Text {
            text,
            signature: None,
        });
    }
    if let Some(text) = choice.message.reasoning_content.filter(|t| !t.is_empty()) {
        parts.push(IrPart::Thinking {
            text,
            signature: None,
        });
    }
    for tc in choice.message.tool_calls.unwrap_or_default() {
        parts.push(IrPart::ToolCall {
            id: tc.id,
            name: tc.function.name,
            signature: None,
            args: parse_arguments(&tc.function.arguments),
        });
    }

    ir.parts = parts;
    ir.stop_reason = choice
        .finish_reason
        .as_deref()
        .map(StopReason::from_openai_chat)
        .unwrap_or_default();
    ir
}

fn read_usage(usage: OpenAIUsage) -> Usage {
    Usage {
        // chat's `prompt_tokens` already counts the cached ones; the cached
        // count rides along so a builder can subtract it (Claude does today)
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_tokens: usage.prompt_tokens_details.and_then(|d| d.cached_tokens),
        cache_write_tokens: None,
        cache_write_1h: None,
        reasoning_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::openai_chat::build::{build, build_response};

    fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
        let path = format!(
            "{}/tests/fixtures/openai_chat/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn text_of(parts: &[IrPart]) -> &str {
        parts
            .iter()
            .find_map(|p| match p {
                IrPart::Text { text: t, .. } => Some(t.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }

    /// `read → build → read` must be the same IR, and what every downstream
    /// format needs must survive.
    fn check_request(
        name: &str,
        model: &str,
        first_user_text: &str,
        tool_names: &[&str],
    ) -> IrRequest {
        let ir1 = read(load(name));
        assert_eq!(ir1.model, model, "{name}");
        let first_user = ir1
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .expect("a user turn");
        assert_eq!(text_of(&first_user.parts), first_user_text, "{name}");
        assert_eq!(
            ir1.tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            tool_names,
            "{name}"
        );

        let ir2 = read(build(ir1.clone()));
        assert_eq!(ir1, ir2, "{name}");
        ir1
    }

    #[test]
    fn request_simple() {
        let ir = check_request("request_simple.json", "gpt-4.1", "Hello, how are you?", &[]);
        assert_eq!(ir.sampling.temperature, Some(0.7));
        assert_eq!(ir.sampling.max_tokens, Some(1024));
        assert!(!ir.stream);
    }

    #[test]
    fn request_system_message() {
        let ir = check_request("request_system_message.json", "gpt-4.1", "Hello!", &[]);
        assert_eq!(
            ir.system,
            vec![IrPart::Text {
                text: "You are a helpful assistant.".into(),
                signature: None
            }]
        );
        assert_eq!(ir.messages.len(), 1, "system never stays a message");
    }

    #[test]
    fn request_multi_turn() {
        let ir = check_request("request_multi_turn.json", "gpt-4.1", "What is 2+2?", &[]);
        assert_eq!(ir.messages.len(), 3);
        assert_eq!(ir.messages[1].role, Role::Assistant);
    }

    #[test]
    fn request_image() {
        let ir = check_request(
            "request_image.json",
            "gpt-4.1",
            "What is in this image?",
            &[],
        );
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
    fn request_reasoning() {
        let ir = check_request("request_reasoning.json", "o1", "hello", &[]);
        assert_eq!(
            ir.messages[1].parts[0],
            IrPart::Thinking {
                text: "The user greeted me, I should respond politely.".into(),
                signature: None,
            }
        );
        assert_eq!(
            ir.messages[1].parts[1],
            IrPart::Text {
                text: "hi there".into(),
                signature: None
            }
        );
    }

    #[test]
    fn request_tools() {
        let ir = check_request(
            "request_tools.json",
            "gpt-4.1",
            "What is the weather in Tokyo?",
            &["get_weather"],
        );
        assert_eq!(ir.tool_choice, Some(IrToolChoice::Auto));
        assert_eq!(
            ir.tools[0].description,
            "Get current weather for a location"
        );
        assert_eq!(
            ir.tools[0].schema["required"],
            serde_json::json!(["location"])
        );
    }

    #[test]
    fn tool_turn_pairs_call_and_result() {
        let wire: OpenAIChatRequest = serde_json::from_value(serde_json::json!({
            "model": "gpt-4.1",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "not json"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
            ]
        }))
        .unwrap();
        let ir = read(wire.clone());
        // unparseable arguments fall back to an empty object, as today
        assert_eq!(
            ir.messages[1].parts[0],
            IrPart::ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                args: serde_json::json!({}),
                signature: None,
            }
        );
        assert_eq!(
            ir.messages[2],
            IrMessage {
                role: Role::User,
                parts: vec![IrPart::ToolResult {
                    call_id: "call_1".into(),
                    is_error: false,
                    content: vec![IrPart::Text {
                        text: "sunny".into(),
                        signature: None
                    }],
                }],
            }
        );
        assert_eq!(read(build(ir)), read(wire), "tool turn round trip");
    }

    #[test]
    fn data_url_image_splits_payload_once() {
        let wire: OpenAIChatRequest = serde_json::from_value(serde_json::json!({
            "model": "gpt-4.1",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc123", "detail": "low"}}
            ]}]
        }))
        .unwrap();
        let ir = read(wire.clone());
        assert_eq!(
            ir.messages[0].parts[0],
            IrPart::Image {
                url: String::new(),
                media_type: "image/png".into(),
                data: Some("abc123".into()),
                detail: Some("low".into()),
            }
        );
        assert_eq!(read(build(ir)), read(wire), "data url round trip");
    }

    fn check_response(name: &str, model: &str, text: &str, tool_names: &[&str]) -> IrResponse {
        let ir1 = read_response(load(name));
        assert_eq!(ir1.model, model, "{name}");
        assert_eq!(text_of(&ir1.parts), text, "{name}");
        assert_eq!(
            ir1.parts
                .iter()
                .filter_map(|p| match p {
                    IrPart::ToolCall { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            tool_names,
            "{name}"
        );
        assert!(ir1.id.is_some(), "{name}");

        let ir2 = read_response(build_response(ir1.clone()));
        assert_eq!(ir1, ir2, "{name}");
        ir1
    }

    #[test]
    fn response_simple() {
        let ir = check_response(
            "response_simple.json",
            "gpt-4.1",
            "I'm doing well, thank you for asking!",
            &[],
        );
        assert_eq!(ir.stop_reason, StopReason::EndTurn);
        assert_eq!(ir.usage.input_tokens, 10);
        assert_eq!(ir.usage.output_tokens, 15);
        assert_eq!(ir.usage.cache_read_tokens, None);
    }

    #[test]
    fn response_cached_tokens_survive() {
        let wire: OpenAIChatResponse = serde_json::from_value(serde_json::json!({
            "id": "chatcmpl-x",
            "object": "chat.completion",
            "created": 1700000000u64,
            "model": "gpt-4.1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi", "reasoning_content": "thinking"},
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 30,
                "completion_tokens": 5,
                "total_tokens": 35,
                "prompt_tokens_details": {"cached_tokens": 20}
            }
        }))
        .unwrap();
        let ir = read_response(wire.clone());
        assert_eq!(ir.stop_reason, StopReason::MaxTokens);
        assert_eq!(
            ir.usage.input_tokens, 30,
            "cached tokens stay inside the total"
        );
        assert_eq!(ir.usage.cache_read_tokens, Some(20));
        assert_eq!(
            ir.parts,
            vec![
                IrPart::Text {
                    text: "hi".into(),
                    signature: None
                },
                IrPart::Thinking {
                    text: "thinking".into(),
                    signature: None,
                },
            ]
        );
        assert_eq!(
            read_response(build_response(ir)),
            read_response(wire),
            "usage round trip"
        );
    }

    #[test]
    fn response_tools() {
        let ir = check_response("response_tools.json", "gpt-4.1", "", &["get_weather"]);
        assert_eq!(ir.stop_reason, StopReason::ToolUse);
        assert_eq!(
            ir.parts[0],
            IrPart::ToolCall {
                id: "call_abc123".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"location": "Tokyo"}),
                signature: None,
            }
        );
    }
}
