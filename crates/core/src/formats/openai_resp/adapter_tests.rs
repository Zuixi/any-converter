use super::*;
use crate::formats::FormatAdapter;

fn parse_req(json: &str) -> OpenAIResponsesRequest {
    OpenAIResponsesAdapter::parse_request(json.as_bytes()).unwrap()
}

fn parse_resp(json: &str) -> OpenAIResponsesResponse {
    OpenAIResponsesAdapter::parse_response(json.as_bytes()).unwrap()
}

#[test]
fn test_string_input_to_canonical() {
    let req = parse_req(
        r#"{"model":"gpt-4.1","input":"Hello, world!"}"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.model, "gpt-4.1");
    assert_eq!(canonical.turns.len(), 1);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::Text { text } if text == "Hello, world!"
    ));
}

#[test]
fn test_message_input_items_to_canonical() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"Hi"}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello!"}]}
            ]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 2);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert_eq!(canonical.turns[1].role, Role::Assistant);
}

#[test]
fn test_instructions_to_system() {
    let req = parse_req(
        r#"{"model":"gpt-4.1","input":"Hi","instructions":"You are helpful."}"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(
        canonical.system.unwrap().as_text(),
        "You are helpful."
    );
}

#[test]
fn test_tool_definitions_mapping() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": "Search",
            "tools": [{
                "type": "function",
                "name": "search",
                "description": "Search the web",
                "parameters": {"type":"object","properties":{"q":{"type":"string"}}},
                "strict": true
            }],
            "tool_choice": "auto"
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "search");
    assert_eq!(canonical.tools[0].description.as_deref(), Some("Search the web"));
    assert!(matches!(canonical.tool_choice, Some(ToolChoice::Auto)));
}

#[test]
fn test_function_call_output_to_tool_use() {
    let resp = parse_resp(
        r#"{
            "id": "resp_abc",
            "object": "response",
            "created_at": 123,
            "model": "gpt-4.1",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"location\":\"Boston\"}"
            }]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::response_to_canonical(resp).unwrap();
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::ToolUse { id, name, input }
            if id == "call_1" && name == "get_weather" && input["location"] == "Boston"
    ));
    assert_eq!(canonical.stop_reason, StopReason::ToolUse);
}

#[test]
fn test_function_call_output_item_to_tool_result() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "72F and sunny"
            }]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 1);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolResult { tool_use_id, content, is_error }
            if tool_use_id == "call_1" && !is_error
                && matches!(&content[0], ContentBlock::Text { text } if text == "72F and sunny")
    ));
}

#[test]
fn test_response_status_mapping() {
    let completed = parse_resp(
        r#"{"id":"resp_1","object":"response","created_at":1,"model":"m","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]}"#,
    );
    let incomplete = parse_resp(
        r#"{"id":"resp_2","object":"response","created_at":1,"model":"m","status":"incomplete","output":[]}"#,
    );
    assert_eq!(
        OpenAIResponsesAdapter::response_to_canonical(completed)
            .unwrap()
            .stop_reason,
        StopReason::EndTurn
    );
    assert_eq!(
        OpenAIResponsesAdapter::response_to_canonical(incomplete)
            .unwrap()
            .stop_reason,
        StopReason::MaxTokens
    );
}

#[test]
fn test_namespace_tools_flattened_to_canonical_short_names() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": "Use playwright",
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp__playwright",
                    "description": "Playwright tools",
                    "tools": [
                        {
                            "type": "function",
                            "name": "browser_navigate",
                            "description": "Navigate to URL",
                            "parameters": {"type":"object","properties":{"url":{"type":"string"}}},
                            "strict": false
                        },
                        {
                            "type": "function",
                            "name": "browser_snapshot",
                            "description": "Take a page snapshot",
                            "parameters": {"type":"object","properties":{}},
                            "strict": false
                        }
                    ]
                },
                {
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run a shell command",
                    "parameters": {"type":"object","properties":{"cmd":{"type":"string"}}},
                    "strict": true
                }
            ],
            "tool_choice": "auto"
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 3);
    assert_eq!(canonical.tools[0].name, "mcp__playwright__browser_navigate");
    assert_eq!(canonical.tools[0].description.as_deref(), Some("Navigate to URL"));
    assert_eq!(canonical.tools[1].name, "mcp__playwright__browser_snapshot");
    assert_eq!(canonical.tools[2].name, "exec_command");
}

#[test]
fn test_namespace_tool_name_conflict_adds_prefix() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": "test",
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp__server_a",
                    "tools": [{"type": "function", "name": "navigate", "parameters": {"type":"object"}}]
                },
                {
                    "type": "namespace",
                    "name": "mcp__server_b",
                    "tools": [{"type": "function", "name": "navigate", "parameters": {"type":"object"}}]
                }
            ]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 2);
    assert_eq!(canonical.tools[0].name, "mcp__server_a__navigate");
    assert_eq!(canonical.tools[1].name, "mcp__server_b__navigate");
}

#[test]
fn test_namespace_tool_roundtrip_with_function_call() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"navigate to example.com"}]},
                {"type":"function_call","call_id":"call_1","name":"browser_navigate","arguments":"{\"url\":\"https://example.com\"}"},
                {"type":"function_call_output","call_id":"call_1","output":"Navigated to https://example.com"}
            ],
            "tools": [{
                "type": "namespace",
                "name": "mcp__playwright",
                "tools": [{
                    "type": "function",
                    "name": "browser_navigate",
                    "description": "Navigate to URL",
                    "parameters": {"type":"object","properties":{"url":{"type":"string"}}}
                }]
            }]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "mcp__playwright__browser_navigate");
    assert_eq!(canonical.turns.len(), 3);
    assert!(matches!(
        &canonical.turns[1].content[0],
        ContentBlock::ToolUse { name, .. } if name == "browser_navigate"
    ));
    assert!(matches!(
        &canonical.turns[2].content[0],
        ContentBlock::ToolResult { .. }
    ));
}

#[test]
fn test_unknown_tool_types_silently_skipped() {
    let req = parse_req(
        r#"{
            "model": "gpt-4.1",
            "input": "test",
            "tools": [
                {"type": "web_search", "name": "web_search"},
                {"type": "function", "name": "calc", "parameters": {"type":"object"}}
            ]
        }"#,
    );
    let canonical = OpenAIResponsesAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "calc");
}

#[test]
fn test_roundtrip_canonical_responses_canonical() {
    let original = CanonicalRequest {
        system: Some(SystemContent::Text("Be concise.".into())),
        turns: vec![
            Turn {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "What's 2+2?".into() }],
            },
            Turn {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "4".into() }],
            },
        ],
        tools: vec![ToolDef {
            name: "calc".into(),
            description: Some("Calculate".into()),
            input_schema: serde_json::json!({"type":"object"}),
            strict: Some(true),
        }],
        tool_choice: Some(ToolChoice::Auto),
        params: GenerationParams {
            max_output_tokens: Some(100),
            temperature: Some(0.5),
            ..Default::default()
        },
        ..CanonicalRequest::simple("gpt-4.1", "")
    };

    let wire = OpenAIResponsesAdapter::request_from_canonical(&original).unwrap();
    let back = OpenAIResponsesAdapter::request_to_canonical(wire).unwrap();

    assert_eq!(back.model, original.model);
    assert_eq!(back.system.unwrap().as_text(), "Be concise.");
    assert_eq!(back.turns.len(), 2);
    assert_eq!(back.tools.len(), 1);
    assert_eq!(back.params.max_output_tokens, Some(100));
    assert!(matches!(back.tool_choice, Some(ToolChoice::Auto)));
}
