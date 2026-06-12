use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    ContentFilter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl StopReason {
    /// Convert to the OpenAI Chat stop reason string.
    ///
    /// # Examples
    ///
    /// ```
    /// use any_converter_core::ir::StopReason;
    ///
    /// assert_eq!(StopReason::EndTurn.to_openai_chat(), "stop");
    /// assert_eq!(StopReason::MaxTokens.to_openai_chat(), "length");
    /// assert_eq!(StopReason::ToolUse.to_openai_chat(), "tool_calls");
    /// ```
    pub fn to_openai_chat(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "stop",
            StopReason::MaxTokens => "length",
            StopReason::ToolUse => "tool_calls",
            StopReason::StopSequence => "stop",
            StopReason::ContentFilter => "content_filter",
        }
    }

    pub fn to_claude(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::ToolUse => "tool_use",
            StopReason::StopSequence => "stop_sequence",
            StopReason::ContentFilter => "end_turn",
        }
    }

    pub fn to_gemini(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "STOP",
            StopReason::MaxTokens => "MAX_TOKENS",
            StopReason::ToolUse => "STOP",
            StopReason::StopSequence => "STOP",
            StopReason::ContentFilter => "SAFETY",
        }
    }

    pub fn from_openai_chat(s: &str) -> Self {
        match s {
            "stop" => StopReason::EndTurn,
            "length" => StopReason::MaxTokens,
            "tool_calls" => StopReason::ToolUse,
            "content_filter" => StopReason::ContentFilter,
            _ => StopReason::EndTurn,
        }
    }

    pub fn from_claude(s: &str) -> Self {
        match s {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "tool_use" => StopReason::ToolUse,
            "stop_sequence" => StopReason::StopSequence,
            "refusal" => StopReason::ContentFilter,
            _ => StopReason::EndTurn,
        }
    }

    pub fn from_gemini(s: &str) -> Self {
        match s {
            "STOP" => StopReason::EndTurn,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" | "PROHIBITED_CONTENT" => StopReason::ContentFilter,
            _ => StopReason::EndTurn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_total() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        assert_eq!(usage.total_tokens(), 150);
    }

    #[test]
    fn test_stop_reason_openai_roundtrip() {
        let reasons = [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::ToolUse,
            StopReason::ContentFilter,
        ];
        for reason in &reasons {
            let s = reason.to_openai_chat();
            let back = StopReason::from_openai_chat(s);
            assert_eq!(reason, &back, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_stop_reason_claude_roundtrip() {
        let reasons = [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::ToolUse,
            StopReason::StopSequence,
        ];
        for reason in &reasons {
            let s = reason.to_claude();
            let back = StopReason::from_claude(s);
            assert_eq!(reason, &back, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_stop_reason_gemini_mapping() {
        assert_eq!(StopReason::from_gemini("STOP"), StopReason::EndTurn);
        assert_eq!(StopReason::from_gemini("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(StopReason::from_gemini("SAFETY"), StopReason::ContentFilter);
        assert_eq!(StopReason::from_gemini("UNKNOWN"), StopReason::EndTurn);
    }
}
