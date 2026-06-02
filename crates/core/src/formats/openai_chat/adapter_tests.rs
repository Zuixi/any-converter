use super::*;

fn sample_request() -> OpenAIChatRequest {
    OpenAIChatRequest {
        model: "gpt-4o".into(),
        messages: vec![OpenAIChatMessage {
            role: "user".into(),
            content: Some(MessageContent::Text("Hello".into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        temperature: None,
        top_p: None,
        max_completion_tokens: None,
        max_tokens: None,
        stop: None,
        seed: None,
        stream: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        response_format: None,
        n: None,
    }
}

#[test]
fn test_simple_request_to_canonical() {
    let req = sample_request();
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.model, "gpt-4o");
    assert_eq!(canonical.turns.len(), 1);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::Text { text } if text == "Hello"
    ));
}

#[test]
fn test_simple_request_roundtrip() {
    let req = sample_request();
    let canonical = OpenAIChatAdapter::request_to_canonical(req.clone()).unwrap();
    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.model, req.model);
    assert_eq!(back.messages.len(), 1);
    assert_eq!(back.messages[0].role, "user");
    assert!(matches!(
        back.messages[0].content,
        Some(MessageContent::Text(ref t)) if t == "Hello"
    ));
}

#[test]
fn test_multi_turn_conversation() {
    let req = OpenAIChatRequest {
        messages: vec![
            OpenAIChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("Hi".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            OpenAIChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::Text("Hello!".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            OpenAIChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("How are you?".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 3);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert_eq!(canonical.turns[1].role, Role::Assistant);
    assert_eq!(canonical.turns[2].role, Role::User);
}

#[test]
fn test_system_message_extraction() {
    let req = OpenAIChatRequest {
        messages: vec![
            OpenAIChatMessage {
                role: "system".into(),
                content: Some(MessageContent::Text("You are helpful.".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            OpenAIChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("Hi".into())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(
        canonical.system.unwrap().as_text(),
        "You are helpful."
    );
    assert_eq!(canonical.turns.len(), 1);
}

#[test]
fn test_system_message_injection() {
    let canonical = CanonicalRequest {
        system: Some(SystemContent::Text("Be concise.".into())),
        ..CanonicalRequest::simple("gpt-4o", "Hello")
    };
    let req = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(req.messages[0].role, "system");
    assert!(matches!(
        req.messages[0].content,
        Some(MessageContent::Text(ref t)) if t == "Be concise."
    ));
    assert_eq!(req.messages[1].role, "user");
}

#[test]
fn test_developer_role_as_system() {
    let req = OpenAIChatRequest {
        messages: vec![OpenAIChatMessage {
            role: "developer".into(),
            content: Some(MessageContent::Text("Dev instructions.".into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(
        canonical.system.unwrap().as_text(),
        "Dev instructions."
    );
    assert!(canonical.turns.is_empty());
}

#[test]
fn test_tool_definitions_mapping() {
    let req = OpenAIChatRequest {
        tools: Some(vec![OpenAIChatTool {
            r#type: "function".into(),
            function: FunctionDef {
                name: "get_weather".into(),
                description: Some("Get weather".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": { "location": { "type": "string" } }
                })),
                strict: Some(true),
            },
        }]),
        tool_choice: Some(serde_json::json!("auto")),
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "get_weather");
    assert_eq!(canonical.tools[0].strict, Some(true));
    assert!(matches!(canonical.tool_choice, Some(ToolChoice::Auto)));

    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.tools.as_ref().unwrap().len(), 1);
    assert_eq!(back.tools.as_ref().unwrap()[0].function.name, "get_weather");
    assert_eq!(back.tool_choice, Some(serde_json::json!("auto")));
}

#[test]
fn test_tool_calls_in_assistant_message() {
    let req = OpenAIChatRequest {
        messages: vec![OpenAIChatMessage {
            role: "assistant".into(),
            content: None,
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_abc".into(),
                r#type: "function".into(),
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: r#"{"location":"Boston"}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 1);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolUse { id, name, input }
            if id == "call_abc" && name == "get_weather" && input["location"] == "Boston"
    ));

    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    let tc = back.messages[0].tool_calls.as_ref().unwrap();
    assert_eq!(tc[0].function.arguments, r#"{"location":"Boston"}"#);
}

#[test]
fn test_tool_result_message() {
    let req = OpenAIChatRequest {
        messages: vec![OpenAIChatMessage {
            role: "tool".into(),
            content: Some(MessageContent::Text("72°F and sunny".into())),
            name: None,
            tool_calls: None,
            tool_call_id: Some("call_abc".into()),
            reasoning_content: None,
        }],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolResult { tool_use_id, .. }
            if tool_use_id == "call_abc"
    ));

    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.messages[0].role, "tool");
    assert_eq!(back.messages[0].tool_call_id.as_deref(), Some("call_abc"));
}

#[test]
fn test_parameter_mapping() {
    let req = OpenAIChatRequest {
        temperature: Some(0.7),
        top_p: Some(0.9),
        max_completion_tokens: Some(1024),
        stop: Some(StopValue::Multiple(vec!["END".into(), "STOP".into()])),
        seed: Some(42),
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.params.temperature, Some(0.7));
    assert_eq!(canonical.params.top_p, Some(0.9));
    assert_eq!(canonical.params.max_output_tokens, Some(1024));
    assert_eq!(canonical.params.stop_sequences, vec!["END", "STOP"]);
    assert_eq!(canonical.params.seed, Some(42));

    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.max_completion_tokens, Some(1024));
    assert!(back.max_tokens.is_none());
    assert!(matches!(back.stop, Some(StopValue::Multiple(_))));
}

#[test]
fn test_max_tokens_fallback() {
    let req = OpenAIChatRequest {
        max_tokens: Some(512),
        max_completion_tokens: None,
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.params.max_output_tokens, Some(512));
}

#[test]
fn test_image_content() {
    let req = OpenAIChatRequest {
        messages: vec![OpenAIChatMessage {
            role: "user".into(),
            content: Some(MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What's in this image?".into(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrlDetail {
                        url: "https://example.com/img.png".into(),
                        detail: Some("high".into()),
                    },
                },
            ])),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        ..sample_request()
    };
    let canonical = OpenAIChatAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns[0].content.len(), 2);
    assert!(matches!(
        &canonical.turns[0].content[1],
        ContentBlock::Image { source: ImageSource::Url { url, detail } }
            if url == "https://example.com/img.png" && detail == &Some("high".into())
    ));

    let back = OpenAIChatAdapter::request_from_canonical(&canonical).unwrap();
    assert!(matches!(
        back.messages[0].content,
        Some(MessageContent::Parts(_))
    ));
}

#[test]
fn test_response_content_only() {
    let resp = OpenAIChatResponse {
        id: "chatcmpl-123".into(),
        object: "chat.completion".into(),
        created: 1234567890,
        model: "gpt-4o".into(),
        choices: vec![Choice {
            index: 0,
            message: ChoiceMessage {
                role: "assistant".into(),
                content: Some("Hello!".into()),
                tool_calls: None,
                refusal: None,
            },
            finish_reason: Some("stop".into()),
        }],
        usage: Some(OpenAIUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
        system_fingerprint: None,
    };
    let canonical = OpenAIChatAdapter::response_to_canonical(resp).unwrap();
    assert_eq!(canonical.content.len(), 1);
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::Text { text } if text == "Hello!"
    ));
    assert_eq!(canonical.stop_reason, StopReason::EndTurn);
    assert_eq!(canonical.usage.input_tokens, 10);
    assert_eq!(canonical.usage.output_tokens, 5);
}

#[test]
fn test_response_with_tool_calls() {
    let resp = OpenAIChatResponse {
        id: "chatcmpl-456".into(),
        object: "chat.completion".into(),
        created: 1234567890,
        model: "gpt-4o".into(),
        choices: vec![Choice {
            index: 0,
            message: ChoiceMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_xyz".into(),
                    r#type: "function".into(),
                    function: FunctionCall {
                        name: "search".into(),
                        arguments: r#"{"query":"rust"}"#.into(),
                    },
                }]),
                refusal: None,
            },
            finish_reason: Some("tool_calls".into()),
        }],
        usage: None,
        system_fingerprint: None,
    };
    let canonical = OpenAIChatAdapter::response_to_canonical(resp).unwrap();
    assert_eq!(canonical.content.len(), 1);
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::ToolUse { name, .. } if name == "search"
    ));
    assert_eq!(canonical.stop_reason, StopReason::ToolUse);
}

