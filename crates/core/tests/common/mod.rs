use std::path::PathBuf;

use any_converter_core::convert::Format;
use serde_json::Value;

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn load_fixture(format_dir: &str, name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(format_dir).join(name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("Failed to load fixture {}: {e}", path.display()))
}

pub fn load_fixture_str(format_dir: &str, name: &str) -> String {
    String::from_utf8(load_fixture(format_dir, name))
        .unwrap_or_else(|e| panic!("Fixture is not valid UTF-8: {e}"))
}

pub fn load_sse_blocks(format_dir: &str, name: &str) -> Vec<String> {
    let raw = load_fixture_str(format_dir, name);
    raw.split("\n\n")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub fn assert_valid_json(data: &[u8]) -> serde_json::Value {
    serde_json::from_slice(data).unwrap_or_else(|e| {
        let preview = String::from_utf8_lossy(&data[..data.len().min(200)]);
        panic!("Output is not valid JSON: {e}\nPreview: {preview}")
    })
}

pub fn all_format_pairs() -> Vec<(Format, Format)> {
    let formats = [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ];
    let mut pairs = Vec::new();
    for &from in &formats {
        for &to in &formats {
            if from != to {
                pairs.push((from, to));
            }
        }
    }
    pairs
}

pub fn format_dir_name(f: Format) -> &'static str {
    match f {
        Format::OpenAIChat => "openai_chat",
        Format::Claude => "claude",
        Format::OpenAIResponses => "openai_resp",
        Format::Gemini => "gemini",
    }
}

// ── Precise field extraction helpers ──────────────────────────────────

pub fn extract_model(json: &Value, format: Format) -> Option<String> {
    match format {
        Format::OpenAIChat | Format::Claude | Format::OpenAIResponses => {
            json.get("model").and_then(|v| v.as_str()).map(String::from)
        }
        Format::Gemini => json.get("model").and_then(|v| v.as_str()).map(String::from),
    }
}

pub fn extract_response_model(json: &Value, format: Format) -> Option<String> {
    match format {
        Format::OpenAIChat | Format::Claude | Format::OpenAIResponses => {
            json.get("model").and_then(|v| v.as_str()).map(String::from)
        }
        Format::Gemini => json
            .get("modelVersion")
            .or_else(|| json.get("model_version"))
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

pub fn extract_first_user_text(json: &Value, format: Format) -> Option<String> {
    match format {
        Format::OpenAIChat => json
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|msgs| {
                msgs.iter()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            })
            .and_then(|m| extract_openai_message_text(m)),
        Format::Claude => json
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|msgs| {
                msgs.iter()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            })
            .and_then(|m| extract_claude_message_text(m)),
        Format::OpenAIResponses => json.get("input").and_then(|input| {
            if let Some(text) = input.as_str() {
                return Some(text.to_string());
            }
            input.as_array().and_then(|items| {
                items.iter().find_map(|item| {
                    let role = item.get("role").and_then(|r| r.as_str());
                    if role == Some("user") {
                        extract_resp_item_text(item)
                    } else {
                        None
                    }
                })
            })
        }),
        Format::Gemini => json
            .get("contents")
            .and_then(|c| c.as_array())
            .and_then(|contents| {
                contents.iter().find_map(|c| {
                    let role = c.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    if role == "user" {
                        c.get("parts")
                            .and_then(|p| p.as_array())
                            .and_then(|parts| {
                                parts
                                    .iter()
                                    .find_map(|p| p.get("text").and_then(|t| t.as_str()))
                            })
                            .map(String::from)
                    } else {
                        None
                    }
                })
            }),
    }
}

fn extract_openai_message_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(parts) = content.as_array() {
        let texts: Vec<&str> = parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return Some(texts.join(""));
        }
    }
    None
}

fn extract_claude_message_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(blocks) = content.as_array() {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return Some(texts.join(""));
        }
    }
    None
}

