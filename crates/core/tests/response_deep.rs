#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use any_converter_core::convert::{Format, convert_response};
use common::{
    all_format_pairs, extract_finish_reason, extract_response_text, extract_response_tool_names,
    extract_usage_input_tokens, extract_usage_output_tokens,
};
use serde_json::{Value, json};

fn convert(value: &Value, from: Format, to: Format) -> Option<Value> {
    let bytes = convert_response(value.to_string().as_bytes(), from, to).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ── stop reason end_turn — per-pair assertion ─────────────────────────

#[test]
fn stop_reason_end_turn_per_pair() {
    let openai_resp = json!({"id":"a","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
    let claude_resp = json!({"id":"a","type":"message","role":"assistant","model":"claude-3","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}});

    for &(from, to) in &all_format_pairs() {
        let source = match from {
            Format::OpenAIChat => &openai_resp,
            Format::Claude => &claude_resp,
            _ => continue,
        };
        let output = convert(source, from, to);
        assert!(
            output.is_some(),
            "end_turn {from:?} -> {to:?}: conversion failed"
        );
        let output = output.unwrap();
        let reason = extract_finish_reason(&output, to);
        assert!(
            reason.is_some(),
            "end_turn {from:?} -> {to:?}: finish_reason missing"
        );
        let r = reason.unwrap();
        match to {
            Format::OpenAIChat => assert_eq!(r, "stop", "{from:?} -> OpenAI Chat"),
            Format::Claude => assert_eq!(r, "end_turn", "{from:?} -> Claude"),
            Format::Gemini => assert_eq!(r, "STOP", "{from:?} -> Gemini"),
            Format::OpenAIResponses => assert_eq!(r, "completed", "{from:?} -> Responses"),
        }
    }
}

// ── stop reason max_tokens — per-pair assertion ───────────────────────

#[test]
fn stop_reason_max_tokens_per_pair() {
    let openai_resp = json!({"id":"a","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"length"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
    let claude_resp = json!({"id":"a","type":"message","role":"assistant","model":"claude-3","content":[{"type":"text","text":"ok"}],"stop_reason":"max_tokens","usage":{"input_tokens":1,"output_tokens":1}});

    for &(from, to) in &all_format_pairs() {
        let source = match from {
            Format::OpenAIChat => &openai_resp,
            Format::Claude => &claude_resp,
            _ => continue,
        };
        let output = convert(source, from, to);
        assert!(
            output.is_some(),
            "max_tokens {from:?} -> {to:?}: conversion failed"
        );
        let output = output.unwrap();
        let reason = extract_finish_reason(&output, to);
        assert!(
            reason.is_some(),
            "max_tokens {from:?} -> {to:?}: finish_reason missing"
        );
        let r = reason.unwrap();
        match to {
            Format::OpenAIChat => assert_eq!(r, "length", "{from:?} -> OpenAI Chat"),
            Format::Claude => assert_eq!(r, "max_tokens", "{from:?} -> Claude"),
            Format::Gemini => assert_eq!(r, "MAX_TOKENS", "{from:?} -> Gemini"),
            Format::OpenAIResponses => {
                assert!(
                    r == "completed" || r == "incomplete",
                    "{from:?} -> Responses: unexpected status '{r}'"
                );
            }
        }
    }
}

// ── usage token counts — precise per-pair ─────────────────────────────

#[test]
fn usage_token_counts_precise_per_pair() {
    let openai_resp = json!({"id":"a","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
    let claude_resp = json!({"id":"a","type":"message","role":"assistant","model":"claude-3","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}});

    for &(from, to) in &all_format_pairs() {
        let source = match from {
            Format::OpenAIChat => &openai_resp,
            Format::Claude => &claude_resp,
            _ => continue,
        };
        let output = convert(source, from, to);
        assert!(
            output.is_some(),
            "usage {from:?} -> {to:?}: conversion failed"
        );
        let output = output.unwrap();

        let input_tokens = extract_usage_input_tokens(&output, to);
        let output_tokens = extract_usage_output_tokens(&output, to);

        assert!(
            input_tokens.is_some(),
            "usage {from:?} -> {to:?}: input_tokens missing"
        );
        assert!(
            output_tokens.is_some(),
            "usage {from:?} -> {to:?}: output_tokens missing"
        );
        assert_eq!(
            input_tokens.unwrap(),
            10,
            "usage {from:?} -> {to:?}: input_tokens mismatch"
        );
        assert_eq!(
            output_tokens.unwrap(),
            5,
            "usage {from:?} -> {to:?}: output_tokens mismatch"
        );
    }
}

// ── claude cache tokens ────────────────────────────────────────────────

#[test]
fn usage_cache_tokens_claude_precise() {
    let claude_resp = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-3-opus",
        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2, "cache_creation_input_tokens": 3}
    });

    let chat_out = convert(&claude_resp, Format::Claude, Format::OpenAIChat).unwrap();
    assert_eq!(
        chat_out["usage"]["prompt_tokens"],
        json!(10),
        "prompt_tokens should be 10"
    );
    assert_eq!(
        chat_out["usage"]["completion_tokens"],
        json!(5),
        "completion_tokens should be 5"
    );

    let gemini_out = convert(&claude_resp, Format::Claude, Format::Gemini).unwrap();
    assert!(gemini_out["usageMetadata"].is_object());
    assert_eq!(
        gemini_out["usageMetadata"]["promptTokenCount"],
        json!(10),
        "Gemini promptTokenCount should be 10"
    );
}

// ── multi tool calls — precise counts and names ───────────────────────

#[test]
fn response_multi_tool_calls_precise() {
    let response = json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "message": {
            "role": "assistant",
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "weather", "arguments": "{\"city\":\"NYC\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "time", "arguments": "{}"}}
            ]
        }, "finish_reason": "tool_calls"}]
    });

    let claude_out = convert(&response, Format::OpenAIChat, Format::Claude).unwrap();
    let tool_names = extract_response_tool_names(&claude_out, Format::Claude);
    assert_eq!(tool_names.len(), 2, "expected 2 tool calls");
    assert!(
        tool_names.contains(&"weather".to_string()),
        "missing tool 'weather'"
    );
    assert!(
        tool_names.contains(&"time".to_string()),
        "missing tool 'time'"
    );

    let resp_out = convert(&response, Format::OpenAIChat, Format::OpenAIResponses).unwrap();
    let resp_tools = extract_response_tool_names(&resp_out, Format::OpenAIResponses);
    assert_eq!(
        resp_tools.len(),
        2,
        "Responses API: expected 2 function_call items"
    );
    assert!(resp_tools.contains(&"weather".to_string()));
    assert!(resp_tools.contains(&"time".to_string()));
}

// ── empty content ──────────────────────────────────────────────────────

#[test]
fn response_empty_content_structure() {
    let response = json!({
        "id": "a", "object": "chat.completion", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": ""}, "finish_reason": "stop"}]
    });
    let output = convert(&response, Format::OpenAIChat, Format::Claude).unwrap();
    assert!(output.is_object());
    assert_eq!(output["model"], "gpt-4", "model should be preserved");
    assert_eq!(
        output["stop_reason"], "end_turn",
        "stop_reason should map correctly"
    );
}

// ── mixed text + tool ──────────────────────────────────────────────────

#[test]
fn response_text_and_tool_mixed_precise() {
    let claude_resp = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-3",
        "content": [
            {"type": "text", "text": "Let me check the weather."},
            {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "NYC"}}
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "output_tokens": 15}
    });

    let chat_out = convert(&claude_resp, Format::Claude, Format::OpenAIChat).unwrap();
    let msg = &chat_out["choices"][0]["message"];
    assert_eq!(
        msg["content"], "Let me check the weather.",
        "text content should be exact"
    );
    let tool_calls = msg["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    assert_eq!(chat_out["choices"][0]["finish_reason"], "tool_calls");
}

// ── thinking/reasoning ─────────────────────────────────────────────────

#[test]
fn response_thinking_reasoning_content_precise() {
    let claude_resp = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-3",
        "content": [
            {"type": "thinking", "thinking": "Let me analyze...", "signature": "sig_abc"},
            {"type": "text", "text": "The answer is 42."}
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 20, "output_tokens": 10}
    });

    let chat_out = convert(&claude_resp, Format::Claude, Format::OpenAIChat).unwrap();
    let msg = &chat_out["choices"][0]["message"];
    assert_eq!(
        msg["reasoning_content"].as_str().unwrap(),
        "Let me analyze...",
        "reasoning_content should contain thinking text"
    );
    assert_eq!(
        msg["content"], "The answer is 42.",
        "text content should be exact"
    );
}

