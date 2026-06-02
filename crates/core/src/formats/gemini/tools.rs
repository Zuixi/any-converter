use crate::ir::*;

use super::types::*;

pub(super) fn parse_tool_config(value: &serde_json::Value) -> Option<ToolChoice> {
    let config = value.get("functionCallingConfig")?;
    let mode = config.get("mode")?.as_str()?;
    let allowed = config
        .get("allowedFunctionNames")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if allowed.len() == 1 {
        return Some(ToolChoice::Tool {
            name: allowed[0].clone(),
        });
    }

    match mode {
        "AUTO" | "VALIDATED" => Some(ToolChoice::Auto),
        "NONE" => Some(ToolChoice::None),
        "ANY" => Some(ToolChoice::Any),
        _ => None,
    }
}

pub(super) fn tool_choice_to_config(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => {
            serde_json::json!({ "functionCallingConfig": { "mode": "AUTO" } })
        }
        ToolChoice::None => {
            serde_json::json!({ "functionCallingConfig": { "mode": "NONE" } })
        }
        ToolChoice::Any => {
            serde_json::json!({ "functionCallingConfig": { "mode": "ANY" } })
        }
        ToolChoice::Tool { name } => serde_json::json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [name]
            }
        }),
    }
}

pub(super) fn generation_config_to_params(
    config: GeminiGenerationConfig,
) -> GenerationParams {
    let response_format = match (config.response_mime_type.as_deref(), &config.response_schema) {
        (Some("application/json"), Some(schema)) => {
            Some(ResponseFormat::JsonSchema {
                name: "response".into(),
                schema: schema.clone(),
                strict: None,
            })
        }
        (Some("application/json"), None) => Some(ResponseFormat::JsonObject),
        _ => None,
    };

    GenerationParams {
        temperature: config.temperature,
        top_p: config.top_p,
        top_k: config.top_k,
        max_output_tokens: config.max_output_tokens,
        stop_sequences: config.stop_sequences.unwrap_or_default(),
        seed: config.seed,
        response_format,
        ..Default::default()
    }
}

pub(super) fn params_to_generation_config(
    params: &GenerationParams,
) -> GeminiGenerationConfig {
    let (response_mime_type, response_schema) = match &params.response_format {
        Some(ResponseFormat::JsonObject) => (Some("application/json".into()), None),
        Some(ResponseFormat::JsonSchema { schema, .. }) => {
            (Some("application/json".into()), Some(schema.clone()))
        }
        Some(ResponseFormat::Text) | None => (None, None),
    };

    GeminiGenerationConfig {
        temperature: params.temperature,
        top_p: params.top_p,
        top_k: params.top_k,
        max_output_tokens: params.max_output_tokens,
        stop_sequences: if params.stop_sequences.is_empty() {
            None
        } else {
            Some(params.stop_sequences.clone())
        },
        seed: params.seed,
        response_mime_type,
        response_schema,
    }
}
