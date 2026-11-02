//! IR → `openai_chat` wire.
//!
//! Ported from the pairwise chat builders (`claude_to_chat`, `resp_to_chat`,
//! `gemini_to_chat`), which stay the semantic reference until the flip.
//! Consuming: strings and payloads are moved into the wire types.

use serde_json::Value;

use crate::convert::Format;
use crate::converters::reasoning;
use crate::converters::shared::{normalize_id_to_chat, now_unix_secs};
use crate::formats::claude::ThinkingConfig;
use crate::formats::openai_chat::*;
use crate::ir::*;

pub fn build(ir: IrRequest) -> OpenAIChatRequest {
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

    let mut out = Vec::with_capacity(system.len() + messages.len());
    // one `system` message per part: chat carries several system turns, and
    // re-reading them has to give the same parts back
    for part in system {
        if let IrPart::Text(text) = part {
            out.push(message("system", Some(MessageContent::Text(text))));
        }
    }
    for msg in messages {
        match msg.role {
            Role::Assistant => out.push(assistant_message(msg.parts)),
            Role::User => push_user_messages(msg.parts, &mut out),
        }
    }

    let mut seed = None;
    let mut stream_options = None;
    let mut response_format = None;
    let mut n = None;
    for (name, value) in extra.take(Format::OpenAIChat) {
        match name.as_str() {
            "seed" => seed = value.as_i64(),
            "stream_options" => stream_options = serde_json::from_value(value).ok(),
            "response_format" => response_format = Some(value),
            "n" => n = value.as_u64().map(|n| n as u32),
            _ => {}
        }
    }

    OpenAIChatRequest {
        model,
        messages: out,
        temperature: sampling.temperature,
        top_p: sampling.top_p,
        max_completion_tokens: sampling.max_tokens,
        max_tokens: None,
        stop: stop_value(sampling.stop),
        seed,
        // `false` and an absent field mean the same to an upstream
        stream: stream.then_some(true),
        stream_options,
        tools: (!tools.is_empty()).then(|| tools.into_iter().map(tool_wire).collect()),
        tool_choice: tool_choice_value(tool_choice),
        response_format,
        n,
        reasoning_effort: match reasoning {
            Reasoning::Effort(effort) => Some(effort),
            // chat has no budget field; reuse the shared budget→effort table
            // so a Claude-sourced IR keeps the effort it asks for today
            Reasoning::Budget(budget) => reasoning::thinking_to_reasoning_effort(&ThinkingConfig {
                r#type: "enabled".into(),
                budget_tokens: budget,
            }),
            Reasoning::Off => None,
        },
        reasoning: None,
    }
}

fn message(role: &str, content: Option<MessageContent>) -> OpenAIChatMessage {
    OpenAIChatMessage {
        role: role.into(),
        content,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

/// A user turn's text/images go out as one message; each `tool_result` goes
/// out as its own `tool` message behind it, as the pairwise builders do.
fn push_user_messages(parts: Vec<IrPart>, out: &mut Vec<OpenAIChatMessage>) {
    let mut content = Vec::new();
    let mut results = Vec::new();
    for part in parts {
        match part {
            IrPart::ToolResult {
                call_id,
                content: inner,
                ..
            } => results.push((call_id, inner)),
            other => content.push(other),
        }
    }

    if let Some(content) = message_content(content) {
        out.push(message("user", Some(content)));
    }
    for (call_id, inner) in results {
        let text = text_of(inner);
        let content = (!text.is_empty()).then_some(MessageContent::Text(text));
        let mut msg = message("tool", content);
        msg.tool_call_id = Some(call_id);
        out.push(msg);
    }
}

fn assistant_message(parts: Vec<IrPart>) -> OpenAIChatMessage {
    let mut content = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning_content = None;

    for part in parts {
        match part {
            IrPart::Text(_) | IrPart::Image { .. } => content.push(part),
            IrPart::ToolCall { id, name, args } => tool_calls.push(ToolCall {
                id,
                r#type: "function".into(),
                function: FunctionCall {
                    name,
                    arguments: arguments_string(args),
                },
            }),
            IrPart::Thinking { text, .. } => reasoning_content = Some(text),
            // no chat field for tool output, sealed reasoning or signatures
            IrPart::ToolResult { .. } | IrPart::Sealed { .. } => {}
        }
    }

    let mut msg = message("assistant", message_content(content));
    msg.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    msg.reasoning_content = reasoning_content.filter(|r| !r.is_empty());
    msg
}

/// Text and images are all chat can carry; an image anywhere forces the parts
/// form, otherwise the texts are concatenated.
fn message_content(parts: Vec<IrPart>) -> Option<MessageContent> {
    let has_image = parts.iter().any(|p| matches!(p, IrPart::Image { .. }));

    if has_image {
        let mut items = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                IrPart::Text(text) => items.push(ContentPart::Text { text }),
                IrPart::Image { .. } => items.push(ContentPart::ImageUrl {
                    image_url: image_url(part),
                }),
                _ => {}
            }
        }
        return (!items.is_empty()).then_some(MessageContent::Parts(items));
    }

    let text = text_of(parts);
    (!text.is_empty()).then_some(MessageContent::Text(text))
}

