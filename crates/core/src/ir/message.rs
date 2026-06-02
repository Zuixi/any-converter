use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

impl SystemContent {
    pub fn as_text(&self) -> String {
        match self {
            SystemContent::Text(t) => t.clone(),
            SystemContent::Blocks(blocks) => {
                blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_equality() {
        assert_eq!(Role::User, Role::User);
        assert_ne!(Role::User, Role::Assistant);
    }

    #[test]
    fn test_content_block_text() {
        let block = ContentBlock::Text {
            text: "hello".to_string(),
        };
        assert!(matches!(block, ContentBlock::Text { text } if text == "hello"));
    }

    #[test]
    fn test_system_content_as_text() {
        let text = SystemContent::Text("You are helpful.".to_string());
        assert_eq!(text.as_text(), "You are helpful.");

        let blocks = SystemContent::Blocks(vec![
            SystemBlock { text: "Line 1".into(), cache_control: None },
            SystemBlock { text: "Line 2".into(), cache_control: None },
        ]);
        assert_eq!(blocks.as_text(), "Line 1\nLine 2");
    }

    #[test]
    fn test_image_source_url() {
        let src = ImageSource::Url {
            url: "https://example.com/img.png".into(),
            detail: Some("high".into()),
        };
        assert!(matches!(src, ImageSource::Url { url, detail } if url.contains("example") && detail == Some("high".into())));
    }

    #[test]
    fn test_tool_use_content_block() {
        let block = ContentBlock::ToolUse {
            id: "call_123".into(),
            name: "get_weather".into(),
            input: serde_json::json!({"location": "Boston"}),
        };
        if let ContentBlock::ToolUse { id, name, input } = &block {
            assert_eq!(id, "call_123");
            assert_eq!(name, "get_weather");
            assert_eq!(input["location"], "Boston");
        } else {
            panic!("expected ToolUse");
        }
    }

    #[test]
    fn test_tool_result_with_error() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_123".into(),
            content: vec![ContentBlock::Text { text: "error occurred".into() }],
            is_error: true,
        };
        if let ContentBlock::ToolResult { is_error, .. } = &block {
            assert!(is_error);
        }
    }

    #[test]
    fn test_thinking_block() {
        let block = ContentBlock::Thinking {
            text: "Let me think...".into(),
            signature: Some("sig_abc".into()),
        };
        if let ContentBlock::Thinking { text, signature } = &block {
            assert_eq!(text, "Let me think...");
            assert_eq!(signature.as_deref(), Some("sig_abc"));
        }
    }

    #[test]
    fn test_turn_with_multiple_blocks() {
        let turn = Turn {
            role: Role::User,
            content: vec![
                ContentBlock::Text { text: "Look at this image:".into() },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.com/cat.jpg".into(),
                        detail: None,
                    },
                },
            ],
        };
        assert_eq!(turn.role, Role::User);
        assert_eq!(turn.content.len(), 2);
    }
}
