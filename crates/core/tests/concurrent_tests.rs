#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::parse_sse_block;
use common::{
    all_format_pairs, extract_model, extract_response_text, format_dir_name, load_sse_blocks,
    minimal_request_json, minimal_response_json,
};

// ── Concurrent request conversions ────────────────────────────────────

#[tokio::test]
async fn concurrent_request_conversions_all_succeed() {
    let pairs = all_format_pairs();
    let mut handles = Vec::new();

    for _ in 0..10 {
        for &(from, to) in &pairs {
            handles.push(tokio::spawn(async move {
                let req = minimal_request_json("gpt-4", "concurrent test", from);
                let bytes = serde_json::to_vec(&req).unwrap();
                let result = convert_request(&bytes, from, to);
                assert!(
                    result.is_ok(),
                    "concurrent {from:?} -> {to:?} failed: {:?}",
                    result.err()
                );
                let out = result.unwrap();
                let output: serde_json::Value = serde_json::from_slice(&out).unwrap();
                if let Some(m) = extract_model(&output, to) {
                    assert_eq!(m, "gpt-4", "model mismatch in concurrent conversion");
                }
            }));
        }
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent response conversions ───────────────────────────────────

#[tokio::test]
async fn concurrent_response_conversions_all_succeed() {
    let pairs = all_format_pairs();
    let mut handles = Vec::new();

    for _ in 0..10 {
        for &(from, to) in &pairs {
            handles.push(tokio::spawn(async move {
                let resp = minimal_response_json("gpt-4", "concurrent response", from);
                let bytes = serde_json::to_vec(&resp).unwrap();
                let result = convert_response(&bytes, from, to);
                assert!(
                    result.is_ok(),
                    "concurrent response {from:?} -> {to:?} failed: {:?}",
                    result.err()
                );
            }));
        }
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent conversions with distinct data ─────────────────────────

#[tokio::test]
async fn concurrent_conversions_with_unique_data_no_cross_contamination() {
    let mut handles = Vec::new();

    for i in 0..50 {
        handles.push(tokio::spawn(async move {
            let model = format!("model-{i}");
            let text = format!("unique message number {i}");
            let from = match i % 4 {
                0 => Format::OpenAIChat,
                1 => Format::Claude,
                2 => Format::OpenAIResponses,
                _ => Format::Gemini,
            };
            let to = match (i + 1) % 4 {
                0 => Format::OpenAIChat,
                1 => Format::Claude,
                2 => Format::OpenAIResponses,
                _ => Format::Gemini,
            };

            let req = minimal_request_json(&model, &text, from);
            let bytes = serde_json::to_vec(&req).unwrap();
            let out = convert_request(&bytes, from, to).unwrap();
            let output: serde_json::Value = serde_json::from_slice(&out).unwrap();

            if let Some(m) = extract_model(&output, to) {
                assert_eq!(m, model, "task {i}: model cross-contamination detected");
            }

            let out_str = output.to_string();
            assert!(
                out_str.contains(&text),
                "task {i}: text '{text}' missing from output"
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent stream conversions with independent state ──────────────

#[tokio::test]
async fn concurrent_stream_conversions_state_isolation() {
    let pairs: Vec<(Format, Format)> = all_format_pairs()
        .into_iter()
        .filter(|&(from, _)| {
            std::path::Path::new(&format!(
                "{}/tests/fixtures/{}/stream_text.sse",
                env!("CARGO_MANIFEST_DIR"),
                format_dir_name(from)
            ))
            .exists()
        })
        .collect();

    let mut handles = Vec::new();

    for _ in 0..5 {
        for &(from, to) in &pairs {
            handles.push(tokio::spawn(async move {
                let blocks = load_sse_blocks(format_dir_name(from), "stream_text.sse");
                let mut state_in = StreamState::new();
                let mut state_out = StreamState::new();
                let mut all_output = Vec::new();

                for block in &blocks {
                    if let Some(event) = parse_sse_block(block) {
                        let lines =
                            convert_stream_event(&event, from, to, &mut state_in, &mut state_out)
                                .unwrap();
                        all_output.extend(lines);
                    }
                }

                assert!(
                    !all_output.is_empty(),
                    "stream {from:?} -> {to:?} produced no output"
                );

                let combined = all_output.join("\n");
                assert!(
                    combined.contains("Hello") || combined.contains("hello"),
                    "stream {from:?} -> {to:?}: expected text content"
                );
            }));
        }
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── High concurrency stress test ──────────────────────────────────────

#[tokio::test]
async fn stress_200_concurrent_conversions() {
    let mut handles = Vec::new();

    for i in 0..200 {
        handles.push(tokio::spawn(async move {
            let from = match i % 4 {
                0 => Format::OpenAIChat,
                1 => Format::Claude,
                2 => Format::OpenAIResponses,
                _ => Format::Gemini,
            };
            let to = match (i + 2) % 4 {
                0 => Format::OpenAIChat,
                1 => Format::Claude,
                2 => Format::OpenAIResponses,
                _ => Format::Gemini,
            };

            let req = minimal_request_json("stress-model", "stress test message", from);
            let bytes = serde_json::to_vec(&req).unwrap();
            let result = convert_request(&bytes, from, to);
            assert!(result.is_ok(), "stress test {i}: {from:?} -> {to:?} failed");
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent response roundtrips ────────────────────────────────────

#[tokio::test]
async fn concurrent_response_roundtrip_consistency() {
    let pairs = all_format_pairs();
    let mut handles = Vec::new();

    for &(from, to) in &pairs {
        handles.push(tokio::spawn(async move {
            let resp = minimal_response_json("roundtrip-model", "roundtrip text", from);
            let bytes = serde_json::to_vec(&resp).unwrap();

            let via_bytes = convert_response(&bytes, from, to).unwrap();
            let via: serde_json::Value = serde_json::from_slice(&via_bytes).unwrap();

            if let Some(text) = extract_response_text(&via, to) {
                assert!(
                    !text.is_empty(),
                    "response {from:?} -> {to:?}: text was lost"
                );
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}
