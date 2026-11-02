//! OpenAI Responses wire types → IR.
//!
//! Port of the Responses half of `converters/resp_to_{chat,claude,gemini}.rs`.
//! Those three disagree on details (typeless messages only work in
//! `resp_to_claude`, assistant images survive in claude/gemini but not chat);
//! where they disagree the reader keeps the information and lets the target
//! builder drop it.
//!
//! Reading is infallible: the pairwise converters returned
//! `ConvertError::UnsupportedContentType` for items the hub has no home for,
//! the reader skips them instead.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::convert::Format;
use crate::converters::shared::{
    ITEM_TYPE_FUNCTION_CALL, ITEM_TYPE_FUNCTION_CALL_OUTPUT, ITEM_TYPE_INPUT_IMAGE,
    ITEM_TYPE_INPUT_TEXT, ITEM_TYPE_MESSAGE, ITEM_TYPE_OUTPUT_TEXT, new_resp_id, parse_arguments,
    parse_data_url,
};
use crate::formats::openai_resp::types::{
    OpenAIResponsesRequest, OpenAIResponsesResponse, ResponsesUsage,
};
use crate::ir::{
    IrMessage, IrPart, IrRequest, IrResponse, IrTool, IrToolChoice, NamespacedExtra, Reasoning,
    Role, Sampling, StopReason, Usage,
};

const ITEM_TYPE_REASONING: &str = "reasoning";
const ITEM_TYPE_SUMMARY_TEXT: &str = "summary_text";

pub fn read(req: OpenAIResponsesRequest) -> IrRequest {
    let OpenAIResponsesRequest {
        model,
        input,
        instructions,
        max_output_tokens,
        temperature,
        top_p,
        stream,
        tools,
        tool_choice,
        text,
        reasoning,
        previous_response_id,
        store,
        extra,
    } = req;

    let (mut system_texts, messages) = read_input(input);
    // Every reference reader appends `instructions` after the input's
    // system/developer messages.
    if let Some(instructions) = instructions {
        system_texts.push(instructions);
    }
    let system = if system_texts.is_empty() {
        Vec::new()
    } else {
        // One joined part keeps `read(build(ir)) == ir`: `instructions` is a
        // single string on the wire, so the split points are not recoverable.
        vec![IrPart::Text {
            text: system_texts.join("\n\n"),
            signature: None,
        }]
    };

    let effort = reasoning
        .as_ref()
        .and_then(|v| v.get("effort"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let (tools, ns_names) = read_tools(tools);
    let tool_choice = read_tool_choice(tool_choice, &ns_names);

    let mut extra_bag = NamespacedExtra::default();
    for (name, value) in extra {
        extra_bag.put(Format::OpenAIResponses, &name, value);
    }
    if let Some(value) = text {
        extra_bag.put(Format::OpenAIResponses, "text", value);
    }
    if let Some(value) = reasoning {
        extra_bag.put(Format::OpenAIResponses, "reasoning", value);
    }
    if let Some(value) = previous_response_id {
        extra_bag.put(
            Format::OpenAIResponses,
            "previous_response_id",
            Value::String(value),
        );
    }
    if let Some(value) = store {
        extra_bag.put(Format::OpenAIResponses, "store", Value::Bool(value));
    }

    IrRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        sampling: Sampling {
            max_tokens: max_output_tokens,
            temperature,
            top_p,
            top_k: None,
            stop: Vec::new(),
        },
        stream: stream.unwrap_or(false),
        reasoning: effort.map(Reasoning::Effort).unwrap_or_default(),
        extra: extra_bag,
    }
}

pub fn read_response(resp: OpenAIResponsesResponse) -> IrResponse {
    let OpenAIResponsesResponse {
        id,
        model,
        status,
        output,
        usage,
        ..
    } = resp;

    let mut parts = Vec::new();
    for item in output {
        read_output_item(item, &mut parts);
    }

    // The reference readers map `status: incomplete` to length/max_tokens.
    // `incomplete.details.reason` is not modelled by `OpenAIResponsesResponse`,
    // so only the status itself is available here.
    let stop_reason = if status == "incomplete" {
        StopReason::MaxTokens
    } else {
        StopReason::from_openai_resp(&status)
    };

    IrResponse {
        id: Some(if id.is_empty() { new_resp_id() } else { id }),
        model,
        parts,
        stop_reason,
        stop_sequence: None,
        usage: usage.map(usage_to_ir).unwrap_or_default(),
    }
}

fn usage_to_ir(usage: ResponsesUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage
            .input_tokens_details
            .and_then(|details| details.cached_tokens),
        ..Default::default()
    }
}

