#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{parse_sse_block, split_sse_blocks};
use common::all_formats;
use proptest::prelude::*;

fn arb_format() -> impl Strategy<Value = Format> {
    prop_oneof![
        Just(Format::OpenAIChat),
        Just(Format::Claude),
        Just(Format::OpenAIResponses),
        Just(Format::Gemini),
    ]
}

// ── Random bytes must not panic, must return Err ──────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn random_bytes_request_no_panic(
        data in prop::collection::vec(any::<u8>(), 0..1024),
        from in arb_format(),
        to in arb_format(),
    ) {
        prop_assume!(from != to);
        let result = convert_request(&data, from, to);
        prop_assert!(result.is_err(), "random bytes should not produce Ok result");
    }

    #[test]
    fn random_bytes_response_no_panic(
        data in prop::collection::vec(any::<u8>(), 0..1024),
        from in arb_format(),
        to in arb_format(),
    ) {
        prop_assume!(from != to);
        let result = convert_response(&data, from, to);
        prop_assert!(result.is_err(), "random bytes should not produce Ok response");
    }
}

// ── Random UTF-8 into SSE parser ──────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn random_utf8_sse_parse_no_panic(data in "\\PC{0,500}") {
        let _ = parse_sse_block(&data);
    }

    #[test]
    fn random_utf8_sse_split_no_panic(data in "\\PC{0,500}") {
        let _ = split_sse_blocks(&data);
    }
}

// ── Random JSON values into converters ────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn arbitrary_json_object_request_no_panic(
        model in "[a-z]{3,10}",
        extra_key in "[a-z_]{1,10}",
        extra_val in "[a-z0-9]{1,20}",
        from in arb_format(),
        to in arb_format(),
    ) {
        prop_assume!(from != to);
        let json = serde_json::json!({
            "model": model,
            extra_key: extra_val,
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let _ = convert_request(&bytes, from, to);
    }

    #[test]
    fn json_with_wrong_types_no_panic(
        from in arb_format(),
        to in arb_format(),
    ) {
        prop_assume!(from != to);
        let cases = vec![
            serde_json::json!({"model": 42, "messages": "not_array"}),
            serde_json::json!({"model": null, "messages": []}),
            serde_json::json!({"model": true, "messages": [{"role": 1, "content": []}]}),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(null),
            serde_json::json!("just a string"),
            serde_json::json!(42),
        ];
        for case in cases {
            let bytes = serde_json::to_vec(&case).unwrap();
            let _ = convert_request(&bytes, from, to);
        }
    }
}

// ── Malformed tool arguments ──────────────────────────────────────────

#[test]
fn malformed_tool_args_fallback_to_empty_object() {
    let bad_args_cases = vec![
        "{not valid json",
        "{{double braces}}",
        "",
        "undefined",
        "[1,2,3",
    ];

    for args in bad_args_cases {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_bad",
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "arguments": args
                    }
                }]
            }]
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let result = convert_request(&bytes, Format::OpenAIChat, Format::Claude);
        if let Ok(out) = result {
            let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
            if let Some(input) = json
                .get("messages")
                .and_then(|m| m.as_array())
                .and_then(|msgs| msgs.first())
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|blocks| blocks.first())
                .and_then(|b| b.get("input"))
            {
                assert!(
                    input.is_object(),
                    "malformed args '{args}' should produce object, got: {input}"
                );
            }
        }
    }
}

// ── Stream conversion with garbage SSE data ───────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn random_sse_data_stream_no_panic(
        data in "[a-zA-Z0-9 :{}\",.\\[\\]]{0,200}",
        from in arb_format(),
        to in arb_format(),
    ) {
        prop_assume!(from != to);
        let block = format!("data: {data}");
        if let Some(event) = parse_sse_block(&block) {
            let mut state_in = StreamState::new();
            let mut state_out = StreamState::new();
            let _ = convert_stream_event(&event, from, to, &mut state_in, &mut state_out);
        }
    }
}

// ── Empty and edge-case inputs ────────────────────────────────────────

#[test]
fn empty_json_object_all_pairs() {
    let formats = all_formats();
    for &from in &formats {
        for &to in &formats {
            if from == to {
                continue;
            }
            let result = convert_request(b"{}", from, to);
            assert!(
                result.is_err(),
                "{from:?} -> {to:?}: empty object should fail"
            );
        }
    }
}

#[test]
fn nested_null_fields_no_panic() {
    let formats = all_formats();
    let payload = serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": null}],
        "temperature": null,
        "max_tokens": null,
        "tools": null,
    });
    let bytes = serde_json::to_vec(&payload).unwrap();
    for &from in &formats {
        for &to in &formats {
            if from == to {
                continue;
            }
            let _ = convert_request(&bytes, from, to);
        }
    }
}

#[test]
fn unicode_content_preserved() {
    let formats = all_formats();
    let texts = vec![
        "你好世界",
        "こんにちは",
        "مرحبا",
        "Привет",
        "🎉🚀💻",
        "mixed English 中文 日本語",
    ];
    for text in texts {
        for &from in &formats {
            for &to in &formats {
                if from == to {
                    continue;
                }
                let req = common::minimal_request_json("test", text, from);
                let bytes = serde_json::to_vec(&req).unwrap();
                if let Ok(out) = convert_request(&bytes, from, to) {
                    let out_str = String::from_utf8_lossy(&out);
                    assert!(
                        out_str.contains(text),
                        "{from:?} -> {to:?}: unicode text '{text}' not preserved in output"
                    );
                }
            }
        }
    }
}
