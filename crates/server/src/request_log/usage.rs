use any_converter_core::convert::Format;
use any_converter_core::ir::Usage;

/// Extract canonical token usage from a provider response body.
pub fn extract_usage(body: &[u8], format: Format) -> Usage {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Usage::default();
    };
    match format {
        Format::Claude => extract_claude_usage(&value),
        Format::Gemini => extract_gemini_usage(&value),
        Format::OpenAIChat | Format::OpenAIResponses => extract_openai_usage(&value),
    }
}

fn extract_openai_usage(value: &serde_json::Value) -> Usage {
    let Some(usage) = value.get("usage") else {
        return Usage::default();
    };
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: None,
        cache_write_1h: None,
        reasoning_tokens,
    }
}

fn extract_claude_usage(value: &serde_json::Value) -> Usage {
    let Some(usage) = value.get("usage") else {
        return Usage::default();
    };
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64());
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64());
    // Anthropic may break the write down by cache lifetime; the 1-hour part is
    // billed higher. `cache_creation_input_tokens` stays the total.
    let cache_write_1h = usage
        .get("cache_creation")
        .and_then(|c| c.get("ephemeral_1h_input_tokens"))
        .and_then(|v| v.as_u64());
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_write_1h,
        reasoning_tokens: None,
    }
}

/// The model the vendor's reply says answered. For a JSON reply it is the
/// top-level `model`; for an SSE body it is the last `model` seen in a
/// `data:` line. `None` when no reply names one.
pub fn extract_served_model(body: &[u8], format: Format) -> Option<String> {
    // Signature mirrors `extract_usage`; every vendor names the model the same way.
    let _ = format;

    if let Ok(serde_json::Value::Object(reply)) = serde_json::from_slice::<serde_json::Value>(body)
    {
        if let Some(model) = reply.get("model").and_then(|v| v.as_str()) {
            return Some(model.to_string());
        }
    }

    let text = String::from_utf8_lossy(body);
    let mut served = None;
    for line in text.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        // `message_start` nests the model under `message`; a flat event has it on top.
        let model = value
            .get("model")
            .or_else(|| value.get("message").and_then(|m| m.get("model")))
            .and_then(|v| v.as_str());
        if let Some(model) = model {
            served = Some(model.to_string());
        }
    }
    served
}

/// The reasoning effort the client asked for, on a single three-word scale.
///
/// Read from OpenAI's `reasoning_effort` or `reasoning.effort`, Claude's
/// `thinking.budget_tokens`, or Gemini's
/// `generationConfig.thinkingConfig.thinkingBudget`; Gemini's
/// `includeThoughts: false` means no reasoning at all. `None` when the body
/// asks for none of them.
pub fn extract_effort(body: &[u8]) -> Option<String> {
    // A request is one body, so the first one that names an effort is the ask.
    reply_values(body).iter().find_map(effort_of)
}

fn effort_of(value: &serde_json::Value) -> Option<String> {
    if let Some(effort) = value.get("reasoning_effort").and_then(|v| v.as_str()) {
        return Some(effort.to_string());
    }
    if let Some(effort) = value
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(|v| v.as_str())
    {
        return Some(effort.to_string());
    }
    if let Some(budget) = value
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|v| v.as_u64())
    {
        return Some(effort_of_budget(budget));
    }
    let thinking = value
        .get("generationConfig")
        .and_then(|c| c.get("thinkingConfig"));
    if let Some(budget) = thinking
        .and_then(|t| t.get("thinkingBudget"))
        .and_then(|v| v.as_u64())
    {
        return Some(effort_of_budget(budget));
    }
    if thinking.and_then(|t| t.get("includeThoughts")) == Some(&serde_json::Value::Bool(false)) {
        return Some("none".to_string());
    }
    None
}

/// Both vendors budget thinking in tokens; two cuts put their numbers on the
/// same `low`/`medium`/`high` scale.
fn effort_of_budget(tokens: u64) -> String {
    if tokens < 2048 {
        "low"
    } else if tokens < 8192 {
        "medium"
    } else {
        "high"
    }
    .to_string()
}

/// The model tier the body names: a top-level `service_tier`, or Claude's
/// `speed: "fast"`. `None` when it names none — a tier is never inferred,
/// because only the one the reply names affects the bill.
pub fn extract_tier(body: &[u8]) -> Option<String> {
    // A streamed reply can name the tier more than once; the last one answered.
    reply_values(body).iter().rev().find_map(|value| {
        if let Some(tier) = value.get("service_tier").and_then(|v| v.as_str()) {
            return Some(tier.to_string());
        }
        // Claude's speed knob has one tier worth naming.
        (value.get("speed").and_then(|v| v.as_str()) == Some("fast")).then(|| "fast".to_string())
    })
}

/// The provider an aggregator's reply says answered (OpenRouter's `provider`,
/// for instance). This is the vendor behind the model, not the model: `model`
/// names what was asked, `provider` names who served it.
pub fn extract_upstream_provider(body: &[u8]) -> Option<String> {
    reply_values(body).iter().rev().find_map(|value| {
        value
            .get("provider")
            .and_then(|v| v.as_str())
            .map(String::from)
    })
}

