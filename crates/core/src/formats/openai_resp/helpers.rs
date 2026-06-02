use crate::error::ConvertError;
use crate::ir::StopReason;

/// Extract function_call fields (call_id, name, parsed arguments) from a JSON object.
/// Shared between adapter (input/output parsing) and stream processing.
pub(super) fn parse_function_call_fields(
    item: &serde_json::Value,
) -> Result<(String, String, serde_json::Value), ConvertError> {
    let call_id = item
        .get("call_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("function_call call_id".into()))?
        .to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("function_call name".into()))?
        .to_string();
    let arguments = item
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let input: serde_json::Value = serde_json::from_str(arguments)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    Ok((call_id, name, input))
}

/// Serialize a function_call to a JSON object.
pub(super) fn emit_function_call_json(
    id: &str,
    name: &str,
    input: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "function_call",
        "call_id": id,
        "name": name,
        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
    })
}

pub(super) fn status_to_stop_reason(status: &str) -> StopReason {
    match status {
        "completed" => StopReason::EndTurn,
        "incomplete" => StopReason::MaxTokens,
        "failed" => StopReason::ContentFilter,
        _ => StopReason::EndTurn,
    }
}

pub(super) fn stop_reason_to_status(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::MaxTokens => "incomplete",
        StopReason::ContentFilter => "failed",
        _ => "completed",
    }
}
