#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use any_converter_core::convert::{Format, convert_request, convert_response};

use common::{
    assert_valid_json, extract_message_count, extract_model, extract_response_model,
    extract_response_text, extract_tool_names, format_dir_name, load_fixture,
    minimal_response_json,
};

const FORMAT_PAIRS: &[(Format, Format)] = &[
    (Format::OpenAIChat, Format::Claude),
    (Format::OpenAIChat, Format::OpenAIResponses),
    (Format::OpenAIChat, Format::Gemini),
    (Format::Claude, Format::OpenAIResponses),
    (Format::Claude, Format::Gemini),
    (Format::OpenAIResponses, Format::Gemini),
];

const REQUEST_FIXTURES: &[&str] = &[
    "request_simple.json",
    "request_tools.json",
    "request_multi_turn.json",
];

fn assert_request_roundtrip(from: Format, via: Format, fixture: &str) {
    let original_bytes = load_fixture(format_dir_name(from), fixture);
    let original = assert_valid_json(&original_bytes);
    let original_model = extract_model(&original, from);
    let original_count = extract_message_count(&original, from);
    let original_tools = extract_tool_names(&original, from);

    let via_bytes = convert_request(&original_bytes, from, via)
        .unwrap_or_else(|e| panic!("{from} -> {via} failed for {fixture}: {e}"));
    let via_json = assert_valid_json(&via_bytes);
    if let Some(ref model) = original_model {
        let via_model = extract_model(&via_json, via);
        assert_eq!(
            via_model.as_deref(),
            Some(model.as_str()),
            "model not preserved in forward {from} -> {via} ({fixture})"
        );
    }

    let roundtrip_bytes = convert_request(&via_bytes, via, from)
        .unwrap_or_else(|e| panic!("{via} -> {from} failed for {fixture}: {e}"));
    let roundtrip = assert_valid_json(&roundtrip_bytes);

    if let Some(model) = original_model {
        assert_eq!(
            extract_model(&roundtrip, from).as_deref(),
            Some(model.as_str()),
            "model not preserved in {from} roundtrip via {via} ({fixture})"
        );
    }

    let roundtrip_count = extract_message_count(&roundtrip, from);
    assert!(
        roundtrip_count >= original_count,
        "message count decreased in {from} roundtrip via {via} ({fixture}): \
         original={original_count}, roundtrip={roundtrip_count}"
    );

    if !original_tools.is_empty() {
        let roundtrip_tools = extract_tool_names(&roundtrip, from);
        let original_set: std::collections::HashSet<_> = original_tools.iter().collect();
        let roundtrip_set: std::collections::HashSet<_> = roundtrip_tools.iter().collect();
        assert_eq!(
            original_set, roundtrip_set,
            "tool names mismatch in {from} roundtrip via {via} ({fixture}): \
             original={original_tools:?}, roundtrip={roundtrip_tools:?}"
        );
    }
}

#[test]
fn request_roundtrip_consistency() {
    for &(a, b) in FORMAT_PAIRS {
        for fixture in REQUEST_FIXTURES {
            if std::path::Path::new(&format!(
                "{}/tests/fixtures/{}/{fixture}",
                env!("CARGO_MANIFEST_DIR"),
                format_dir_name(a)
            ))
            .exists()
                && std::path::Path::new(&format!(
                    "{}/tests/fixtures/{}/{fixture}",
                    env!("CARGO_MANIFEST_DIR"),
                    format_dir_name(b)
                ))
                .exists()
            {
                assert_request_roundtrip(a, b, fixture);
                assert_request_roundtrip(b, a, fixture);
            } else {
                assert_request_roundtrip(a, b, fixture);
            }
        }
    }
}

// ── Response roundtrip tests ──────────────────────────────────────────

fn assert_response_roundtrip(from: Format, via: Format, model: &str, text: &str) {
    let original = minimal_response_json(model, text, from);
    let original_bytes = serde_json::to_vec(&original).unwrap();

    let via_bytes = convert_response(&original_bytes, from, via)
        .unwrap_or_else(|e| panic!("response {from} -> {via} failed: {e}"));
    let via_json = assert_valid_json(&via_bytes);

    if let Some(m) = extract_response_model(&via_json, via) {
        assert_eq!(
            m, model,
            "response model not preserved in forward {from} -> {via}"
        );
    }

    let roundtrip_bytes = convert_response(&via_bytes, via, from)
        .unwrap_or_else(|e| panic!("response {via} -> {from} failed: {e}"));
    let roundtrip = assert_valid_json(&roundtrip_bytes);

    if let Some(m) = extract_response_model(&roundtrip, from) {
        assert_eq!(
            m, model,
            "response model not preserved in {from} roundtrip via {via}"
        );
    }

    let rt_text = extract_response_text(&roundtrip, from);
    assert!(
        rt_text.is_some(),
        "response text lost in {from} roundtrip via {via}"
    );
    assert_eq!(
        rt_text.unwrap(),
        text,
        "response text mismatch in {from} roundtrip via {via}"
    );
}

#[test]
fn response_roundtrip_consistency() {
    for &(a, b) in FORMAT_PAIRS {
        assert_response_roundtrip(a, b, "test-model", "Hello, world!");
        assert_response_roundtrip(b, a, "test-model", "Hello, world!");
    }
}

// ── Fixture-based response roundtrip ──────────────────────────────────

#[test]
fn response_roundtrip_from_fixtures() {
    let fixtures = ["response_simple.json", "response_tools.json"];
    for &(a, b) in FORMAT_PAIRS {
        for fixture in &fixtures {
            let path = format!(
                "{}/tests/fixtures/{}/{fixture}",
                env!("CARGO_MANIFEST_DIR"),
                format_dir_name(a)
            );
            if !std::path::Path::new(&path).exists() {
                continue;
            }
            let original_bytes = load_fixture(format_dir_name(a), fixture);
            let original = assert_valid_json(&original_bytes);
            let original_model = extract_response_model(&original, a);

            let via_bytes = convert_response(&original_bytes, a, b)
                .unwrap_or_else(|e| panic!("response {a} -> {b} ({fixture}): {e}"));
            assert_valid_json(&via_bytes);

            let roundtrip_bytes = convert_response(&via_bytes, b, a)
                .unwrap_or_else(|e| panic!("response {b} -> {a} ({fixture}): {e}"));
            let roundtrip = assert_valid_json(&roundtrip_bytes);

            if let Some(model) = original_model {
                if let Some(rt_model) = extract_response_model(&roundtrip, a) {
                    assert_eq!(
                        rt_model, model,
                        "response model mismatch in roundtrip {a} -> {b} -> {a} ({fixture})"
                    );
                }
            }
        }
    }
}
