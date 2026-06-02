use crate::error::ConvertError;
use crate::ir::{ResponseFormat, ToolChoice, ToolDef};

/// Parse tool definitions from a single tool JSON value.
/// Handles `type: "function"` (single tool) and `type: "namespace"` (flattens
/// child tools with qualified names like `namespace__toolname` for upstream
/// providers that only support flat function calling).
pub(super) fn parse_tool_defs(tool: serde_json::Value) -> Result<Vec<ToolDef>, ConvertError> {
    let tool_type = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match tool_type {
        "function" => parse_function_tool(&tool).map(|t| vec![t]),
        "namespace" => parse_namespace_tool(&tool),
        _ => Ok(vec![]),
    }
}

fn parse_function_tool(tool: &serde_json::Value) -> Result<ToolDef, ConvertError> {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool name".into()))?
        .to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let input_schema = tool
        .get("parameters")
        .cloned()
        .unwrap_or(serde_json::json!({"type": "object"}));
    let strict = tool.get("strict").and_then(|v| v.as_bool());

    Ok(ToolDef {
        name,
        description,
        input_schema,
        strict,
    })
}

/// Flatten a namespace tool into individual ToolDefs with qualified names.
/// The qualified name `{namespace}__{tool}` is used as the Claude-side tool name.
/// The server layer is responsible for splitting it back into separate
/// `namespace` + `name` fields in the Responses API function_call output.
fn parse_namespace_tool(tool: &serde_json::Value) -> Result<Vec<ToolDef>, ConvertError> {
    let namespace = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ns_description = tool.get("description").and_then(|v| v.as_str());

    let child_tools = tool
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut defs = Vec::with_capacity(child_tools.len());
    for child in &child_tools {
        let child_type = child.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if child_type != "function" {
            continue;
        }

        let child_name = child
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let qualified_name = if namespace.is_empty() {
            child_name.to_string()
        } else {
            format!("{namespace}__{child_name}")
        };

        let description = child
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| ns_description.map(String::from));

        let input_schema = child
            .get("parameters")
            .cloned()
            .unwrap_or(serde_json::json!({"type": "object"}));
        let strict = child.get("strict").and_then(|v| v.as_bool());

        defs.push(ToolDef {
            name: qualified_name,
            description,
            input_schema,
            strict,
        });
    }

    Ok(defs)
}

pub(super) fn parse_tool_choice(
    value: &serde_json::Value,
) -> Result<ToolChoice, ConvertError> {
    if let Some(s) = value.as_str() {
        return match s {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" => Ok(ToolChoice::Any),
            other => Err(ConvertError::InvalidField {
                field: "tool_choice".into(),
                reason: format!("unsupported value: {other}"),
            }),
        };
    }

    if value.get("type").and_then(|v| v.as_str()) == Some("function") {
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConvertError::MissingField("tool_choice name".into()))?
            .to_string();
        return Ok(ToolChoice::Tool { name });
    }

    Err(ConvertError::InvalidField {
        field: "tool_choice".into(),
        reason: "expected string or function object".into(),
    })
}

pub(super) fn parse_response_format(
    text: &serde_json::Value,
) -> Option<ResponseFormat> {
    let format = text.get("format")?;
    let format_type = format.get("type")?.as_str()?;

    match format_type {
        "text" => Some(ResponseFormat::Text),
        "json_object" => Some(ResponseFormat::JsonObject),
        "json_schema" => Some(ResponseFormat::JsonSchema {
            name: format.get("name")?.as_str()?.to_string(),
            schema: format.get("schema").cloned().unwrap_or(serde_json::json!({})),
            strict: format.get("strict").and_then(|v| v.as_bool()),
        }),
        _ => None,
    }
}

pub(super) fn tool_def_to_json(tool: &ToolDef) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
        "strict": tool.strict.unwrap_or(true),
    })
}

pub(super) fn tool_choice_to_json(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::Value::String("auto".into()),
        ToolChoice::None => serde_json::Value::String("none".into()),
        ToolChoice::Any => serde_json::Value::String("required".into()),
        ToolChoice::Tool { name } => serde_json::json!({
            "type": "function",
            "name": name,
        }),
    }
}

pub(super) fn response_format_to_text(format: &ResponseFormat) -> serde_json::Value {
    match format {
        ResponseFormat::Text => serde_json::json!({ "format": { "type": "text" } }),
        ResponseFormat::JsonObject => {
            serde_json::json!({ "format": { "type": "json_object" } })
        }
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => serde_json::json!({
            "format": {
                "type": "json_schema",
                "name": name,
                "schema": schema,
                "strict": strict.unwrap_or(true),
            }
        }),
    }
}