// ── stop reason Gemini mappings ───────────────────────────────────────

#[test]
fn stop_reason_gemini_mappings_precise() {
    let response = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}});

    let chat_out = convert(&response, Format::Gemini, Format::OpenAIChat).unwrap();
    assert_eq!(chat_out["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        extract_response_text(&chat_out, Format::OpenAIChat).unwrap(),
        "ok"
    );

    let claude_out = convert(&response, Format::Gemini, Format::Claude).unwrap();
    assert_eq!(claude_out["stop_reason"], "end_turn");
    assert_eq!(
        extract_response_text(&claude_out, Format::Claude).unwrap(),
        "ok"
    );
}

// ── response text preservation across all pairs ───────────────────────

#[test]
fn response_text_preserved_all_pairs() {
    let formats = [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ];
    for &from in &formats {
        let source = common::minimal_response_json("test-model", "Hello, world!", from);
        let bytes = serde_json::to_vec(&source).unwrap();
        for &to in &formats {
            if from == to {
                continue;
            }
            let result = convert_response(&bytes, from, to);
            assert!(
                result.is_ok(),
                "text preservation {from:?} -> {to:?}: conversion failed"
            );
            let output: Value = serde_json::from_slice(&result.unwrap()).unwrap();
            let text = extract_response_text(&output, to);
            assert!(
                text.is_some(),
                "text preservation {from:?} -> {to:?}: response text missing"
            );
            assert_eq!(
                text.unwrap(),
                "Hello, world!",
                "text preservation {from:?} -> {to:?}: text content mismatch"
            );
        }
    }
}

// ── response model preserved across all pairs ─────────────────────────

#[test]
fn response_model_preserved_all_pairs() {
    let formats = [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ];
    for &from in &formats {
        let source = common::minimal_response_json("test-model-xyz", "ok", from);
        let bytes = serde_json::to_vec(&source).unwrap();
        for &to in &formats {
            if from == to {
                continue;
            }
            let result = convert_response(&bytes, from, to);
            assert!(
                result.is_ok(),
                "model preservation {from:?} -> {to:?}: conversion failed"
            );
            let output: Value = serde_json::from_slice(&result.unwrap()).unwrap();
            let model = common::extract_response_model(&output, to);
            assert!(
                model.is_some(),
                "model preservation {from:?} -> {to:?}: model missing"
            );
            assert_eq!(
                model.unwrap(),
                "test-model-xyz",
                "model preservation {from:?} -> {to:?}: model mismatch"
            );
        }
    }
}