fn extract_resp_item_text(item: &Value) -> Option<String> {
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(arr) = content.as_array() {
        let texts: Vec<&str> = arr
            .iter()
            .filter_map(|p| {
                let t = p.get("type").and_then(|v| v.as_str())?;
                if t == "input_text" || t == "output_text" || t == "text" {
                    p.get("text").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return Some(texts.join(""));
        }
    }
    None
}

pub fn extract_tool_names(json: &Value, format: Format) -> Vec<String> {
    match format {
        Format::OpenAIChat => json
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| {
                        t.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Format::Claude | Format::OpenAIResponses => json
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Format::Gemini => {
            let mut names = Vec::new();
            if let Some(tools) = json.get("tools").and_then(|v| v.as_array()) {
                for tool in tools {
                    if let Some(decls) = tool
                        .get("functionDeclarations")
                        .or_else(|| tool.get("function_declarations"))
                        .and_then(|d| d.as_array())
                    {
                        for decl in decls {
                            if let Some(name) = decl.get("name").and_then(|n| n.as_str()) {
                                names.push(name.to_string());
                            }
                        }
                    }
                }
            }
            names
        }
    }
}

pub fn extract_temperature(json: &Value, format: Format) -> Option<f64> {
    match format {
        Format::OpenAIChat | Format::OpenAIResponses | Format::Claude => {
            json.get("temperature").and_then(|v| v.as_f64())
        }
        Format::Gemini => json
            .get("generationConfig")
            .and_then(|g| g.get("temperature"))
            .and_then(|v| v.as_f64()),
    }
}

pub fn extract_top_p(json: &Value, format: Format) -> Option<f64> {
    match format {
        Format::OpenAIChat | Format::OpenAIResponses | Format::Claude => {
            json.get("top_p").and_then(|v| v.as_f64())
        }
        Format::Gemini => json
            .get("generationConfig")
            .and_then(|g| g.get("topP"))
            .and_then(|v| v.as_f64()),
    }
}

pub fn extract_max_tokens(json: &Value, format: Format) -> Option<u64> {
    match format {
        Format::OpenAIChat => json
            .get("max_tokens")
            .or_else(|| json.get("max_completion_tokens"))
            .and_then(|v| v.as_u64()),
        Format::Claude => json.get("max_tokens").and_then(|v| v.as_u64()),
        Format::OpenAIResponses => json.get("max_output_tokens").and_then(|v| v.as_u64()),
        Format::Gemini => json
            .get("generationConfig")
            .and_then(|g| g.get("maxOutputTokens"))
            .and_then(|v| v.as_u64()),
    }
}

pub fn extract_stop_sequences(json: &Value, format: Format) -> Vec<String> {
    match format {
        Format::OpenAIChat | Format::OpenAIResponses => {
            let stop = json.get("stop");
            match stop {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => vec![],
            }
        }
        Format::Claude => json
            .get("stop_sequences")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Format::Gemini => json
            .get("generationConfig")
            .and_then(|g| g.get("stopSequences"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn extract_response_text(json: &Value, format: Format) -> Option<String> {
    match format {
        Format::OpenAIChat => json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(String::from),
        Format::Claude => json
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|blocks| {
                blocks.iter().find_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
            }),
        Format::OpenAIResponses => {
            json.get("output")
                .and_then(|o| o.as_array())
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                            item.get("content")
                                .and_then(|c| c.as_array())
                                .and_then(|parts| {
                                    parts.iter().find_map(|p| {
                                        if p.get("type").and_then(|t| t.as_str())
                                            == Some("output_text")
                                        {
                                            p.get("text").and_then(|t| t.as_str()).map(String::from)
                                        } else {
                                            None
                                        }
                                    })
                                })
                        } else {
                            None
                        }
                    })
                })
        }
        Format::Gemini => json
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|cands| cands.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|parts| {
                parts
                    .iter()
                    .find_map(|p| p.get("text").and_then(|t| t.as_str()))
            })
            .map(String::from),
    }
}

