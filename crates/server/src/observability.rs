use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestTraceSummary {
    pub client: TraceBodySummary,
    pub upstream: TraceBodySummary,
    pub response: TraceBodySummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceBodySummary {
    pub messages: Vec<TraceMessage>,
    pub tool_definitions: Vec<TraceToolDefinition>,
    pub tool_calls: Vec<TraceToolCall>,
    pub tool_results: Vec<TraceToolResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMessage {
    pub role: String,
    pub content_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub arguments_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content_preview: String,
}

pub fn summarize_request_trace(
    client_body: &[u8],
    client_format: Format,
    upstream_body: &[u8],
    upstream_format: Format,
    response_body: &[u8],
    response_format: Format,
    max_preview_bytes: usize,
) -> RequestTraceSummary {
    RequestTraceSummary {
        client: summarize_json_body(client_body, client_format, max_preview_bytes),
        upstream: summarize_json_body(upstream_body, upstream_format, max_preview_bytes),
        response: summarize_json_body(response_body, response_format, max_preview_bytes),
    }
}

pub fn summarize_stream_trace(
    client_body: &[u8],
    client_format: Format,
    upstream_body: &[u8],
    upstream_format: Format,
    sse_lines: &[String],
    response_format: Format,
    max_preview_bytes: usize,
) -> RequestTraceSummary {
    RequestTraceSummary {
        client: summarize_json_body(client_body, client_format, max_preview_bytes),
        upstream: summarize_json_body(upstream_body, upstream_format, max_preview_bytes),
        response: summarize_sse_lines(sse_lines, response_format, max_preview_bytes),
    }
}

fn summarize_json_body(body: &[u8], format: Format, max_preview_bytes: usize) -> TraceBodySummary {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return TraceBodySummary::default();
    };
    match format {
        Format::Claude => summarize_claude(&value, max_preview_bytes),
        Format::OpenAIResponses => summarize_responses(&value, max_preview_bytes),
        Format::OpenAIChat => summarize_openai_chat(&value, max_preview_bytes),
        Format::Gemini => summarize_gemini(&value, max_preview_bytes),
    }
}

fn summarize_claude(value: &Value, max_preview_bytes: usize) -> TraceBodySummary {
    let mut summary = TraceBodySummary::default();

    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                summary.tool_definitions.push(TraceToolDefinition {
                    name: name.to_string(),
                    namespace: None,
                });
            }
        }
    }

    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let content = message.get("content").unwrap_or(&Value::Null);
            summary.messages.push(TraceMessage {
                role,
                content_preview: preview_value(content, max_preview_bytes),
            });
            collect_claude_content(content, &mut summary, max_preview_bytes);
        }
    }

    if let Some(content) = value.get("content") {
        collect_claude_content(content, &mut summary, max_preview_bytes);
    }

    summary
}

fn collect_claude_content(
    content: &Value,
    summary: &mut TraceBodySummary,
    max_preview_bytes: usize,
) {
    let Some(blocks) = content.as_array() else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    summary.tool_calls.push(TraceToolCall {
                        id: block.get("id").and_then(Value::as_str).map(String::from),
                        name: name.to_string(),
                        namespace: None,
                        arguments_preview: preview_value(
                            block.get("input").unwrap_or(&Value::Null),
                            max_preview_bytes,
                        ),
                    });
                }
            }
            Some("tool_result") => {
                summary.tool_results.push(TraceToolResult {
                    id: block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(String::from),
                    content_preview: preview_value(
                        block.get("content").unwrap_or(&Value::Null),
                        max_preview_bytes,
                    ),
                });
            }
            _ => {}
        }
    }
}

fn summarize_responses(value: &Value, max_preview_bytes: usize) -> TraceBodySummary {
    let mut summary = TraceBodySummary::default();

    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        collect_responses_tools(tools, &mut summary, None);
    }

    if let Some(input) = value.get("input") {
        collect_responses_items(input, &mut summary, max_preview_bytes);
    }
    if let Some(output) = value.get("output") {
        collect_responses_items(output, &mut summary, max_preview_bytes);
    }
    if let Some(response) = value.get("response") {
        if let Some(output) = response.get("output") {
            collect_responses_items(output, &mut summary, max_preview_bytes);
        }
    }

    summary
}

fn collect_responses_tools(
    tools: &[Value],
    summary: &mut TraceBodySummary,
    namespace: Option<&str>,
) {
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let ns = tool.get("name").and_then(Value::as_str);
                if let Some(inner) = tool.get("tools").and_then(Value::as_array) {
                    collect_responses_tools(inner, summary, ns);
                }
            }
            _ => {
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    summary.tool_definitions.push(TraceToolDefinition {
                        name: name.to_string(),
                        namespace: namespace.map(String::from).or_else(|| {
                            tool.get("namespace")
                                .and_then(Value::as_str)
                                .map(String::from)
                        }),
                    });
                }
            }
        }
    }
}

