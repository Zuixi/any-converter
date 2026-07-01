use crate::formats::claude::ThinkingConfig;

/// Maps an OpenAI-style reasoning effort string to a Claude `ThinkingConfig`.
///
/// Supported effort values: `none`, `low`, `medium`, `high`. Unknown values are
/// treated as `medium` to keep the request valid for upstream providers that
/// emit non-standard effort labels.
pub fn reasoning_effort_to_thinking(
    effort: Option<&str>,
    max_tokens: u32,
) -> Option<ThinkingConfig> {
    match effort? {
        "none" => None,
        "low" => Some(ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: 1024,
        }),
        "high" => Some(ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: max_tokens.max(4096),
        }),
        _ => Some(ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: max_tokens.max(2048),
        }),
    }
}

/// Maps a Claude `ThinkingConfig` back to an OpenAI-style reasoning effort string.
pub fn thinking_to_reasoning_effort(thinking: &ThinkingConfig) -> Option<String> {
    if thinking.r#type != "enabled" {
        return Some("none".into());
    }

    Some(
        (if thinking.budget_tokens <= 1024 {
            "low"
        } else if thinking.budget_tokens >= 4096 {
            "high"
        } else {
            "medium"
        })
        .into(),
    )
}

/// Maps a Claude `ThinkingConfig` to an OpenAI Responses-style reasoning object.
pub fn thinking_to_reasoning_json(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    thinking_to_reasoning_effort(thinking).map(|effort| serde_json::json!({ "effort": effort }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_effort_returns_no_thinking() {
        assert!(reasoning_effort_to_thinking(Some("none"), 4096).is_none());
    }

    #[test]
    fn test_low_effort_maps_to_small_budget() {
        let cfg = reasoning_effort_to_thinking(Some("low"), 4096).unwrap();
        assert_eq!(cfg.r#type, "enabled");
        assert_eq!(cfg.budget_tokens, 1024);
    }

    #[test]
    fn test_medium_effort_maps_to_default_budget() {
        let cfg = reasoning_effort_to_thinking(Some("medium"), 4096).unwrap();
        assert_eq!(cfg.r#type, "enabled");
        assert_eq!(cfg.budget_tokens, 4096);
    }

    #[test]
    fn test_medium_effort_respects_max_tokens_floor() {
        let cfg = reasoning_effort_to_thinking(Some("medium"), 1024).unwrap();
        assert_eq!(cfg.budget_tokens, 2048);
    }

    #[test]
    fn test_high_effort_maps_to_large_budget() {
        let cfg = reasoning_effort_to_thinking(Some("high"), 8192).unwrap();
        assert_eq!(cfg.r#type, "enabled");
        assert_eq!(cfg.budget_tokens, 8192);
    }

    #[test]
    fn test_unknown_effort_treated_as_medium() {
        let cfg = reasoning_effort_to_thinking(Some("xhigh"), 4096).unwrap();
        assert_eq!(cfg.r#type, "enabled");
        assert_eq!(cfg.budget_tokens, 4096);
    }

    #[test]
    fn test_missing_effort_returns_none() {
        assert!(reasoning_effort_to_thinking(None, 4096).is_none());
    }

    #[test]
    fn test_thinking_disabled_maps_to_none() {
        let cfg = ThinkingConfig {
            r#type: "disabled".into(),
            budget_tokens: 0,
        };
        assert_eq!(thinking_to_reasoning_effort(&cfg), Some("none".to_string()));
    }

    #[test]
    fn test_thinking_low_budget_maps_to_low() {
        let cfg = ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: 512,
        };
        assert_eq!(thinking_to_reasoning_effort(&cfg), Some("low".to_string()));
    }

    #[test]
    fn test_thinking_medium_budget_maps_to_medium() {
        let cfg = ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: 2048,
        };
        assert_eq!(
            thinking_to_reasoning_effort(&cfg),
            Some("medium".to_string())
        );
    }

    #[test]
    fn test_thinking_high_budget_maps_to_high() {
        let cfg = ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: 8192,
        };
        assert_eq!(thinking_to_reasoning_effort(&cfg), Some("high".to_string()));
    }

    #[test]
    fn test_thinking_to_reasoning_json() {
        let cfg = ThinkingConfig {
            r#type: "enabled".into(),
            budget_tokens: 1024,
        };
        assert_eq!(
            thinking_to_reasoning_json(&cfg),
            Some(serde_json::json!({ "effort": "low" }))
        );
    }
}
