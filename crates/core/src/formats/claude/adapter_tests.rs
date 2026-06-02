use super::*;

fn sample_request_json() -> &'static str {
    r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "Hello"}]
    }"#
}

#[test]
fn test_simple_text_request_to_canonical_roundtrip() {
    let req: ClaudeRequest = serde_json::from_str(sample_request_json()).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.model, "claude-sonnet-4-20250514");
    assert_eq!(canonical.turns.len(), 1);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::Text { text } if text == "Hello"
    ));
    assert_eq!(canonical.params.max_output_tokens, Some(1024));

    let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.model, "claude-sonnet-4-20250514");
    assert_eq!(back.max_tokens, 1024);
    assert_eq!(back.messages.len(), 1);
    assert_eq!(back.messages[0].content, "Hello");
}

#[test]
fn test_system_prompt_string_to_canonical() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "system": "You are helpful.",
        "messages": [{"role": "user", "content": "Hi"}]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert_eq!(
        canonical.system,
        Some(SystemContent::Text("You are helpful.".into()))
    );

    let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.system, Some(serde_json::json!("You are helpful.")));
}

#[test]
fn test_system_prompt_block_array_to_canonical() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "system": [
            {"type": "text", "text": "Line 1"},
            {"type": "text", "text": "Line 2"}
        ],
        "messages": [{"role": "user", "content": "Hi"}]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    match canonical.system.unwrap() {
        SystemContent::Blocks(blocks) => {
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].text, "Line 1");
            assert_eq!(blocks[1].text, "Line 2");
        }
        _ => panic!("expected blocks"),
    }
}

#[test]
fn test_multi_turn_conversation() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Hi there!"},
            {"role": "user", "content": "How are you?"}
        ]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 3);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert_eq!(canonical.turns[1].role, Role::Assistant);
    assert_eq!(canonical.turns[2].role, Role::User);
}

#[test]
fn test_tool_definitions_mapping() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "Search"}],
        "tools": [{
            "name": "search",
            "description": "Search the web",
            "input_schema": {
                "type": "object",
                "properties": {"query": {"type": "string"}}
            }
        }],
        "tool_choice": {"type": "auto"}
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "search");
    assert_eq!(
        canonical.tools[0].input_schema["type"],
        serde_json::json!("object")
    );
    assert!(matches!(canonical.tool_choice, Some(ToolChoice::Auto)));

    let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
    let tools = back.tools.unwrap();
    assert_eq!(tools[0].name, "search");
    assert!(tools[0].input_schema.get("function").is_none());
    assert_eq!(back.tool_choice, Some(serde_json::json!({"type": "auto"})));
}

#[test]
fn test_tool_use_block_in_assistant_message() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [{
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_123",
                "name": "get_weather",
                "input": {"location": "Boston"}
            }]
        }]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolUse { id, name, input }
            if id == "toolu_123" && name == "get_weather" && input["location"] == "Boston"
    ));
}

#[test]
fn test_tool_result_block_in_user_message() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_123",
                "content": "72F and sunny",
                "is_error": false
            }]
        }]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolResult { tool_use_id, is_error, .. }
            if tool_use_id == "toolu_123" && !is_error
    ));
}

#[test]
fn test_image_content_base64_and_url() {
    let json = r#"{
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Look:"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/img.png"}}
            ]
        }]
    }"#;
    let req: ClaudeRequest = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns[0].content.len(), 3);
    assert!(matches!(
        &canonical.turns[0].content[1],
        ContentBlock::Image { source: ImageSource::Base64 { media_type, data } }
            if media_type == "image/png" && data == "abc123"
    ));
    assert!(matches!(
        &canonical.turns[0].content[2],
        ContentBlock::Image { source: ImageSource::Url { url, .. } }
            if url == "https://example.com/img.png"
    ));
}

#[test]
fn test_max_tokens_default_4096() {
    let canonical = CanonicalRequest {
        params: GenerationParams {
            max_output_tokens: None,
            ..Default::default()
        },
        ..CanonicalRequest::simple("claude-sonnet-4-20250514", "Hi")
    };
    let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.max_tokens, DEFAULT_MAX_TOKENS);
}