fn collect_responses_items(
    items: &Value,
    summary: &mut TraceBodySummary,
    max_preview_bytes: usize,
) {
    match items {
        Value::String(text) => summary.messages.push(TraceMessage {
            role: "user".to_string(),
            content_preview: preview_str(text, max_preview_bytes),
        }),
        Value::Array(items) => {
            for item in items {
                collect_responses_item(item, summary, max_preview_bytes);
            }
        }
        Value::Object(_) => collect_responses_item(items, summary, max_preview_bytes),
        _ => {}
    }
}

fn collect_responses_item(item: &Value, summary: &mut TraceBodySummary, max_preview_bytes: usize) {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                summary.tool_calls.push(TraceToolCall {
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    name: name.to_string(),
                    namespace: item
                        .get("namespace")
                        .and_then(Value::as_str)
                        .map(String::from),
                    arguments_preview: preview_value(
                        item.get("arguments").unwrap_or(&Value::Null),
                        max_preview_bytes,
                    ),
                });
            }
        }
        Some("function_call_output") => {
            summary.tool_results.push(TraceToolResult {
                id: item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .map(String::from),
                content_preview: preview_value(
                    item.get("output").unwrap_or(&Value::Null),
                    max_preview_bytes,
                ),
            });
        }
        _ => {
            if let Some(role) = item.get("role").and_then(Value::as_str) {
                summary.messages.push(TraceMessage {
                    role: role.to_string(),
                    content_preview: preview_value(
                        item.get("content").unwrap_or(item),
                        max_preview_bytes,
                    ),
                });
            }
        }
    }
}

fn summarize_openai_chat(value: &Value, max_preview_bytes: usize) -> TraceBodySummary {
    let mut summary = TraceBodySummary::default();

    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            let function = tool.get("function").unwrap_or(tool);
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                summary.tool_definitions.push(TraceToolDefinition {
                    name: name.to_string(),
                    namespace: None,
                });
            }
        }
    }

    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            summary.messages.push(TraceMessage {
                role,
                content_preview: preview_value(
                    message.get("content").unwrap_or(&Value::Null),
                    max_preview_bytes,
                ),
            });
            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    let function = call.get("function").unwrap_or(call);
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        summary.tool_calls.push(TraceToolCall {
                            id: call.get("id").and_then(Value::as_str).map(String::from),
                            name: name.to_string(),
                            namespace: None,
                            arguments_preview: preview_value(
                                function.get("arguments").unwrap_or(&Value::Null),
                                max_preview_bytes,
                            ),
                        });
                    }
                }
            }
            if message.get("role").and_then(Value::as_str) == Some("tool") {
                summary.tool_results.push(TraceToolResult {
                    id: message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .map(String::from),
                    content_preview: preview_value(
                        message.get("content").unwrap_or(&Value::Null),
                        max_preview_bytes,
                    ),
                });
            }
        }
    }

    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(message) = choice.get("message") {
                let wrapper = serde_json::json!({ "messages": [message] });
                let nested = summarize_openai_chat(&wrapper, max_preview_bytes);
                summary.tool_calls.extend(nested.tool_calls);
                summary.tool_results.extend(nested.tool_results);
                summary.messages.extend(nested.messages);
            }
        }
    }

    summary
}

fn summarize_gemini(value: &Value, max_preview_bytes: usize) -> TraceBodySummary {
    let mut summary = TraceBodySummary::default();

    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if let Some(functions) = tool.get("functionDeclarations").and_then(Value::as_array) {
                for function in functions {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        summary.tool_definitions.push(TraceToolDefinition {
                            name: name.to_string(),
                            namespace: None,
                        });
                    }
                }
            }
        }
    }

    if let Some(contents) = value.get("contents").and_then(Value::as_array) {
        for content in contents {
            summary.messages.push(TraceMessage {
                role: content
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                content_preview: preview_value(
                    content.get("parts").unwrap_or(content),
                    max_preview_bytes,
                ),
            });
        }
    }

    summary
}

fn summarize_sse_lines(
    lines: &[String],
    format: Format,
    max_preview_bytes: usize,
) -> TraceBodySummary {
    let mut summary = TraceBodySummary::default();
    for line in lines {
        for value in parse_sse_json_values(line) {
            match format {
                Format::OpenAIResponses => {
                    collect_responses_stream_event(&value, &mut summary, max_preview_bytes)
                }
                Format::OpenAIChat => {
                    let nested = summarize_openai_chat(&value, max_preview_bytes);
                    merge_summary(&mut summary, nested);
                }
                Format::Claude => {
                    let nested = summarize_claude(&value, max_preview_bytes);
                    merge_summary(&mut summary, nested);
                }
                Format::Gemini => {
                    let nested = summarize_gemini(&value, max_preview_bytes);
                    merge_summary(&mut summary, nested);
                }
            }
        }
    }
    summary
}

fn collect_responses_stream_event(
    value: &Value,
    summary: &mut TraceBodySummary,
    max_preview_bytes: usize,
) {
    if matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done") | Some("response.output_item.added")
    ) {
        if let Some(item) = value.get("item") {
            collect_responses_item(item, summary, max_preview_bytes);
        }
    }
    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
        let nested = summarize_responses(value, max_preview_bytes);
        merge_summary(summary, nested);
    }
}

