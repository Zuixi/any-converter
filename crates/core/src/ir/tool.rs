use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolChoice {
    Auto,
    None,
    Any,
    Tool { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_def_creation() {
        let tool = ToolDef {
            name: "get_weather".into(),
            description: Some("Get current weather".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"]
            }),
            strict: Some(true),
        };
        assert_eq!(tool.name, "get_weather");
        assert!(tool.strict.unwrap());
    }

    #[test]
    fn test_tool_choice_variants() {
        assert!(matches!(ToolChoice::Auto, ToolChoice::Auto));
        assert!(matches!(ToolChoice::None, ToolChoice::None));
        assert!(matches!(ToolChoice::Any, ToolChoice::Any));

        let specific = ToolChoice::Tool { name: "search".into() };
        if let ToolChoice::Tool { name } = &specific {
            assert_eq!(name, "search");
        }
    }

    #[test]
    fn test_tool_def_serialization_roundtrip() {
        let tool = ToolDef {
            name: "calculator".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            strict: None,
        };
        let json = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolDef = serde_json::from_str(&json).unwrap();
        assert_eq!(tool, deserialized);
        assert!(!json.contains("description"));
        assert!(!json.contains("strict"));
    }
}