#[test]
fn test_temperature_clamping_to_0_1() {
    let canonical = CanonicalRequest {
        params: GenerationParams {
            temperature: Some(1.8),
            ..Default::default()
        },
        ..CanonicalRequest::simple("claude-sonnet-4-20250514", "Hi")
    };
    let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.temperature, Some(1.0));
}

#[test]
fn test_response_with_text_content() {
    let json = r#"{
        "id": "msg_abc",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{"type": "text", "text": "Hello!"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    }"#;
    let resp: ClaudeResponse = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::response_to_canonical(resp).unwrap();
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::Text { text } if text == "Hello!"
    ));
    assert_eq!(canonical.stop_reason, StopReason::EndTurn);
    assert_eq!(canonical.usage.input_tokens, 10);
    assert_eq!(canonical.usage.output_tokens, 5);
}

#[test]
fn test_response_with_tool_use_content() {
    let json = r#"{
        "id": "msg_abc",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{
            "type": "tool_use",
            "id": "toolu_456",
            "name": "search",
            "input": {"q": "rust"}
        }],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 20, "output_tokens": 10}
    }"#;
    let resp: ClaudeResponse = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::response_to_canonical(resp).unwrap();
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::ToolUse { id, name, .. } if id == "toolu_456" && name == "search"
    ));
    assert_eq!(canonical.stop_reason, StopReason::ToolUse);
}

#[test]
fn test_response_with_thinking_block() {
    let json = r#"{
        "id": "msg_abc",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{
            "type": "thinking",
            "thinking": "Let me think...",
            "signature": "sig_xyz"
        }],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    }"#;
    let resp: ClaudeResponse = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::response_to_canonical(resp).unwrap();
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::Thinking { text, signature }
            if text == "Let me think..." && signature.as_deref() == Some("sig_xyz")
    ));
}

#[test]
fn test_stop_reason_mapping() {
    let cases = [
        ("end_turn", StopReason::EndTurn),
        ("max_tokens", StopReason::MaxTokens),
        ("tool_use", StopReason::ToolUse),
        ("stop_sequence", StopReason::StopSequence),
    ];
    for (claude_reason, expected) in cases {
        let json = format!(
            r#"{{
                "id": "msg_abc",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "content": [{{"type": "text", "text": "x"}}],
                "stop_reason": "{claude_reason}",
                "usage": {{"input_tokens": 1, "output_tokens": 1}}
            }}"#
        );
        let resp: ClaudeResponse = serde_json::from_str(&json).unwrap();
        let canonical = ClaudeAdapter::response_to_canonical(resp).unwrap();
        assert_eq!(canonical.stop_reason, expected, "failed for {claude_reason}");
    }
}

#[test]
fn test_usage_mapping_including_cache_tokens() {
    let json = r#"{
        "id": "msg_abc",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{"type": "text", "text": "Hi"}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_creation_input_tokens": 20,
            "cache_read_input_tokens": 30
        }
    }"#;
    let resp: ClaudeResponse = serde_json::from_str(json).unwrap();
    let canonical = ClaudeAdapter::response_to_canonical(resp).unwrap();
    assert_eq!(canonical.usage.cache_write_tokens, Some(20));
    assert_eq!(canonical.usage.cache_read_tokens, Some(30));

    let back = ClaudeAdapter::response_from_canonical(&canonical).unwrap();
    assert_eq!(back.usage.cache_creation_input_tokens, Some(20));
    assert_eq!(back.usage.cache_read_input_tokens, Some(30));
}

#[test]
fn test_tool_choice_mapping() {
    let choices = [
        (ToolChoice::Auto, serde_json::json!({"type": "auto"})),
        (ToolChoice::None, serde_json::json!({"type": "none"})),
        (ToolChoice::Any, serde_json::json!({"type": "any"})),
        (
            ToolChoice::Tool {
                name: "search".into(),
            },
            serde_json::json!({"type": "tool", "name": "search"}),
        ),
    ];
    for (choice, expected) in choices {
        let canonical = CanonicalRequest {
            tool_choice: Some(choice),
            ..CanonicalRequest::simple("claude-sonnet-4-20250514", "Hi")
        };
        let back = ClaudeAdapter::request_from_canonical(&canonical).unwrap();
        assert_eq!(back.tool_choice, Some(expected));
    }
}