/// What kind of call this is, when the body makes it clear.
///
/// Rule, deliberately blunt: a body under 4 KiB with no top-level `tools` and
/// no `system` prompt is `auxiliary` — a small helper call, not the agent's own
/// turn. Anything else is `None` rather than guessed.
pub fn extract_kind(body: &[u8], agent: Option<&str>) -> Option<String> {
    // The delegation chain comes from headers; the body adds nothing to it.
    let _ = agent;
    const AUXILIARY_MAX_BYTES: usize = 4 * 1024;
    if body.len() >= AUXILIARY_MAX_BYTES {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let object = value.as_object()?;
    if object.contains_key("tools") || object.contains_key("system") {
        return None;
    }
    Some("auxiliary".to_string())
}

/// The JSON values a body carries: the body itself when it parses as one JSON
/// document, otherwise the `data:` payloads of an SSE stream, because a
/// streamed reply is many events rather than one body.
fn reply_values(body: &[u8]) -> Vec<serde_json::Value> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        return vec![value];
    }

    String::from_utf8_lossy(body)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty() && *data != "[DONE]")
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .collect()
}

fn extract_gemini_usage(value: &serde_json::Value) -> Usage {
    let Some(meta) = value.get("usageMetadata") else {
        return Usage::default();
    };
    let input_tokens = meta
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = meta
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cache_write_1h: None,
        reasoning_tokens: None,
    }
}

pub(super) fn extract_stream_usage_from_sse_lines(lines: &[String], format: Format) -> Usage {
    let mut latest = Usage::default();
    for line in lines {
        for data in extract_sse_data_payloads(line) {
            if data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            let usage = match format {
                Format::OpenAIResponses => value
                    .get("response")
                    .and_then(|response| response.get("usage"))
                    .map(|usage| extract_openai_usage(&serde_json::json!({ "usage": usage })))
                    .unwrap_or_default(),
                Format::OpenAIChat => extract_openai_usage(&value),
                Format::Claude => extract_claude_usage(&value).or_else(|| {
                    value
                        .get("message")
                        .and_then(|message| message.get("usage"))
                        .map(|usage| extract_claude_usage(&serde_json::json!({ "usage": usage })))
                        .unwrap_or_default()
                }),
                Format::Gemini => extract_gemini_usage(&value),
            };
            if usage.input_tokens > 0
                || usage.output_tokens > 0
                || usage.cache_read_tokens.is_some()
                || usage.cache_write_tokens.is_some()
                || usage.reasoning_tokens.is_some()
            {
                latest = usage;
            }
        }
    }
    latest
}

trait UsageExt {
    fn or_else<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self;
}

impl UsageExt for Usage {
    fn or_else<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self,
    {
        if self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.reasoning_tokens.is_some()
        {
            self
        } else {
            fallback()
        }
    }
}

