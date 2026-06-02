use super::*;

fn sample_request() -> GeminiRequest {
    GeminiRequest {
        contents: vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart {
                text: Some("Hello".into()),
                inline_data: None,
                function_call: None,
                function_response: None,
            }],
        }],
        system_instruction: None,
        generation_config: None,
        tools: None,
        tool_config: None,
    }
}

#[test]
fn test_simple_text_request_to_canonical() {
    let req = sample_request();
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert!(canonical.model.is_empty());
    assert_eq!(canonical.turns.len(), 1);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::Text { text } if text == "Hello"
    ));
}

#[test]
fn test_role_mapping_model_to_assistant() {
    let req = GeminiRequest {
        contents: vec![GeminiContent {
            role: Some("model".into()),
            parts: vec![GeminiPart {
                text: Some("Hi there".into()),
                inline_data: None,
                function_call: None,
                function_response: None,
            }],
        }],
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns[0].role, Role::Assistant);

    let back = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.contents[0].role.as_deref(), Some("model"));
}

#[test]
fn test_system_instruction_extraction() {
    let req = GeminiRequest {
        system_instruction: Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart {
                text: Some("You are helpful.".into()),
                inline_data: None,
                function_call: None,
                function_response: None,
            }],
        }),
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(
        canonical.system.unwrap().as_text(),
        "You are helpful."
    );
}

#[test]
fn test_system_instruction_injection() {
    let canonical = CanonicalRequest {
        model: "gemini-2.0-flash".into(),
        system: Some(SystemContent::Text("Be concise.".into())),
        ..CanonicalRequest::simple("gemini-2.0-flash", "Hello")
    };
    let req = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(
        req.system_instruction
            .as_ref()
            .unwrap()
            .parts[0]
            .text
            .as_deref(),
        Some("Be concise.")
    );
}

#[test]
fn test_multi_turn_conversation() {
    let req = GeminiRequest {
        contents: vec![
            GeminiContent {
                role: Some("user".into()),
                parts: vec![GeminiPart {
                    text: Some("Hi".into()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            },
            GeminiContent {
                role: Some("model".into()),
                parts: vec![GeminiPart {
                    text: Some("Hello!".into()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            },
            GeminiContent {
                role: Some("user".into()),
                parts: vec![GeminiPart {
                    text: Some("How are you?".into()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            },
        ],
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns.len(), 3);
    assert_eq!(canonical.turns[0].role, Role::User);
    assert_eq!(canonical.turns[1].role, Role::Assistant);
    assert_eq!(canonical.turns[2].role, Role::User);
}

#[test]
fn test_tool_definitions_mapping() {
    let req = GeminiRequest {
        tools: Some(vec![GeminiToolDeclaration {
            function_declarations: vec![GeminiFunctionDeclaration {
                name: "get_weather".into(),
                description: Some("Get weather".into()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": { "location": { "type": "string" } }
                })),
            }],
        }]),
        tool_config: Some(serde_json::json!({
            "functionCallingConfig": { "mode": "AUTO" }
        })),
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.tools.len(), 1);
    assert_eq!(canonical.tools[0].name, "get_weather");
    assert!(matches!(canonical.tool_choice, Some(ToolChoice::Auto)));

    let back = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    assert_eq!(back.tools.as_ref().unwrap().len(), 1);
    assert_eq!(
        back.tools.as_ref().unwrap()[0].function_declarations[0].name,
        "get_weather"
    );
}

#[test]
fn test_function_call_in_response_parts() {
    let resp = GeminiResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".into()),
                parts: vec![GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: Some(GeminiFunctionCall {
                        name: "get_weather".into(),
                        args: serde_json::json!({"location": "Boston"}),
                        id: Some("call_abc".into()),
                    }),
                    function_response: None,
                }],
            },
            finish_reason: Some("STOP".into()),
            index: Some(0),
        }],
        usage_metadata: None,
        model_version: Some("gemini-2.0-flash".into()),
    };
    let canonical = GeminiAdapter::response_to_canonical(resp).unwrap();
    assert!(matches!(
        &canonical.content[0],
        ContentBlock::ToolUse { id, name, input }
            if id == "call_abc" && name == "get_weather" && input["location"] == "Boston"
    ));
}

#[test]
fn test_function_response_in_user_parts() {
    let req = GeminiRequest {
        contents: vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart {
                text: None,
                inline_data: None,
                function_call: None,
                function_response: Some(GeminiFunctionResponse {
                    name: "get_weather".into(),
                    response: serde_json::json!({"result": "72°F and sunny"}),
                    id: Some("call_abc".into()),
                }),
            }],
        }],
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert!(matches!(
        &canonical.turns[0].content[0],
        ContentBlock::ToolResult { tool_use_id, .. }
            if tool_use_id == "call_abc"
    ));

    let back = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    let fr = back.contents[0].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr.id.as_deref(), Some("call_abc"));
    assert_eq!(fr.response["result"], "72°F and sunny");
}

