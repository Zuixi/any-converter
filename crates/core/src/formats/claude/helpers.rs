use crate::error::ConvertError;
use crate::ir::*;

use super::types::*;

pub(super) fn parse_tool_choice(
    value: serde_json::Value,
) -> Result<ToolChoice, ConvertError> {
    let obj = value.as_object().ok_or_else(|| ConvertError::InvalidField {
        field: "tool_choice".into(),
        reason: "expected object".into(),
    })?;
    let choice_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField("tool_choice.type".into()))?;

    match choice_type {
        "auto" => Ok(ToolChoice::Auto),
        "none" => Ok(ToolChoice::None),
        "any" => Ok(ToolChoice::Any),
        "tool" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField("tool_choice.name".into()))?
                .to_string();
            Ok(ToolChoice::Tool { name })
        }
        other => Err(ConvertError::InvalidField {
            field: "tool_choice.type".into(),
            reason: format!("unsupported type: {other}"),
        }),
    }
}

pub(super) fn tool_choice_to_claude(
    choice: &ToolChoice,
) -> Result<serde_json::Value, ConvertError> {
    let value = match choice {
        ToolChoice::Auto => serde_json::json!({"type": "auto"}),
        ToolChoice::None => serde_json::json!({"type": "none"}),
        ToolChoice::Any => serde_json::json!({"type": "any"}),
        ToolChoice::Tool { name } => serde_json::json!({"type": "tool", "name": name}),
    };
    Ok(value)
}

/// Merge consecutive turns with the same role into single turns.
/// Claude API requires strictly alternating user/assistant messages.
/// Parallel tool calls in Responses format produce separate function_call items
/// that become separate assistant Turns — these must be merged into one.
pub(super) fn merge_consecutive_same_role_turns(turns: &[Turn]) -> Vec<Turn> {
    let mut merged: Vec<Turn> = Vec::with_capacity(turns.len());
    for turn in turns {
        if let Some(last) = merged.last_mut() {
            if last.role == turn.role {
                last.content.extend(turn.content.iter().cloned());
                continue;
            }
        }
        merged.push(turn.clone());
    }
    merged
}

pub(super) fn claude_usage_to_ir(usage: &ClaudeUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        cache_write_tokens: usage.cache_creation_input_tokens,
        reasoning_tokens: None,
    }
}

pub(super) fn ir_usage_to_claude(usage: &Usage) -> ClaudeUsage {
    ClaudeUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_write_tokens,
        cache_read_input_tokens: usage.cache_read_tokens,
    }
}
