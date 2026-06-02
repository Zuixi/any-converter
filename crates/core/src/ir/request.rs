use serde::{Deserialize, Serialize};

use super::{GenerationParams, SystemContent, ToolChoice, ToolDef, Turn};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemContent>,
    pub turns: Vec<Turn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub params: GenerationParams,
    #[serde(default)]
    pub stream: bool,
    /// Forward-compat: provider-specific fields that don't map to the canonical model
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl CanonicalRequest {
    pub fn simple(model: impl Into<String>, user_message: impl Into<String>) -> Self {
        use super::{ContentBlock, Role};
        Self {
            model: model.into(),
            system: None,
            turns: vec![Turn {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: user_message.into(),
                }],
            }],
            tools: vec![],
            tool_choice: None,
            params: GenerationParams::default(),
            stream: false,
            extra: serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ContentBlock, Role};

    #[test]
    fn test_simple_request() {
        let req = CanonicalRequest::simple("gpt-4", "Hello");
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.turns.len(), 1);
        assert_eq!(req.turns[0].role, Role::User);
        assert!(matches!(&req.turns[0].content[0], ContentBlock::Text { text } if text == "Hello"));
        assert!(!req.stream);
    }

    #[test]
    fn test_request_with_system() {
        let req = CanonicalRequest {
            system: Some(SystemContent::Text("You are helpful.".into())),
            ..CanonicalRequest::simple("claude-sonnet-4-20250514", "Hi")
        };
        assert_eq!(req.system.unwrap().as_text(), "You are helpful.");
    }

    #[test]
    fn test_request_with_tools() {
        let req = CanonicalRequest {
            tools: vec![ToolDef {
                name: "search".into(),
                description: Some("Search the web".into()),
                input_schema: serde_json::json!({"type": "object"}),
                strict: None,
            }],
            tool_choice: Some(ToolChoice::Auto),
            ..CanonicalRequest::simple("gpt-4", "Search for Rust")
        };
        assert_eq!(req.tools.len(), 1);
        assert!(matches!(req.tool_choice, Some(ToolChoice::Auto)));
    }

    #[test]
    fn test_request_serialization_roundtrip() {
        let req = CanonicalRequest::simple("gpt-4", "Hello");
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: CanonicalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, deserialized);
    }

    #[test]
    fn test_request_serialization_skips_empty() {
        let req = CanonicalRequest::simple("gpt-4", "Hello");
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tools"));
        assert!(!json.contains("tool_choice"));
        assert!(!json.contains("system"));
        assert!(!json.contains("extra"));
    }

    #[test]
    fn test_multi_turn_conversation() {
        let req = CanonicalRequest {
            turns: vec![
                Turn {
                    role: Role::User,
                    content: vec![ContentBlock::Text { text: "Hello".into() }],
                },
                Turn {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text { text: "Hi there!".into() }],
                },
                Turn {
                    role: Role::User,
                    content: vec![ContentBlock::Text { text: "How are you?".into() }],
                },
            ],
            ..CanonicalRequest::simple("gpt-4", "")
        };
        assert_eq!(req.turns.len(), 3);
        assert_eq!(req.turns[1].role, Role::Assistant);
    }
}