#[test]
fn test_image_inline_data_mapping() {
    let req = GeminiRequest {
        contents: vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![
                GeminiPart {
                    text: Some("What's this?".into()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                },
                GeminiPart {
                    text: None,
                    inline_data: Some(GeminiInlineData {
                        mime_type: "image/png".into(),
                        data: "base64data".into(),
                    }),
                    function_call: None,
                    function_response: None,
                },
            ],
        }],
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.turns[0].content.len(), 2);
    assert!(matches!(
        &canonical.turns[0].content[1],
        ContentBlock::Image { source: ImageSource::Base64 { media_type, data } }
            if media_type == "image/png" && data == "base64data"
    ));

    let back = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    assert!(back.contents[0].parts[1].inline_data.is_some());
}

#[test]
fn test_generation_config_parameter_mapping() {
    let req = GeminiRequest {
        generation_config: Some(GeminiGenerationConfig {
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
            max_output_tokens: Some(1024),
            stop_sequences: Some(vec!["END".into()]),
            seed: Some(42),
            response_mime_type: None,
            response_schema: None,
        }),
        ..sample_request()
    };
    let canonical = GeminiAdapter::request_to_canonical(req).unwrap();
    assert_eq!(canonical.params.temperature, Some(0.7));
    assert_eq!(canonical.params.top_p, Some(0.9));
    assert_eq!(canonical.params.top_k, Some(40));
    assert_eq!(canonical.params.max_output_tokens, Some(1024));
    assert_eq!(canonical.params.stop_sequences, vec!["END"]);
    assert_eq!(canonical.params.seed, Some(42));

    let back = GeminiAdapter::request_from_canonical(&canonical).unwrap();
    let cfg = back.generation_config.unwrap();
    assert_eq!(cfg.temperature, Some(0.7));
    assert_eq!(cfg.max_output_tokens, Some(1024));
}

#[test]
fn test_finish_reason_mapping() {
    let cases = [
        ("STOP", StopReason::EndTurn),
        ("MAX_TOKENS", StopReason::MaxTokens),
        ("SAFETY", StopReason::ContentFilter),
    ];
    for (reason, expected) in cases {
        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".into()),
                    parts: vec![GeminiPart {
                        text: Some("x".into()),
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                    }],
                },
                finish_reason: Some(reason.into()),
                index: Some(0),
            }],
            usage_metadata: None,
            model_version: None,
        };
        let canonical = GeminiAdapter::response_to_canonical(resp).unwrap();
        assert_eq!(canonical.stop_reason, expected, "failed for {reason}");
    }
}

#[test]
fn test_usage_metadata_mapping() {
    let resp = GeminiResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".into()),
                parts: vec![GeminiPart {
                    text: Some("ok".into()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                }],
            },
            finish_reason: Some("STOP".into()),
            index: Some(0),
        }],
        usage_metadata: Some(GeminiUsageMetadata {
            prompt_token_count: Some(100),
            candidates_token_count: Some(50),
            total_token_count: Some(150),
        }),
        model_version: None,
    };
    let canonical = GeminiAdapter::response_to_canonical(resp).unwrap();
    assert_eq!(canonical.usage.input_tokens, 100);
    assert_eq!(canonical.usage.output_tokens, 50);
}

#[test]
fn test_request_roundtrip_canonical_to_gemini_to_canonical() {
    let original = CanonicalRequest {
        model: "gemini-2.0-flash".into(),
        system: Some(SystemContent::Text("You are helpful.".into())),
        turns: vec![
            Turn {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "Hi".into() }],
            },
            Turn {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Hello!".into(),
                }],
            },
        ],
        tools: vec![ToolDef {
            name: "search".into(),
            description: Some("Search the web".into()),
            input_schema: serde_json::json!({"type": "object"}),
            strict: None,
        }],
        tool_choice: Some(ToolChoice::Auto),
        params: GenerationParams {
            temperature: Some(0.5),
            max_output_tokens: Some(256),
            ..Default::default()
        },
        stream: false,
        extra: serde_json::Value::Null,
    };

    let gemini = GeminiAdapter::request_from_canonical(&original).unwrap();
    let back = GeminiAdapter::request_to_canonical(gemini).unwrap();

    assert_eq!(back.system.unwrap().as_text(), "You are helpful.");
    assert_eq!(back.turns.len(), 2);
    assert_eq!(back.turns[0].role, Role::User);
    assert_eq!(back.turns[1].role, Role::Assistant);
    assert_eq!(back.tools[0].name, "search");
    assert!(matches!(back.tool_choice, Some(ToolChoice::Auto)));
    assert_eq!(back.params.temperature, Some(0.5));
    assert_eq!(back.params.max_output_tokens, Some(256));
}
