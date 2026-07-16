#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fmt::Write as _;

use any_converter_core::convert::{Format, convert_request, convert_stream_event};
use any_converter_core::error::ConvertError;
use any_converter_core::ir::StreamState;
use any_converter_core::sse::parse_sse_block;

use common::{all_format_pairs, assert_valid_json};

fn assert_json_error(result: Result<Vec<u8>, ConvertError>) {
    match result {
        Err(ConvertError::Json(_)) => {}
        other => panic!("expected ConvertError::Json, got {other:?}"),
    }
}

#[test]
fn empty_body_returns_json_error_for_all_cross_format_pairs() {
    for (from, to) in all_format_pairs() {
        let result = convert_request(b"", from, to);
        assert_json_error(result);
    }
}

#[test]
fn invalid_json_returns_json_error_for_all_cross_format_pairs() {
    for (from, to) in all_format_pairs() {
        let result = convert_request(b"not json", from, to);
        assert_json_error(result);
    }
}

#[test]
fn missing_messages_field_returns_json_error() {
    let payload = br#"{"model":"gpt-4"}"#;
    let result = convert_request(payload, Format::OpenAIChat, Format::Claude);
    assert_json_error(result);
}

#[test]
fn identity_passthrough_returns_same_bytes() {
    let payload = br#"{"model":"claude-sonnet-4","max_tokens":1024,"messages":[{"role":"user","content":"hello"}]}"#;
    let result = convert_request(payload, Format::Claude, Format::Claude).unwrap();
    assert_eq!(result, payload);
}

#[test]
fn unknown_format_string_returns_invalid_field_error() {
    let result = Format::parse("xyz");
    match result {
        Err(ConvertError::InvalidField { field, reason }) => {
            assert_eq!(field, "format");
            assert_eq!(reason, "unknown format: xyz");
        }
        other => panic!("expected InvalidField error, got {other:?}"),
    }
}

#[test]
fn malformed_sse_block_parses_raw_data_without_json_validation() {
    let block = "data: {broken json";
    let event = parse_sse_block(block).expect("malformed JSON in data should still parse as SSE");
    assert_eq!(event.event, None);
    assert_eq!(event.data, "{broken json");
}

#[test]
fn empty_sse_data_parses_to_empty_string_payload() {
    let event = parse_sse_block("data: ").expect("empty data line should still produce an event");
    assert_eq!(event.event, None);
    assert_eq!(event.data, "");
}

#[test]
fn tool_args_not_valid_json_converts_gracefully_to_empty_object() {
    let payload = br#"{
        "model": "gpt-4",
        "messages": [{
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_bad",
                "type": "function",
                "function": {
                    "name": "lookup",
                    "arguments": "{not valid json"
                }
            }]
        }]
    }"#;

    let result = convert_request(payload, Format::OpenAIChat, Format::Claude).unwrap();
    let json = assert_valid_json(&result);
    let input = &json["messages"][0]["content"][0]["input"];
    assert_eq!(input, &serde_json::json!({}));
}

#[test]
fn empty_messages_array_converts_without_panic() {
    let payload = br#"{"model":"gpt-4","messages":[]}"#;
    let result = convert_request(payload, Format::OpenAIChat, Format::Claude);
    assert!(
        result.is_ok(),
        "empty messages should convert: {:?}",
        result.err()
    );
    let json = assert_valid_json(&result.unwrap());
    assert!(json["messages"].as_array().unwrap().is_empty());
}

#[test]
fn null_message_content_converts_successfully() {
    let payload = br#"{
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": null},
            {"role": "assistant", "content": null}
        ]
    }"#;

    let result = convert_request(payload, Format::OpenAIChat, Format::Claude).unwrap();
    let json = assert_valid_json(&result);
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
}

#[test]
fn very_large_payload_converts_successfully() {
    let mut large_text = String::with_capacity(100 * 1024);
    large_text.push('x');
    for _ in 1..(100 * 1024) {
        large_text.push('a');
    }

    let payload =
        format!(r#"{{"model":"gpt-4","messages":[{{"role":"user","content":"{large_text}"}}]}}"#);

    let result = convert_request(payload.as_bytes(), Format::OpenAIChat, Format::Gemini);
    assert!(
        result.is_ok(),
        "100KB payload should convert without panic: {:?}",
        result.err()
    );
    let json = assert_valid_json(&result.unwrap());
    assert!(json["contents"].is_array());
}

#[test]
fn stream_identity_passthrough_preserves_sse_formatting() {
    let block = "event: content_block_delta\ndata: {\"type\":\"text_delta\",\"text\":\"hi\"}";
    let event = parse_sse_block(block).unwrap();

    let mut state_in = StreamState::new();
    let mut state_out = StreamState::new();
    let lines = convert_stream_event(
        &event,
        Format::Claude,
        Format::Claude,
        &mut state_in,
        &mut state_out,
    )
    .unwrap();

    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("event: content_block_delta\n"));
    assert!(lines[0].contains("data: {\"type\":\"text_delta\",\"text\":\"hi\"}"));
    assert!(lines[0].ends_with("\n\n"));
}

#[test]
fn unsupported_conversion_error_message_format() {
    let err = ConvertError::UnsupportedConversion {
        from: "openai_chat".into(),
        to: "unknown_target".into(),
    };
    assert_eq!(
        err.to_string(),
        "unsupported format conversion: openai_chat -> unknown_target"
    );
}

#[test]
fn format_parse_accepts_all_valid_aliases() {
    let cases = [
        ("openai_chat", Format::OpenAIChat),
        ("openai", Format::OpenAIChat),
        ("claude", Format::Claude),
        ("anthropic", Format::Claude),
        ("openai_responses", Format::OpenAIResponses),
        ("responses", Format::OpenAIResponses),
        ("gemini", Format::Gemini),
        ("google", Format::Gemini),
    ];

    for (alias, expected) in cases {
        assert_eq!(
            Format::parse(alias).unwrap(),
            expected,
            "alias {alias:?} should map to {expected:?}"
        );
    }
}

#[test]
fn format_display_matches_canonical_names() {
    let cases = [
        (Format::OpenAIChat, "openai_chat"),
        (Format::Claude, "claude"),
        (Format::OpenAIResponses, "openai_responses"),
        (Format::Gemini, "gemini"),
    ];

    for (format, expected) in cases {
        assert_eq!(format.to_string(), expected);
        let mut buf = String::new();
        write!(&mut buf, "{format}").unwrap();
        assert_eq!(buf, expected);
    }
}