fn parse_sse_json_values(line: &str) -> Vec<Value> {
    line.lines()
        .filter_map(|raw| raw.strip_prefix("data: "))
        .filter(|data| !data.trim().is_empty() && data.trim() != "[DONE]")
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .collect()
}

fn merge_summary(target: &mut TraceBodySummary, source: TraceBodySummary) {
    target.messages.extend(source.messages);
    target.tool_definitions.extend(source.tool_definitions);
    target.tool_calls.extend(source.tool_calls);
    target.tool_results.extend(source.tool_results);
}

fn preview_value(value: &Value, max_preview_bytes: usize) -> String {
    match value {
        Value::String(s) => preview_str(s, max_preview_bytes),
        Value::Null => String::new(),
        other => preview_str(
            &serde_json::to_string(other).unwrap_or_default(),
            max_preview_bytes,
        ),
    }
}

/// Prefix of `text` capped at `max_bytes`, never splitting a UTF-8 code point.
pub(crate) fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn preview_str(text: &str, max_preview_bytes: usize) -> String {
    if text.len() <= max_preview_bytes {
        return text.to_string();
    }
    format!("{}...<truncated>", utf8_prefix(text, max_preview_bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn utf8_prefix_does_not_split_multibyte_char() {
        // Each CJK ideograph is 3 UTF-8 bytes; index 200-style cuts must stay on boundaries.
        let text = "ab配置cd";
        assert_eq!(utf8_prefix(text, 2), "ab");
        assert_eq!(utf8_prefix(text, 3), "ab"); // would split '配'
        assert_eq!(utf8_prefix(text, 4), "ab");
        assert_eq!(utf8_prefix(text, 5), "ab配");
        assert_eq!(utf8_prefix(text, 200), text);
    }

    #[test]
    fn summarizes_responses_namespace_tools_and_function_calls() {
        let body = br#"{
            "model":"gpt-5.4",
            "input":[{"role":"user","content":"read README"}],
            "tools":[{"type":"namespace","name":"shell","tools":[{"name":"exec_command","description":"run command"}]}]
        }"#;
        let response = br#"{
            "output":[{"type":"function_call","call_id":"call_1","name":"exec_command","namespace":"shell","arguments":"{\"cmd\":\"pwd\"}"}],
            "usage":{"input_tokens":10,"output_tokens":5}
        }"#;

        let summary = summarize_request_trace(
            body,
            Format::OpenAIResponses,
            body,
            Format::OpenAIResponses,
            response,
            Format::OpenAIResponses,
            200,
        );

        assert_eq!(summary.client.messages[0].role, "user");
        assert_eq!(
            summary.client.tool_definitions[0].namespace.as_deref(),
            Some("shell")
        );
        assert_eq!(summary.client.tool_definitions[0].name, "exec_command");
        assert_eq!(
            summary.response.tool_calls[0].namespace.as_deref(),
            Some("shell")
        );
        assert_eq!(summary.response.tool_calls[0].name, "exec_command");
        assert!(
            summary.response.tool_calls[0]
                .arguments_preview
                .contains("pwd")
        );
    }

    #[test]
    fn summarizes_claude_tool_use_and_tool_result() {
        let request = br#"{
            "model":"claude",
            "messages":[
                {"role":"user","content":"hello"},
                {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"search","input":{"q":"rust"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"found"}]}
            ],
            "tools":[{"name":"search","input_schema":{"type":"object"}}]
        }"#;

        let summary = summarize_request_trace(
            request,
            Format::Claude,
            request,
            Format::Claude,
            br#"{"content":[]}"#,
            Format::Claude,
            200,
        );

        assert_eq!(summary.client.messages.len(), 3);
        assert_eq!(summary.client.tool_definitions[0].name, "search");
        assert_eq!(summary.client.tool_calls[0].id.as_deref(), Some("toolu_1"));
        assert_eq!(summary.client.tool_calls[0].name, "search");
        assert_eq!(
            summary.client.tool_results[0].id.as_deref(),
            Some("toolu_1")
        );
        assert!(
            summary.client.tool_results[0]
                .content_preview
                .contains("found")
        );
    }

    #[test]
    fn summarizes_responses_stream_function_call_done() {
        let lines = vec![r#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"exec_command","namespace":"shell","arguments":"{\"cmd\":\"head README.md\"}"}}

"#
        .to_string()];

        let summary = summarize_stream_trace(
            br#"{"model":"gpt-5.4","input":"hi"}"#,
            Format::OpenAIResponses,
            br#"{"model":"kimi","messages":[]}"#,
            Format::Claude,
            &lines,
            Format::OpenAIResponses,
            200,
        );

        assert_eq!(summary.response.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            summary.response.tool_calls[0].namespace.as_deref(),
            Some("shell")
        );
        assert!(
            summary.response.tool_calls[0]
                .arguments_preview
                .contains("head README.md")
        );
    }
}