// ── request input ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum ItemKind {
    Message,
    FunctionCall,
    FunctionCallOutput,
    Reasoning,
    Other,
}

/// `resp_to_claude` treats an item that only carries `role` as a message.
fn input_item_kind(item: &Map<String, Value>) -> ItemKind {
    match item.get("type").and_then(Value::as_str) {
        Some(ITEM_TYPE_MESSAGE) => ItemKind::Message,
        Some(ITEM_TYPE_FUNCTION_CALL) => ItemKind::FunctionCall,
        Some(ITEM_TYPE_FUNCTION_CALL_OUTPUT) => ItemKind::FunctionCallOutput,
        Some(ITEM_TYPE_REASONING) => ItemKind::Reasoning,
        None if item.contains_key("role") => ItemKind::Message,
        _ => ItemKind::Other,
    }
}

fn read_input(input: Option<Value>) -> (Vec<String>, Vec<IrMessage>) {
    let mut system_texts = Vec::new();
    let mut messages = Vec::new();

    match input {
        None => {}
        Some(Value::String(text)) => push_message(
            &mut messages,
            Role::User,
            vec![IrPart::Text {
                text: text,
                signature: None,
            }],
        ),
        Some(Value::Array(items)) => {
            for item in items {
                read_input_item(item, &mut system_texts, &mut messages);
            }
        }
        Some(_) => {}
    }

    (system_texts, messages)
}

fn read_input_item(item: Value, system_texts: &mut Vec<String>, messages: &mut Vec<IrMessage>) {
    let Value::Object(mut item) = item else {
        return;
    };

    match input_item_kind(&item) {
        ItemKind::Message => {
            let is_instruction = matches!(
                item.get("role").and_then(Value::as_str),
                Some("developer" | "system")
            );
            if is_instruction {
                let text = take_message_text(&mut item);
                if !text.is_empty() {
                    system_texts.push(text);
                }
                return;
            }
            let role = match item.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => return,
            };
            let parts = match item.remove("content") {
                Some(content) => content_to_parts(content),
                None => Vec::new(),
            };
            push_message(messages, role, parts);
        }
        ItemKind::FunctionCall => {
            let call_id = take_string(&mut item, "call_id");
            let name = take_string(&mut item, "name");
            if name.is_empty() {
                return;
            }
            let args = parse_arguments(&take_string(&mut item, "arguments"));
            push_message(
                messages,
                Role::Assistant,
                vec![IrPart::ToolCall {
                    id: call_id,
                    name,
                    signature: None,
                    args,
                }],
            );
        }
        ItemKind::FunctionCallOutput => {
            let call_id = take_string(&mut item, "call_id");
            let output = take_string(&mut item, "output");
            push_message(
                messages,
                Role::User,
                vec![IrPart::ToolResult {
                    call_id,
                    is_error: false,
                    content: vec![IrPart::Text {
                        text: output,
                        signature: None,
                    }],
                }],
            );
        }
        ItemKind::Reasoning => {
            let text = take_reasoning_text(&mut item);
            push_message(
                messages,
                Role::Assistant,
                vec![IrPart::Thinking {
                    text,
                    signature: None,
                }],
            );
        }
        ItemKind::Other => {}
    }
}

/// Turns upstream item order into a canonical IR: adjacent same-role items
/// become one message, so `read(build(ir)) == ir`.
fn push_message(messages: &mut Vec<IrMessage>, role: Role, mut parts: Vec<IrPart>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.role == role {
            last.parts.append(&mut parts);
            return;
        }
    }
    messages.push(IrMessage { role, parts });
}

fn content_to_parts(content: Value) -> Vec<IrPart> {
    match content {
        Value::String(text) => vec![IrPart::Text {
            text: text,
            signature: None,
        }],
        Value::Array(items) => items.into_iter().filter_map(part_from_value).collect(),
        _ => Vec::new(),
    }
}

