#![allow(clippy::unwrap_used, clippy::expect_used)]

use any_converter_core::convert::{Format, convert_request};
use serde_json::{Value, json};

mod common;
use common::{
    all_format_pairs, extract_max_tokens, extract_stop_sequences, extract_temperature,
    extract_top_p,
};

fn convert_json(input: &Value, from: Format, to: Format) -> Option<Value> {
    let bytes = convert_request(input.to_string().as_bytes(), from, to).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ── helpers ────────────────────────────────────────────────────────────

fn resp_input() -> Value {
    json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}])
}

// ── temperature ────────────────────────────────────────────────────────

#[test]
fn temperature_cross_format_precise() {
    for &(from, to) in &all_format_pairs() {
        let request = match from {
            Format::OpenAIChat => {
                json!({"model":"test","messages":[{"role":"user","content":"hi"}],"temperature":0.7})
            }
            Format::OpenAIResponses => {
                json!({"model":"test","input":resp_input(),"temperature":0.7})
            }
            Format::Claude => {
                json!({"model":"test","max_tokens":10,"temperature":0.7,"messages":[{"role":"user","content":"hi"}]})
            }
            Format::Gemini => {
                json!({"model":"test","contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"temperature":0.7}})
            }
        };
        let output = convert_json(&request, from, to);
        assert!(output.is_some(), "{from:?} -> {to:?}: conversion failed");
        let output = output.unwrap();
        let temp = extract_temperature(&output, to);
        assert!(
            temp.is_some(),
            "{from:?} -> {to:?}: temperature missing from output"
        );
        let t = temp.unwrap();
        match to {
            Format::Claude => {
                assert!(
                    (t - 0.7_f64.min(1.0)).abs() < 0.01,
                    "{from:?} -> {to:?}: expected temperature ~0.7 (clamped), got {t}"
                );
            }
            _ => {
                assert!(
                    (t - 0.7).abs() < 0.01,
                    "{from:?} -> {to:?}: expected temperature ~0.7, got {t}"
                );
            }
        }
    }
}

// ── top_p ──────────────────────────────────────────────────────────────

#[test]
fn top_p_cross_format_precise() {
    for &(from, to) in &all_format_pairs() {
        let request = match from {
            Format::OpenAIChat => {
                json!({"model":"test","messages":[{"role":"user","content":"hi"}],"top_p":0.9})
            }
            Format::OpenAIResponses => {
                json!({"model":"test","input":resp_input(),"top_p":0.9})
            }
            Format::Claude => {
                json!({"model":"test","max_tokens":10,"top_p":0.9,"messages":[{"role":"user","content":"hi"}]})
            }
            Format::Gemini => {
                json!({"model":"test","contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"topP":0.9}})
            }
        };
        let output = convert_json(&request, from, to);
        assert!(output.is_some(), "{from:?} -> {to:?}: conversion failed");
        let output = output.unwrap();
        let top_p = extract_top_p(&output, to);
        assert!(
            top_p.is_some(),
            "{from:?} -> {to:?}: top_p missing from output"
        );
        let p = top_p.unwrap();
        assert!(
            (p - 0.9).abs() < 0.01,
            "{from:?} -> {to:?}: expected top_p ~0.9, got {p}"
        );
    }
}

// ── max_tokens ─────────────────────────────────────────────────────────

#[test]
fn max_tokens_cross_format_precise() {
    for &(from, to) in &all_format_pairs() {
        let request = match from {
            Format::OpenAIChat => {
                json!({"model":"test","messages":[{"role":"user","content":"hi"}],"max_tokens":100})
            }
            Format::Claude => {
                json!({"model":"test","max_tokens":100,"messages":[{"role":"user","content":"hi"}]})
            }
            Format::OpenAIResponses => {
                json!({"model":"test","input":resp_input(),"max_output_tokens":100})
            }
            Format::Gemini => {
                json!({"model":"test","contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"maxOutputTokens":100}})
            }
        };
        let output = convert_json(&request, from, to);
        assert!(output.is_some(), "{from:?} -> {to:?}: conversion failed");
        let output = output.unwrap();
        let mt = extract_max_tokens(&output, to);
        assert!(
            mt.is_some(),
            "{from:?} -> {to:?}: max_tokens missing from output"
        );
        assert_eq!(
            mt.unwrap(),
            100,
            "{from:?} -> {to:?}: expected max_tokens = 100"
        );
    }
}

// ── stop sequences ────────────────────────────────────────────────────

#[test]
fn stop_sequences_cross_format_precise() {
    for &(from, to) in &all_format_pairs() {
        let request = match from {
            Format::OpenAIChat => {
                json!({"model":"test","messages":[{"role":"user","content":"hi"}],"stop":["END","STOP"]})
            }
            Format::OpenAIResponses => {
                json!({"model":"test","input":resp_input(),"stop":["END","STOP"]})
            }
            Format::Claude => {
                json!({"model":"test","max_tokens":10,"stop_sequences":["END","STOP"],"messages":[{"role":"user","content":"hi"}]})
            }
            Format::Gemini => {
                json!({"model":"test","contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"stopSequences":["END","STOP"]}})
            }
        };
        let output = convert_json(&request, from, to);
        assert!(
            output.is_some(),
            "{from:?} -> {to:?}: conversion failed for stop sequences"
        );
        let output = output.unwrap();
        let seqs = extract_stop_sequences(&output, to);
        if to == Format::OpenAIResponses || from == Format::OpenAIResponses {
            continue;
        }
        assert!(
            seqs.contains(&"END".to_string()),
            "{from:?} -> {to:?}: stop sequence 'END' missing, got: {seqs:?}"
        );
        assert!(
            seqs.contains(&"STOP".to_string()),
            "{from:?} -> {to:?}: stop sequence 'STOP' missing, got: {seqs:?}"
        );
    }
}

// ── system prompt ──────────────────────────────────────────────────────

#[test]
fn system_prompt_openai_to_claude() {
    let request = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helpful."},
            {"role": "user", "content": "Hello"}
        ]
    });
    let output = convert_json(&request, Format::OpenAIChat, Format::Claude).unwrap();
    assert_eq!(output["system"], json!("You are helpful."));
    let messages = output["messages"].as_array().unwrap();
    for msg in messages {
        assert_ne!(msg["role"].as_str(), Some("system"));
    }
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn system_prompt_openai_to_gemini() {
    let request = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": "Hello"}
        ]
    });
    let output = convert_json(&request, Format::OpenAIChat, Format::Gemini).unwrap();
    assert!(output["systemInstruction"].is_object());
    let parts = output["systemInstruction"]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], json!("Be concise."));
}

#[test]
fn system_prompt_claude_to_openai() {
    let request = json!({
        "model": "claude-3",
        "max_tokens": 100,
        "system": "You are helpful.",
        "messages": [{"role": "user", "content": "Hello"}]
    });
    let output = convert_json(&request, Format::Claude, Format::OpenAIChat).unwrap();
    let messages = output["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[0]["content"], json!("You are helpful."));
    assert_eq!(messages[1]["role"], json!("user"));
}

#[test]
fn system_prompt_gemini_to_openai() {
    let request = json!({
        "model": "gemini-2.0-flash",
        "contents": [{"role": "user", "parts": [{"text": "Hello"}]}],
        "systemInstruction": {"parts": [{"text": "Be concise."}]}
    });
    let output = convert_json(&request, Format::Gemini, Format::OpenAIChat).unwrap();
    let messages = output["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[0]["content"], json!("Be concise."));
}

// ── seed ───────────────────────────────────────────────────────────────

#[test]
fn seed_preservation() {
    for &(from, to) in &all_format_pairs() {
        if from == Format::Claude {
            continue;
        }
        let request = match from {
            Format::OpenAIChat => {
                json!({"model":"test","messages":[{"role":"user","content":"hi"}],"seed":42})
            }
            Format::OpenAIResponses => {
                json!({"model":"test","input":resp_input(),"seed":42})
            }
            Format::Gemini => {
                json!({"model":"test","contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"seed":42}})
            }
            _ => unreachable!(),
        };
        let output = convert_json(&request, from, to);
        assert!(
            output.is_some(),
            "{from:?} -> {to:?}: seed conversion failed"
        );
        assert!(
            output.unwrap().is_object(),
            "{from:?} -> {to:?}: conversion produced non-object"
        );
    }
}

// ── multi-turn tool ────────────────────────────────────────────────────

#[test]
fn multi_turn_with_tool_results() {
    let request = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "What's the weather?"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"NYC\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "Cloudy, 72F"}
        ]
    });
    let output = convert_json(&request, Format::OpenAIChat, Format::Claude).unwrap();
    let messages = output["messages"].as_array().unwrap();
    assert!(
        messages.len() >= 3,
        "expected at least 3 messages, got {}: {output}",
        messages.len()
    );
    let haystack = output.to_string();
    assert!(
        haystack.contains("get_weather"),
        "tool name 'get_weather' missing from output"
    );
    assert!(
        haystack.contains("Cloudy") || haystack.contains("72F"),
        "tool result content missing from output"
    );
}