pub fn extract_finish_reason(json: &Value, format: Format) -> Option<String> {
    match format {
        Format::OpenAIChat => json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|c| c.get("finish_reason"))
            .and_then(|r| r.as_str())
            .map(String::from),
        Format::Claude => json
            .get("stop_reason")
            .and_then(|r| r.as_str())
            .map(String::from),
        Format::OpenAIResponses => json
            .get("status")
            .and_then(|s| s.as_str())
            .map(String::from),
        Format::Gemini => json
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|cands| cands.first())
            .and_then(|c| c.get("finishReason"))
            .and_then(|r| r.as_str())
            .map(String::from),
    }
}

pub fn extract_usage_input_tokens(json: &Value, format: Format) -> Option<u64> {
    match format {
        Format::OpenAIChat => json
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64()),
        Format::OpenAIResponses => json
            .get("usage")
            .and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens")))
            .and_then(|v| v.as_u64()),
        Format::Claude => json
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64()),
        Format::Gemini => json
            .get("usageMetadata")
            .and_then(|u| u.get("promptTokenCount"))
            .and_then(|v| v.as_u64()),
    }
}

pub fn extract_usage_output_tokens(json: &Value, format: Format) -> Option<u64> {
    match format {
        Format::OpenAIChat => json
            .get("usage")
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_u64()),
        Format::OpenAIResponses => json
            .get("usage")
            .and_then(|u| {
                u.get("output_tokens")
                    .or_else(|| u.get("completion_tokens"))
            })
            .and_then(|v| v.as_u64()),
        Format::Claude => json
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64()),
        Format::Gemini => json
            .get("usageMetadata")
            .and_then(|u| u.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64()),
    }
}

pub fn extract_response_tool_names(json: &Value, format: Format) -> Vec<String> {
    match format {
        Format::OpenAIChat => json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .and_then(|tc| tc.as_array())
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| {
                        c.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Format::Claude => json
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            b.get("name").and_then(|n| n.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Format::OpenAIResponses => json
            .get("output")
            .and_then(|o| o.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                            item.get("name").and_then(|n| n.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Format::Gemini => json
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|cands| cands.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| {
                        p.get("functionCall")
                            .and_then(|fc| fc.get("name"))
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn extract_message_count(json: &Value, format: Format) -> usize {
    match format {
        Format::OpenAIChat | Format::Claude => json
            .get("messages")
            .and_then(|v| v.as_array())
            .map_or(0, |a| a.len()),
        Format::OpenAIResponses => json
            .get("input")
            .and_then(|v| v.as_array())
            .map_or(0, |a| a.len()),
        Format::Gemini => json
            .get("contents")
            .and_then(|v| v.as_array())
            .map_or(0, |a| a.len()),
    }
}

// ── Proptest helpers ──────────────────────────────────────────────────

pub fn all_formats() -> [Format; 4] {
    [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ]
}

pub fn minimal_request_json(model: &str, user_text: &str, format: Format) -> Value {
    match format {
        Format::OpenAIChat => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": user_text}]
        }),
        Format::Claude => serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": user_text}]
        }),
        Format::OpenAIResponses => serde_json::json!({
            "model": model,
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": user_text}]}]
        }),
        Format::Gemini => serde_json::json!({
            "model": model,
            "contents": [{"role": "user", "parts": [{"text": user_text}]}]
        }),
    }
}

pub fn minimal_response_json(model: &str, text: &str, format: Format) -> Value {
    match format {
        Format::OpenAIChat => serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }),
        Format::Claude => serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }),
        Format::OpenAIResponses => serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 0,
            "model": model,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        }),
        Format::Gemini => serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": text}]},
                "finishReason": "STOP",
                "index": 0
            }],
            "modelVersion": model,
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15}
        }),
    }
}