#[test]
fn test_finish_reason_mapping() {
    let cases = [
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("tool_calls", StopReason::ToolUse),
        ("content_filter", StopReason::ContentFilter),
    ];
    for (reason, expected) in cases {
        let resp = OpenAIChatResponse {
            id: "id".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "gpt-4o".into(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".into(),
                    content: Some("x".into()),
                    tool_calls: None,
                    refusal: None,
                },
                finish_reason: Some(reason.into()),
            }],
            usage: None,
            system_fingerprint: None,
        };
        let canonical = OpenAIChatAdapter::response_to_canonical(resp).unwrap();
        assert_eq!(canonical.stop_reason, expected, "failed for {reason}");
    }
}

#[test]
fn test_response_from_canonical() {
    let canonical = CanonicalResponse {
        id: "resp_1".into(),
        model: "gpt-4o".into(),
        content: vec![
            ContentBlock::Text {
                text: "Sure!".into(),
            },
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "calc".into(),
                input: serde_json::json!({"a": 1}),
            },
        ],
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 20,
            output_tokens: 10,
            ..Default::default()
        },
    };
    let resp = OpenAIChatAdapter::response_from_canonical(&canonical).unwrap();
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("Sure!"));
    assert_eq!(resp.choices[0].message.tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 20);
}

#[test]
fn test_usage_mapping() {
    let resp = OpenAIChatResponse {
        id: "id".into(),
        object: "chat.completion".into(),
        created: 0,
        model: "gpt-4o".into(),
        choices: vec![Choice {
            index: 0,
            message: ChoiceMessage {
                role: "assistant".into(),
                content: Some("ok".into()),
                tool_calls: None,
                refusal: None,
            },
            finish_reason: Some("stop".into()),
        }],
        usage: Some(OpenAIUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        }),
        system_fingerprint: None,
    };
    let canonical = OpenAIChatAdapter::response_to_canonical(resp).unwrap();
    assert_eq!(canonical.usage.input_tokens, 100);
    assert_eq!(canonical.usage.output_tokens, 50);
}