fn part_from_value(part: Value) -> Option<IrPart> {
    let Value::Object(mut part) = part else {
        return None;
    };
    let is_text = matches!(
        part.get("type").and_then(Value::as_str),
        Some(ITEM_TYPE_INPUT_TEXT | ITEM_TYPE_OUTPUT_TEXT | "text")
    );
    if is_text {
        return Some(IrPart::Text {
            text: take_string(&mut part, "text"),
            signature: None,
        });
    }
    if part.get("type").and_then(Value::as_str) == Some(ITEM_TYPE_INPUT_IMAGE) {
        let url = take_image_url(&mut part);
        if url.is_empty() {
            return None;
        }
        let (media_type, data) = match parse_data_url(&url) {
            Some((media_type, data)) => (media_type, Some(data)),
            None => (String::new(), None),
        };
        return Some(IrPart::Image {
            url,
            media_type,
            data,
            detail: take_opt_string(&mut part, "detail"),
        });
    }
    None
}

/// `extract_message_text` without the copies: joins text parts with `\n`.
fn take_message_text(item: &mut Map<String, Value>) -> String {
    match item.remove("content") {
        Some(Value::String(text)) => text,
        Some(Value::Array(parts)) => parts
            .into_iter()
            .filter_map(|part| {
                let Value::Object(mut part) = part else {
                    return None;
                };
                let is_text = matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text" | "text")
                );
                is_text.then(|| take_string(&mut part, "text"))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn take_reasoning_text(item: &mut Map<String, Value>) -> String {
    match item.remove("summary") {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|summary| {
                let Value::Object(mut summary) = summary else {
                    return None;
                };
                let is_text =
                    summary.get("type").and_then(Value::as_str) == Some(ITEM_TYPE_SUMMARY_TEXT);
                is_text.then(|| take_string(&mut summary, "text"))
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

// ── tools ──────────────────────────────────────────────────────────────

/// Returns the tools plus `short name -> namespace__name` for `tool_choice`
/// qualification.
fn read_tools(tools: Option<Vec<Value>>) -> (Vec<IrTool>, HashMap<String, String>) {
    let mut out = Vec::new();
    let mut ns_names = HashMap::new();
    let Some(tools) = tools else {
        return (out, ns_names);
    };

    for tool in tools {
        let Value::Object(mut tool) = tool else {
            continue;
        };

        if tool.get("type").and_then(Value::as_str) == Some("function") {
            if let Some(ir_tool) = function_tool(tool) {
                out.push(ir_tool);
            }
            continue;
        }
        if tool.get("type").and_then(Value::as_str) == Some("namespace") {
            let namespace = take_string(&mut tool, "name");
            let ns_description = take_opt_string(&mut tool, "description");
            let Some(Value::Array(children)) = tool.remove("tools") else {
                continue;
            };
            for child in children {
                let Value::Object(child) = child else {
                    continue;
                };
                if child.get("type").and_then(Value::as_str) != Some("function") {
                    continue;
                }
                let short_name = child.get("name").and_then(Value::as_str).unwrap_or("");
                let name = if namespace.is_empty() {
                    short_name.to_owned()
                } else {
                    let qualified = format!("{namespace}__{short_name}");
                    ns_names.insert(short_name.to_owned(), qualified.clone());
                    qualified
                };
                if let Some(mut ir_tool) = function_tool(child) {
                    ir_tool.name = name;
                    if ir_tool.description.is_empty() {
                        ir_tool.description = ns_description.clone().unwrap_or_default();
                    }
                    out.push(ir_tool);
                }
            }
        }
    }

    (out, ns_names)
}

fn function_tool(mut tool: Map<String, Value>) -> Option<IrTool> {
    let name = take_string(&mut tool, "name");
    if name.is_empty() {
        return None;
    }
    Some(IrTool {
        name,
        description: take_string(&mut tool, "description"),
        // `resp_to_chat`/`resp_to_gemini` forward a missing schema as null,
        // `resp_to_claude` defaults to an object schema.
        schema: tool
            .remove("parameters")
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        strict: match tool.remove("strict") {
            Some(Value::Bool(strict)) => Some(strict),
            // absent stays absent: Responses' own default is its dialect rule
            _ => None,
        },
    })
}

fn read_tool_choice(
    tool_choice: Option<Value>,
    ns_names: &HashMap<String, String>,
) -> Option<IrToolChoice> {
    let tool_choice = tool_choice?;

    Some(match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "none" => IrToolChoice::None,
            "required" => IrToolChoice::Required,
            _ => IrToolChoice::Auto,
        },
        Value::Object(mut choice) => {
            if choice.get("type").and_then(Value::as_str) != Some("function") {
                return Some(IrToolChoice::Auto);
            }
            // `resp_to_chat` also accepts the nested `function.name` shape.
            let name = match choice.remove("name") {
                Some(Value::String(name)) => name,
                _ => match choice.remove("function") {
                    Some(Value::Object(mut function)) => take_string(&mut function, "name"),
                    _ => String::new(),
                },
            };
            IrToolChoice::Named(match ns_names.get(&name) {
                Some(qualified) => qualified.clone(),
                None => name,
            })
        }
        _ => IrToolChoice::Auto,
    })
}

// ── response output ────────────────────────────────────────────────────

fn read_output_item(item: Value, parts: &mut Vec<IrPart>) {
    let Value::Object(mut item) = item else {
        return;
    };

    let kind = match item.get("type").and_then(Value::as_str) {
        Some(ITEM_TYPE_MESSAGE) => ItemKind::Message,
        Some(ITEM_TYPE_FUNCTION_CALL) => ItemKind::FunctionCall,
        Some(ITEM_TYPE_REASONING) => ItemKind::Reasoning,
        _ => ItemKind::Other,
    };

    match kind {
        ItemKind::Message => {
            let Some(Value::Array(content)) = item.remove("content") else {
                return;
            };
            for part in content {
                let Value::Object(mut part) = part else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) != Some(ITEM_TYPE_OUTPUT_TEXT) {
                    continue;
                }
                let text = take_string(&mut part, "text");
                // The reference readers drop empty text blocks.
                if !text.is_empty() {
                    parts.push(IrPart::Text {
                        text: text,
                        signature: None,
                    });
                }
            }
        }
        ItemKind::FunctionCall => {
            let id = take_string(&mut item, "call_id");
            let name = take_string(&mut item, "name");
            if name.is_empty() {
                return;
            }
            let args = parse_arguments(&take_string(&mut item, "arguments"));
            parts.push(IrPart::ToolCall {
                id,
                name,
                args,
                signature: None,
            });
        }
        ItemKind::Reasoning => {
            let text = take_reasoning_text(&mut item);
            parts.push(IrPart::Thinking {
                text,
                signature: None,
            });
        }
        _ => {}
    }
}

// ── value helpers (move strings out, never clone payloads) ─────────────

fn take_string(map: &mut Map<String, Value>, key: &str) -> String {
    match map.remove(key) {
        Some(Value::String(value)) => value,
        _ => String::new(),
    }
}

fn take_opt_string(map: &mut Map<String, Value>, key: &str) -> Option<String> {
    match map.remove(key) {
        Some(Value::String(value)) => Some(value),
        _ => None,
    }
}

/// Responses accepts `image_url` as a string or as `{"url": ...}`.
fn take_image_url(part: &mut Map<String, Value>) -> String {
    match part.remove("image_url") {
        Some(Value::String(url)) => url,
        Some(Value::Object(mut image)) => take_string(&mut image, "url"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::openai_resp::build::{build, build_response};

    fn request(fixture: &str) -> OpenAIResponsesRequest {
        serde_json::from_str(fixture).expect("valid Responses request fixture")
    }

    fn response(fixture: &str) -> OpenAIResponsesResponse {
        serde_json::from_str(fixture).expect("valid Responses response fixture")
    }

    fn assert_request_roundtrip(fixture: &str) -> IrRequest {
        let ir1 = read(request(fixture));
        let ir2 = read(build(ir1.clone()));
        // build materializes Responses' wire default (strict=true), so a
        // tool that came in without `strict` reads back with it
        let mut ir1_norm = ir1.clone();
        for tool in &mut ir1_norm.tools {
            if tool.strict.is_none() {
                tool.strict = Some(true);
            }
        }
        assert_eq!(ir1_norm, ir2);

        let wire = build(ir1.clone());
        assert_eq!(wire.model, ir1.model, "model lost in {fixture}");
        assert_eq!(
            first_user_text(&ir1),
            first_user_text(&ir2),
            "user text lost in {fixture}"
        );
        let mut original: Vec<_> = ir1.tools.iter().map(|t| t.name.as_str()).collect();
        let mut rebuilt: Vec<_> = ir2.tools.iter().map(|t| t.name.as_str()).collect();
        original.sort_unstable();
        rebuilt.sort_unstable();
        assert_eq!(original, rebuilt, "tool names lost in {fixture}");

        ir1
    }

    fn first_user_text(ir: &IrRequest) -> Option<&str> {
        ir.messages
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| m.parts.iter())
            .find_map(|part| match part {
                IrPart::Text { text: text, .. } => Some(text.as_str()),
                _ => None,
            })
    }

    fn assert_response_roundtrip(fixture: &str) -> IrResponse {
        let ir1 = read_response(response(fixture));
        let ir2 = read_response(build_response(ir1.clone()));
        assert_eq!(ir1, ir2);

        let mut original: Vec<String> = part_names(&ir1);
        let mut rebuilt: Vec<String> = part_names(&ir2);
        original.sort_unstable();
        rebuilt.sort_unstable();
        assert_eq!(original, rebuilt, "parts lost in {fixture}");
        assert_eq!(ir1.model, ir2.model, "model lost in {fixture}");

        ir1
    }

    fn part_names(ir: &IrResponse) -> Vec<String> {
        ir.parts
            .iter()
            .map(|part| match part {
                IrPart::Text { text: text, .. } => format!("text:{text}"),
                IrPart::ToolCall { name, .. } => format!("tool:{name}"),
                IrPart::Thinking { text, .. } => format!("thinking:{text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn request_simple_roundtrips() {
        let ir = assert_request_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/request_simple.json"
        ));
        assert_eq!(ir.model, "gpt-4.1");
        assert_eq!(first_user_text(&ir), Some("Hello, how are you?"));
        assert!(ir.tools.is_empty());
    }

    #[test]
    fn request_multi_turn_roundtrips() {
        let ir = assert_request_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/request_multi_turn.json"
        ));
        assert_eq!(ir.messages.len(), 3);
        assert_eq!(first_user_text(&ir), Some("What is 2+2?"));
        assert_eq!(ir.messages[1].role, Role::Assistant);
    }

    #[test]
    fn request_tools_roundtrips() {
        let ir = assert_request_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/request_tools.json"
        ));
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "get_weather");
        assert!(ir.tools[0].strict.is_none());
        assert_eq!(ir.tool_choice, Some(IrToolChoice::Auto));
    }

    #[test]
    fn request_reasoning_roundtrips() {
        let ir = assert_request_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/request_reasoning.json"
        ));
        assert_eq!(ir.reasoning, Reasoning::Effort("high".into()));
    }

    #[test]
    fn response_simple_roundtrips() {
        let ir = assert_response_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/response_simple.json"
        ));
        assert_eq!(ir.id.as_deref(), Some("resp_test123"));
        assert_eq!(ir.parts.len(), 1);
        assert_eq!(ir.stop_reason, StopReason::EndTurn);
        assert_eq!(ir.usage.input_tokens, 10);
        assert_eq!(ir.usage.output_tokens, 15);
    }

    #[test]
    fn response_tools_roundtrips() {
        let ir = assert_response_roundtrip(include_str!(
            "../../../tests/fixtures/openai_resp/response_tools.json"
        ));
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

    #[test]
    fn instructions_and_system_messages_join_without_losing_text() {
        let ir = read(request(
            r#"{"model":"o1","instructions":"Be brief","input":[
                {"type":"message","role":"system","content":[{"type":"input_text","text":"ctx"}]},
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}
            ]}"#,
        ));

        assert_eq!(
            ir.system,
            vec![IrPart::Text {
                text: "ctx\n\nBe brief".into(),
                signature: None
            }]
        );
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(first_user_text(&ir), Some("hi"));
    }

    #[test]
    fn typeless_message_with_string_content_reads_as_user_text() {
        let ir = read(request(
            r#"{"model":"o1","input":[{"role":"user","content":"hello"}]}"#,
        ));
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(ir.messages[0].role, Role::User);
        assert_eq!(
            ir.messages[0].parts,
            vec![IrPart::Text {
                text: "hello".into(),
                signature: None
            }]
        );
    }

    #[test]
    fn typeless_system_message_reads_as_system() {
        let ir = read(request(
            r#"{"model":"o1","input":[
                {"role":"system","content":[{"type":"text","text":"You are concise."}]},
                {"role":"user","content":"hello"}
            ]}"#,
        ));
        assert_eq!(
            ir.system,
            vec![IrPart::Text {
                text: "You are concise.".into(),
                signature: None
            }]
        );
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(first_user_text(&ir), Some("hello"));
    }

    #[test]
    fn namespace_tool_choice_qualifies_name() {
        let ir = read(request(
            r#"{"model":"gpt-4.1","input":"use shell","tools":[
                {"type":"namespace","name":"mcp","tools":[
                    {"type":"function","name":"shell","parameters":{}}
                ]}
            ],"tool_choice":{"type":"function","name":"shell"}}"#,
        ));

        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "mcp__shell");
        assert_eq!(
            ir.tool_choice,
            Some(IrToolChoice::Named("mcp__shell".into()))
        );
    }

    #[test]
    fn function_call_and_output_become_tool_parts() {
        let ir = read(request(
            r#"{"model":"gpt-4.1","input":[
                {"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"location\":\"Tokyo\"}"},
                {"type":"function_call_output","call_id":"call_1","output":"sunny"}
            ]}"#,
        ));

        assert_eq!(ir.messages.len(), 2);
        assert_eq!(
            ir.messages[0].parts[0],
            IrPart::ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"location": "Tokyo"}),
                signature: None,
            }
        );
        assert_eq!(
            ir.messages[1].parts[0],
            IrPart::ToolResult {
                call_id: "call_1".into(),
                is_error: false,
                content: vec![IrPart::Text {
                    text: "sunny".into(),
                    signature: None
                }],
            }
        );
    }

    #[test]
    fn data_url_image_keeps_media_type_and_data() {
        let ir = read(request(
            r#"{"model":"gpt-4.1","input":[{"type":"message","role":"user","content":[
                {"type":"input_image","image_url":"data:image/png;base64,abc123","detail":"high"}
            ]}]}"#,
        ));

        assert_eq!(
            ir.messages[0].parts[0],
            IrPart::Image {
                url: "data:image/png;base64,abc123".into(),
                media_type: "image/png".into(),
                data: Some("abc123".into()),
                detail: Some("high".into()),
            }
        );
    }

    #[test]
    fn incomplete_status_maps_to_max_tokens() {
        let ir = read_response(response(
            r#"{"id":"resp_x","object":"response","created_at":1,"model":"o1","status":"incomplete","output":[]}"#,
        ));
        assert_eq!(ir.stop_reason, StopReason::MaxTokens);
        assert!(ir.usage.input_tokens == 0);
    }

    #[test]
    fn reasoning_item_summary_reads_as_thinking() {
        let ir = read_response(response(
            r#"{"id":"resp_x","object":"response","created_at":1,"model":"o1","status":"completed","output":[
                {"type":"reasoning","summary":[{"type":"summary_text","text":"deep"}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}
            ]}"#,
        ));

        assert_eq!(
            ir.parts,
            vec![
                IrPart::Thinking {
                    text: "deep".into(),
                    signature: None
                },
                IrPart::Text {
                    text: "answer".into(),
                    signature: None
                }
            ]
        );
    }

    #[test]
    fn cached_tokens_survive_as_cache_read() {
        let ir = read_response(response(
            r#"{"id":"resp_x","object":"response","created_at":1,"model":"o1","status":"completed","output":[],
                "usage":{"input_tokens":10,"output_tokens":2,"input_tokens_details":{"cached_tokens":4}}}"#,
        ));
        assert_eq!(ir.usage.cache_read_tokens, Some(4));
        assert_eq!(read_response(build_response(ir.clone())), ir);
    }
}
