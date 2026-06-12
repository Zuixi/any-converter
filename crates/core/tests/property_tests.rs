#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use any_converter_core::convert::{Format, convert_request, convert_response};
use any_converter_core::ir::{StopReason, Usage};
use any_converter_core::sse::parse_sse_block;
use common::{all_formats, extract_model, minimal_request_json, minimal_response_json};
use proptest::prelude::*;

fn arb_format() -> impl Strategy<Value = Format> {
    prop_oneof![
        Just(Format::OpenAIChat),
        Just(Format::Claude),
        Just(Format::OpenAIResponses),
        Just(Format::Gemini),
    ]
}

fn arb_stop_reason_openai() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        Just(StopReason::EndTurn),
        Just(StopReason::MaxTokens),
        Just(StopReason::ToolUse),
        Just(StopReason::ContentFilter),
    ]
}

fn arb_stop_reason_claude() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        Just(StopReason::EndTurn),
        Just(StopReason::MaxTokens),
        Just(StopReason::ToolUse),
        Just(StopReason::StopSequence),
    ]
}

// ── Identity conversion ───────────────────────────────────────────────

proptest! {
    #[test]
    fn identity_request_is_noop(fmt in arb_format()) {
        let req = minimal_request_json("test-model", "hello", fmt);
        let bytes = serde_json::to_vec(&req).unwrap();
        let out = convert_request(&bytes, fmt, fmt).unwrap();
        prop_assert_eq!(&bytes, &out, "identity conversion must return identical bytes");
    }

    #[test]
    fn identity_response_is_noop(fmt in arb_format()) {
        let resp = minimal_response_json("test-model", "ok", fmt);
        let bytes = serde_json::to_vec(&resp).unwrap();
        let out = convert_response(&bytes, fmt, fmt).unwrap();
        prop_assert_eq!(&bytes, &out, "identity response must return identical bytes");
    }
}

// ── Conversion always produces valid JSON ─────────────────────────────

proptest! {
    #[test]
    fn request_conversion_always_produces_valid_json(
        from in arb_format(),
        to in arb_format(),
        model in "[a-z][a-z0-9_-]{2,20}",
        text in "[a-zA-Z0-9 ,.!?]{1,50}",
    ) {
        let req = minimal_request_json(&model, &text, from);
        let bytes = serde_json::to_vec(&req).unwrap();
        if let Ok(out) = convert_request(&bytes, from, to) {
            let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&out);
            prop_assert!(parsed.is_ok(), "output must be valid JSON, got: {:?}", String::from_utf8_lossy(&out[..out.len().min(200)]));
        }
    }

    #[test]
    fn response_conversion_always_produces_valid_json(
        from in arb_format(),
        to in arb_format(),
        model in "[a-z][a-z0-9_-]{2,20}",
        text in "[a-zA-Z0-9 ,.!?]{1,50}",
    ) {
        let resp = minimal_response_json(&model, &text, from);
        let bytes = serde_json::to_vec(&resp).unwrap();
        if let Ok(out) = convert_response(&bytes, from, to) {
            let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&out);
            prop_assert!(parsed.is_ok(), "output must be valid JSON");
        }
    }
}

// ── Model preservation ────────────────────────────────────────────────