fn image_url(part: IrPart) -> ImageUrlDetail {
    match part {
        IrPart::Image {
            url,
            media_type,
            data,
            detail,
        } => ImageUrlDetail {
            url: match data {
                Some(data) => format!("data:{media_type};base64,{data}"),
                None => url,
            },
            detail,
        },
        // caller only passes Image parts
        _ => ImageUrlDetail {
            url: String::new(),
            detail: None,
        },
    }
}

fn text_of(parts: Vec<IrPart>) -> String {
    parts
        .into_iter()
        .filter_map(|p| match p {
            IrPart::Text(text) => Some(text),
            _ => None,
        })
        .collect()
}

fn tool_wire(tool: IrTool) -> OpenAIChatTool {
    OpenAIChatTool {
        r#type: "function".into(),
        function: FunctionDef {
            name: tool.name,
            description: (!tool.description.is_empty()).then_some(tool.description),
            parameters: Some(tool.schema),
            strict: tool.strict,
        },
    }
}

fn tool_choice_value(choice: Option<IrToolChoice>) -> Option<Value> {
    match choice? {
        // an explicit `auto` stays explicit; absent stays absent
        IrToolChoice::Auto => Some(Value::String("auto".into())),
        IrToolChoice::None => Some(Value::String("none".into())),
        IrToolChoice::Required => Some(Value::String("required".into())),
        IrToolChoice::Named(name) => Some(serde_json::json!({
            "type": "function",
            "function": { "name": name }
        })),
    }
}

fn stop_value(stop: Vec<String>) -> Option<StopValue> {
    match stop.len() {
        0 => None,
        1 => Some(StopValue::Single(
            stop.into_iter().next().unwrap_or_default(),
        )),
        _ => Some(StopValue::Multiple(stop)),
    }
}

fn arguments_string(args: Value) -> String {
    serde_json::to_string(&args).unwrap_or_else(|_| "{}".into())
}

pub fn build_response(ir: IrResponse) -> OpenAIChatResponse {
    let IrResponse {
        id,
        model,
        parts,
        stop_reason,
        usage,
        ..
    } = ir;

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning_content = None;

    for part in parts {
        match part {
            IrPart::Text(text) => {
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
            // the last thinking block wins, as in the pairwise builders
            IrPart::Thinking { text, .. } => reasoning_content = Some(text),
            IrPart::ToolCall { id, name, args } => tool_calls.push(ToolCall {
                id,
                r#type: "function".into(),
                function: FunctionCall {
                    name,
                    arguments: arguments_string(args),
                },
            }),
            // chat replies carry text, thinking and tool calls only
            IrPart::Image { .. } | IrPart::ToolResult { .. } | IrPart::Sealed { .. } => {}
        }
    }

    let content = (!text_parts.is_empty()).then(|| text_parts.concat());

    let total_tokens = usage.total_tokens();

    OpenAIChatResponse {
        id: normalize_id_to_chat(id.as_deref().unwrap_or("")),
        object: "chat.completion".into(),
        created: now_unix_secs(),
        model,
        choices: vec![Choice {
            index: 0,
            message: ChoiceMessage {
                role: "assistant".into(),
                content,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                refusal: None,
                reasoning_content: reasoning_content.filter(|r| !r.is_empty()),
            },
            finish_reason: Some(stop_reason.to_openai_chat().into()),
        }],
        usage: Some(OpenAIUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens,
            prompt_tokens_details: usage.cache_read_tokens.map(|cached| PromptTokensDetails {
                cached_tokens: Some(cached),
            }),
        }),
        system_fingerprint: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_maps_to_effort_like_the_claude_converter() {
        let mut ir = IrRequest {
            reasoning: Reasoning::Budget(8192),
            ..Default::default()
        };
        let wire = build(ir.clone());
        assert_eq!(wire.reasoning_effort.as_deref(), Some("high"));

        ir.reasoning = Reasoning::Off;
        assert_eq!(build(ir).reasoning_effort, None);
    }

    #[test]
    fn extra_fields_come_back() {
        let mut ir = IrRequest {
            model: "gpt-4.1".into(),
            stream: true,
            ..Default::default()
        };
        ir.extra
            .put(Format::OpenAIChat, "seed", serde_json::json!(7));
        let wire = build(ir);
        assert_eq!(wire.seed, Some(7));
        assert_eq!(wire.stream, Some(true));
    }
}
