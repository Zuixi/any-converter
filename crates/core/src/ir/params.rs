use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GenerationParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

impl GenerationParams {
    /// Clamp temperature to the target format's valid range.
    /// Claude: [0, 1], OpenAI/Gemini: [0, 2]
    pub fn clamped_temperature(&self, max: f32) -> Option<f32> {
        self.temperature.map(|t| t.clamp(0.0, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_params() {
        let params = GenerationParams::default();
        assert!(params.temperature.is_none());
        assert!(params.top_p.is_none());
        assert!(params.max_output_tokens.is_none());
        assert!(params.stop_sequences.is_empty());
    }

    #[test]
    fn test_temperature_clamping() {
        let params = GenerationParams {
            temperature: Some(1.5),
            ..Default::default()
        };
        assert_eq!(params.clamped_temperature(1.0), Some(1.0));
        assert_eq!(params.clamped_temperature(2.0), Some(1.5));
    }

    #[test]
    fn test_temperature_clamping_none() {
        let params = GenerationParams::default();
        assert_eq!(params.clamped_temperature(1.0), None);
    }

    #[test]
    fn test_response_format_json_schema() {
        let fmt = ResponseFormat::JsonSchema {
            name: "person".into(),
            schema: serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            strict: Some(true),
        };
        let json = serde_json::to_string(&fmt).unwrap();
        assert!(json.contains("JsonSchema"));
        assert!(json.contains("person"));
    }

    #[test]
    fn test_params_serialization_skips_none() {
        let params = GenerationParams {
            temperature: Some(0.7),
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("temperature"));
        assert!(!json.contains("top_p"));
        assert!(!json.contains("stop_sequences"));
    }
}
