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
    IrMessage, IrPart, IrRequest, IrResponse, IrTool, IrToolChoice, Reasoning, Role, StopReason,
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
            IrPart::Text { text: text, .. } => {
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
            IrPart::Thinking { text, .. } => {
                flush_text(&mut output, &mut text_content);
                output.push(reasoning_item(text));
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
            IrPart::Text { text: text, .. } => Some(text),
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
                IrPart::Text { text: text, .. } => content.push(serde_json::json!({
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
            IrPart::Text { text: text, .. } => Some(text),
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

fn reasoning_item(text: String) -> Value {
    serde_json::json!({
        "type": ITEM_TYPE_REASONING,
        "id": format!("rs_{}", new_uuid()),
        "summary": [{"type": ITEM_TYPE_SUMMARY_TEXT, "text": text}],
    })
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
