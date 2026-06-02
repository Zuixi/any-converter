use crate::ir::{ResponseFormat, ToolChoice};

use super::types::StopValue;

pub(super) fn stop_to_sequences(stop: StopValue) -> Vec<String> {
    match stop {
        StopValue::Single(s) => vec![s],
        StopValue::Multiple(v) => v,
    }
}

pub(super) fn sequences_to_stop(sequences: &[String]) -> Option<StopValue> {
    match sequences.len() {
        0 => None,
        1 => Some(StopValue::Single(sequences[0].clone())),
        _ => Some(StopValue::Multiple(sequences.to_vec())),
    }
}

pub(super) fn parse_tool_choice(value: &serde_json::Value) -> Option<ToolChoice> {
    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" => Some(ToolChoice::Any),
            _ => None,
        },
        serde_json::Value::Object(obj) => {
            let name = obj
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())?;
            Some(ToolChoice::Tool {
                name: name.into(),
            })
        }
        _ => None,
    }
}

pub(super) fn tool_choice_to_json(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Any => serde_json::json!("required"),
        ToolChoice::Tool { name } => serde_json::json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

pub(super) fn parse_response_format(value: &serde_json::Value) -> Option<ResponseFormat> {
    let typ = value.get("type")?.as_str()?;
    match typ {
        "text" => Some(ResponseFormat::Text),
        "json_object" => Some(ResponseFormat::JsonObject),
        "json_schema" => {
            let schema_obj = value.get("json_schema")?;
            Some(ResponseFormat::JsonSchema {
                name: schema_obj.get("name")?.as_str()?.into(),
                schema: schema_obj.get("schema")?.clone(),
                strict: schema_obj.get("strict").and_then(|s| s.as_bool()),
            })
        }
        _ => None,
    }
}

pub(super) fn response_format_to_json(format: &ResponseFormat) -> serde_json::Value {
    match format {
        ResponseFormat::Text => serde_json::json!({ "type": "text" }),
        ResponseFormat::JsonObject => serde_json::json!({ "type": "json_object" }),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema,
                "strict": strict
            }
        }),
    }
}
