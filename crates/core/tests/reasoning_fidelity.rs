//! End-to-end reasoning-fidelity proofs (magpie alignment):
//! Claude's signed thinking survives a round trip through a Responses
//! client (claudeseal, both directions, body and stream), Gemini's
//! thoughtSignature stays Gemini's, and no vendor's signature leaks into
//! another's wire format.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use any_converter_core::convert::{
    Format, convert_request, convert_response, convert_stream_event,
};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::{SseEvent, parse_sse_block};
use base64::Engine;
use serde_json::Value;

fn open_seal(encrypted: &str) -> Value {
    let b64 = encrypted
        .strip_prefix("magpie-claude1:")
        .unwrap_or_else(|| panic!("not a seal: {encrypted}"));
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    serde_json::from_slice(&raw).unwrap()
}

// ---------------------------------------------------------------- claude body

const CLAUDE_REPLY: &str = r#"{
  "id": "msg_signed_1", "type": "message", "role": "assistant",
  "model": "claude-sonnet-4-20250514",
  "content": [
    {"type": "thinking", "thinking": "I should check the weather.", "signature": "sig-abc123"},
    {"type": "text", "text": "Let me look that up."}
  ],
  "stop_reason": "tool_use",
  "usage": {"input_tokens": 10, "output_tokens": 20}
}"#;

#[test]
fn claude_signed_thinking_round_trips_through_a_responses_client() {
    // reply side: the reasoning item carries the seal
    let resp = convert_response(
        CLAUDE_REPLY.as_bytes(),
        Format::Claude,
        Format::OpenAIResponses,
    )
    .unwrap();
    let out: Value = serde_json::from_slice(&resp).unwrap();
    let items = out["output"].as_array().unwrap();
    let reasoning = items
        .iter()
        .find(|i| i["type"] == "reasoning")
        .expect("reasoning item present");
    let block = open_seal(reasoning["encrypted_content"].as_str().unwrap());
    assert_eq!(block["type"], "thinking");
    assert_eq!(block["thinking"], "I should check the weather.");
    assert_eq!(block["signature"], "sig-abc123");

    // ask side: the client's history reads back into the block as Claude wrote it
    let ask = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "stream": false,
        "input": [
            items.clone()[0],
            items.clone()[1],
            {"type": "message", "role": "user",
             "content": [{"type": "input_text", "text": "tokyo?"}]}
        ]
    });
    let back = convert_request(
        serde_json::to_string(&ask).unwrap().as_bytes(),
        Format::OpenAIResponses,
        Format::Claude,
    )
    .unwrap();
    let wire: Value = serde_json::from_slice(&back).unwrap();
    let blocks = wire["messages"][0]["content"].as_array().unwrap();
    let thinking = blocks.iter().find(|b| b["type"] == "thinking").unwrap();
    assert_eq!(thinking["thinking"], "I should check the weather.");
    assert_eq!(thinking["signature"], "sig-abc123");
}

#[test]
fn claude_redacted_thinking_round_trips_whole() {
    let reply = r#"{
      "id": "msg_redacted_1", "type": "message", "role": "assistant",
      "model": "claude-sonnet-4-20250514",
      "content": [
        {"type": "redacted_thinking", "data": "cmVkYWN0ZWQ="},
        {"type": "text", "text": "Answer."}
      ],
      "stop_reason": "end_turn",
      "usage": {"input_tokens": 5, "output_tokens": 5}
    }"#;
    let resp = convert_response(reply.as_bytes(), Format::Claude, Format::OpenAIResponses).unwrap();
    let out: Value = serde_json::from_slice(&resp).unwrap();
    let reasoning = out["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "reasoning")
        .expect("reasoning item present");
    let block = open_seal(reasoning["encrypted_content"].as_str().unwrap());
    assert_eq!(block["type"], "redacted_thinking");
    assert_eq!(block["data"], "cmVkYWN0ZWQ=");
}

// --------------------------------------------------------------- claude stream

const CLAUDE_STREAM: &[&str] = &[
    r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_s1","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":10}}}"#,
    r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"think "}}"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hard"}}"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-stream-1"}}"#,
    r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
    r#"event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
    r#"event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
    r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
    r#"event: message_stop
data: {"type":"message_stop"}"#,
];

#[test]
fn claude_stream_signature_seals_into_the_reasoning_item_done() {
    let mut state_in = StreamState::new();
    let mut state_out = StreamState::new();
    let mut lines = Vec::new();
    for block in CLAUDE_STREAM {
        let event: SseEvent = parse_sse_block(block).unwrap();
        lines.extend(
            convert_stream_event(
                &event,
                Format::Claude,
                Format::OpenAIResponses,
                &mut state_in,
                &mut state_out,
            )
            .unwrap(),
        );
    }
    let done: Value = lines
        .iter()
        .flat_map(|block| block.lines())
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str::<Value>(d).unwrap())
        .find(|v| v["type"] == "response.output_item.done" && v["item"]["type"] == "reasoning")
        .expect("reasoning item done event");
    let block = open_seal(done["item"]["encrypted_content"].as_str().unwrap());
    assert_eq!(block["thinking"], "think hard");
    assert_eq!(block["signature"], "sig-stream-1");
}

// ------------------------------------------------------------------- gemini

#[test]
fn gemini_signature_stays_gemini_and_does_not_leak_into_claude() {
    let ask = r#"{
      "contents": [
        {"role": "user", "parts": [{"text": "weather?"}]},
        {"role": "model", "parts": [
          {"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}, "id": "call_1"},
           "thoughtSignature": "gemini-sig-1"}
        ]}
      ]
    }"#;
    // to Claude: the signature is Gemini's, Claude must not see it
    let claude = convert_request(ask.as_bytes(), Format::Gemini, Format::Claude).unwrap();
    let wire: Value = serde_json::from_slice(&claude).unwrap();
    let json = wire.to_string();
    assert!(
        !json.contains("gemini-sig-1"),
        "signature leaked into claude wire"
    );
    assert!(
        !json.contains("thoughtSignature"),
        "field leaked into claude wire"
    );
    // and Claude's own signature does not reach Gemini either
    let claude_ask = r#"{
      "model": "claude-sonnet-4-20250514", "max_tokens": 100,
      "messages": [
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": [
          {"type": "thinking", "thinking": "hmm", "signature": "claude-sig-9"}
        ]}
      ]
    }"#;
    let gemini = convert_request(claude_ask.as_bytes(), Format::Claude, Format::Gemini).unwrap();
    let json = String::from_utf8(gemini).unwrap();
    assert!(
        !json.contains("claude-sig-9"),
        "claude signature leaked into gemini wire"
    );
}

// --------------------------------------------------------------------- chat

#[test]
fn claude_signature_drops_cleanly_to_chat() {
    let resp =
        convert_response(CLAUDE_REPLY.as_bytes(), Format::Claude, Format::OpenAIChat).unwrap();
    let out: Value = serde_json::from_slice(&resp).unwrap();
    let json = out.to_string();
    assert!(
        json.contains("I should check the weather."),
        "thinking text kept"
    );
    assert!(!json.contains("sig-abc123"), "signature leaked into chat");
    assert!(!json.contains("magpie-claude1:"), "seal leaked into chat");
}