fn extract_sse_data_payloads(block: &str) -> impl Iterator<Item = &str> {
    block
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_usage_openai_chat() {
        let body = br#"{"id":"chatcmpl-1","usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let usage = extract_usage(body, Format::OpenAIChat);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn test_extract_usage_claude() {
        let body = br#"{"id":"msg_1","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":10}}"#;
        let usage = extract_usage(body, Format::Claude);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, Some(10));
    }

    #[test]
    fn test_extract_usage_claude_cache_creation_breakdown() {
        let body = br#"{"id":"msg_1","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":150,"cache_creation":{"ephemeral_5m_input_tokens":50,"ephemeral_1h_input_tokens":100}}}"#;
        let usage = extract_usage(body, Format::Claude);
        // The total stays the total; the 1h part rides alongside it.
        assert_eq!(usage.cache_write_tokens, Some(150));
        assert_eq!(usage.cache_write_1h, Some(100));
    }

    #[test]
    fn test_extract_usage_claude_without_cache_creation_breakdown() {
        let body = br#"{"id":"msg_1","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":150}}"#;
        let usage = extract_usage(body, Format::Claude);
        assert_eq!(usage.cache_write_tokens, Some(150));
        assert_eq!(usage.cache_write_1h, None);
    }

    #[test]
    fn test_extract_served_model_from_json_reply() {
        let body = br#"{"id":"msg_1","model":"claude-sonnet-4-20250514","usage":{}}"#;
        assert_eq!(
            extract_served_model(body, Format::Claude).as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn test_extract_served_model_takes_last_model_from_sse() {
        let body = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-first\"}}\n\nevent: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-last\"}}\n\ndata: [DONE]\n\n";
        assert_eq!(
            extract_served_model(body, Format::Claude).as_deref(),
            Some("claude-last")
        );
    }

    #[test]
    fn test_extract_served_model_none_when_unnamed() {
        assert_eq!(
            extract_served_model(br#"{"usage":{"input_tokens":1}}"#, Format::Claude),
            None
        );
        assert_eq!(
            extract_served_model(b"data: [DONE]\n\n", Format::Claude),
            None
        );
        assert_eq!(extract_served_model(b"", Format::OpenAIChat), None);
    }

    #[test]
    fn test_extract_effort_flat_and_nested() {
        assert_eq!(
            extract_effort(br#"{"reasoning_effort":"high"}"#).as_deref(),
            Some("high")
        );
        assert_eq!(
            extract_effort(br#"{"reasoning":{"effort":"medium"}}"#).as_deref(),
            Some("medium")
        );
    }

    #[test]
    fn test_extract_effort_from_claude_budget() {
        assert_eq!(
            extract_effort(br#"{"thinking":{"type":"enabled","budget_tokens":1024}}"#).as_deref(),
            Some("low")
        );
        assert_eq!(
            extract_effort(br#"{"thinking":{"budget_tokens":4096}}"#).as_deref(),
            Some("medium")
        );
        assert_eq!(
            extract_effort(br#"{"thinking":{"budget_tokens":16384}}"#).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn test_extract_effort_from_gemini_thinking_config() {
        assert_eq!(
            extract_effort(br#"{"generationConfig":{"thinkingConfig":{"thinkingBudget":1024}}}"#)
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            extract_effort(br#"{"generationConfig":{"thinkingConfig":{"thinkingBudget":8192}}}"#)
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            extract_effort(br#"{"generationConfig":{"thinkingConfig":{"includeThoughts":false}}}"#)
                .as_deref(),
            Some("none")
        );
    }

    #[test]
    fn test_extract_effort_none_when_unspecified() {
        assert_eq!(extract_effort(br#"{}"#), None);
        assert_eq!(extract_effort(br#"{"thinking":{"type":"enabled"}}"#), None);
        assert_eq!(extract_effort(b"not json"), None);
        assert_eq!(extract_effort(b""), None);
    }

    #[test]
    fn test_extract_tier_from_service_tier_and_claude_speed() {
        assert_eq!(
            extract_tier(br#"{"service_tier":"flex"}"#).as_deref(),
            Some("flex")
        );
        assert_eq!(
            extract_tier(br#"{"speed":"fast"}"#).as_deref(),
            Some("fast")
        );
    }

    #[test]
    fn test_extract_tier_is_never_inferred() {
        assert_eq!(extract_tier(br#"{"model":"claude-3"}"#), None);
        assert_eq!(extract_tier(br#"{"speed":"standard"}"#), None);
        assert_eq!(extract_tier(br#"{}"#), None);
    }

    #[test]
    fn test_extract_tier_from_last_sse_event() {
        let body = b"data: {\"service_tier\":\"default\"}\n\ndata: {\"service_tier\":\"flex\"}\n\ndata: [DONE]\n\n";
        assert_eq!(extract_tier(body).as_deref(), Some("flex"));
    }

    #[test]
    fn test_extract_upstream_provider_is_not_the_model() {
        assert_eq!(
            extract_upstream_provider(br#"{"provider":"DeepInfra","model":"x"}"#).as_deref(),
            Some("DeepInfra")
        );
        assert_eq!(extract_upstream_provider(br#"{"model":"x"}"#), None);
        assert_eq!(extract_upstream_provider(br#"{}"#), None);
    }

    #[test]
    fn test_extract_kind_only_for_small_tool_free_bodies() {
        assert_eq!(
            extract_kind(br#"{"model":"m","messages":[{"role":"user"}]}"#, None).as_deref(),
            Some("auxiliary")
        );
        // Tools or a system prompt mean the agent is doing its own work.
        assert_eq!(extract_kind(br#"{"model":"m","tools":[]}"#, None), None);
        assert_eq!(
            extract_kind(br#"{"model":"m","system":"be brief"}"#, None),
            None
        );
        // Over 4 KiB is a real turn, not a helper call.
        let big = serde_json::json!({ "model": "m", "input": "x".repeat(4096) });
        assert_eq!(extract_kind(big.to_string().as_bytes(), None), None);
        // The agent hint does not change the rule.
        assert_eq!(
            extract_kind(br#"{"model":"m"}"#, Some("sub-1")).as_deref(),
            Some("auxiliary")
        );
        assert_eq!(extract_kind(b"not json", None), None);
    }

    #[test]
    fn test_extract_usage_gemini() {
        let body = br#"{"candidates":[],"usageMetadata":{"promptTokenCount":150,"candidatesTokenCount":60}}"#;
        let usage = extract_usage(body, Format::Gemini);
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 60);
    }

    #[test]
    fn test_extract_usage_missing() {
        let body = br#"{"error":"bad request"}"#;
        let usage = extract_usage(body, Format::OpenAIChat);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_extract_usage_from_responses_completed_sse_lines() {
        let lines = vec![format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "usage": {
                        "input_tokens": 1122,
                        "output_tokens": 52,
                        "total_tokens": 1174
                    }
                }
            })
        )];

        let usage = extract_stream_usage_from_sse_lines(&lines, Format::OpenAIResponses);
        assert_eq!(usage.input_tokens, 1122);
        assert_eq!(usage.output_tokens, 52);
    }
}