proptest! {
    #[test]
    fn model_preserved_in_request_conversion(
        from in arb_format(),
        to in arb_format(),
        model in "[a-z][a-z0-9_-]{2,20}",
    ) {
        prop_assume!(from != to);
        let req = minimal_request_json(&model, "hello", from);
        let bytes = serde_json::to_vec(&req).unwrap();
        if let Ok(out) = convert_request(&bytes, from, to) {
            let output: serde_json::Value = serde_json::from_slice(&out).unwrap();
            if let Some(out_model) = extract_model(&output, to) {
                prop_assert_eq!(out_model, model, "model must be preserved from {:?} to {:?}", from, to);
            }
        }
    }

    #[test]
    fn model_preserved_in_response_conversion(
        from in arb_format(),
        to in arb_format(),
        model in "[a-z][a-z0-9_-]{2,20}",
    ) {
        prop_assume!(from != to);
        let resp = minimal_response_json(&model, "ok", from);
        let bytes = serde_json::to_vec(&resp).unwrap();
        if let Ok(out) = convert_response(&bytes, from, to) {
            let output: serde_json::Value = serde_json::from_slice(&out).unwrap();
            let out_model = match to {
                Format::Gemini => output.get("modelVersion")
                    .or_else(|| output.get("model_version"))
                    .or_else(|| output.get("model"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                _ => output.get("model").and_then(|v| v.as_str()).map(String::from),
            };
            if let Some(m) = out_model {
                prop_assert_eq!(m, model, "response model must be preserved from {:?} to {:?}", from, to);
            }
        }
    }
}

// ── StopReason roundtrip ──────────────────────────────────────────────

proptest! {
    #[test]
    fn stop_reason_openai_chat_roundtrip(r in arb_stop_reason_openai()) {
        let s = r.to_openai_chat();
        let back = StopReason::from_openai_chat(s);
        prop_assert_eq!(&r, &back, "OpenAI Chat roundtrip failed for {}", s);
    }

    #[test]
    fn stop_reason_claude_roundtrip(r in arb_stop_reason_claude()) {
        let s = r.to_claude();
        let back = StopReason::from_claude(s);
        prop_assert_eq!(&r, &back, "Claude roundtrip failed for {}", s);
    }
}

// ── Usage total_tokens consistency ────────────────────────────────────

proptest! {
    #[test]
    fn usage_total_tokens_is_sum(
        input in 0u64..1_000_000,
        output in 0u64..1_000_000,
        cache_read in proptest::option::of(0u64..100_000),
        reasoning in proptest::option::of(0u64..100_000),
    ) {
        let u = Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: None,
            reasoning_tokens: reasoning,
        };
        prop_assert_eq!(u.total_tokens(), input + output);
    }
}

// ── SSE parse never panics ────────────────────────────────────────────

proptest! {
    #[test]
    fn sse_parse_no_panic(data in ".*") {
        let _ = parse_sse_block(&data);
    }

    #[test]
    fn sse_parse_with_prefix(data in "(event: [a-z_]+\n)?data: .*") {
        let _ = parse_sse_block(&data);
    }
}

// ── Format parse / display roundtrip ──────────────────────────────────

#[test]
fn format_display_parse_roundtrip_all() {
    for fmt in all_formats() {
        let s = fmt.as_str();
        let back = Format::parse(s).unwrap();
        assert_eq!(fmt, back, "format roundtrip failed for {s}");
    }
}

// ── Cross-format request conversion succeeds for all 12 pairs ────────

#[test]
fn all_12_pairs_convert_minimal_request() {
    let formats = all_formats();
    for &from in &formats {
        for &to in &formats {
            if from == to {
                continue;
            }
            let req = minimal_request_json("test-model", "hello world", from);
            let bytes = serde_json::to_vec(&req).unwrap();
            let result = convert_request(&bytes, from, to);
            assert!(
                result.is_ok(),
                "{from:?} -> {to:?} failed: {:?}",
                result.err()
            );
            let out = result.unwrap();
            let _: serde_json::Value = serde_json::from_slice(&out)
                .unwrap_or_else(|e| panic!("{from:?} -> {to:?} produced invalid JSON: {e}"));
        }
    }
}

// ── Cross-format response conversion succeeds for all 12 pairs ───────

#[test]
fn all_12_pairs_convert_minimal_response() {
    let formats = all_formats();
    for &from in &formats {
        for &to in &formats {
            if from == to {
                continue;
            }
            let resp = minimal_response_json("test-model", "response text", from);
            let bytes = serde_json::to_vec(&resp).unwrap();
            let result = convert_response(&bytes, from, to);
            assert!(
                result.is_ok(),
                "{from:?} -> {to:?} response failed: {:?}",
                result.err()
            );
        }
    }
}
