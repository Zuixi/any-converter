use crate::error::ConvertError;
use crate::formats::FormatAdapter;
use crate::ir::*;

use super::tools;
use super::types::*;

pub struct GeminiAdapter;

impl FormatAdapter for GeminiAdapter {
    type Request = GeminiRequest;
    type Response = GeminiResponse;

    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn request_to_canonical(req: GeminiRequest) -> Result<CanonicalRequest, ConvertError> {
        let system = req
            .system_instruction
            .as_ref()
            .map(content_to_system)
            .transpose()?;

        let mut turns = Vec::new();
        for content in req.contents {
            turns.push(content_to_turn(content)?);
        }

        let tool_defs = req
            .tools
            .unwrap_or_default()
            .into_iter()
            .flat_map(|t| t.function_declarations)
            .map(|fd| ToolDef {
                name: fd.name,
                description: fd.description,
                input_schema: fd
                    .parameters
                    .unwrap_or(serde_json::json!({"type": "object"})),
                strict: None,
            })
            .collect();

        let tool_choice = req
            .tool_config
            .as_ref()
            .and_then(tools::parse_tool_config);

        let params = req
            .generation_config
            .map(tools::generation_config_to_params)
            .unwrap_or_default();

        Ok(CanonicalRequest {
            model: String::new(),
            system,
            turns,
            tools: tool_defs,
            tool_choice,
            params,
            stream: false,
            extra: serde_json::Value::Null,
        })
    }

    fn request_from_canonical(req: &CanonicalRequest) -> Result<GeminiRequest, ConvertError> {
        let system_instruction = req
            .system
            .as_ref()
            .map(system_to_content);

        let mut contents = Vec::new();
        for turn in &req.turns {
            contents.push(turn_to_content(turn)?);
        }

        let tool_decls = if req.tools.is_empty() {
            None
        } else {
            Some(vec![GeminiToolDeclaration {
                function_declarations: req
                    .tools
                    .iter()
                    .map(|t| GeminiFunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: Some(t.input_schema.clone()),
                    })
                    .collect(),
            }])
        };

        let tool_config = req.tool_choice.as_ref().map(tools::tool_choice_to_config);
        let generation_config = if req.params == GenerationParams::default() {
            None
        } else {
            Some(tools::params_to_generation_config(&req.params))
        };

        Ok(GeminiRequest {
            contents,
            system_instruction,
            generation_config,
            tools: tool_decls,
            tool_config,
        })
    }

    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError> {
        Ok(serde_json::from_slice(json)?)
    }

    fn response_to_canonical(resp: GeminiResponse) -> Result<CanonicalResponse, ConvertError> {
        let candidate = resp
            .candidates
            .first()
            .ok_or_else(|| ConvertError::MissingField("candidates".into()))?;

        let content = parts_to_blocks(&candidate.content.parts)?;

        let stop_reason = candidate
            .finish_reason
            .as_deref()
            .map(StopReason::from_gemini)
            .unwrap_or(StopReason::EndTurn);

        let usage = resp
            .usage_metadata
            .map(|u| Usage {
                input_tokens: u.prompt_token_count.unwrap_or(0),
                output_tokens: u.candidates_token_count.unwrap_or(0),
                ..Default::default()
            })
            .unwrap_or_default();

        Ok(CanonicalResponse {
            id: uuid::Uuid::new_v4().to_string(),
            model: resp.model_version.unwrap_or_default(),
            content,
            stop_reason,
            usage,
        })
    }

    fn response_from_canonical(resp: &CanonicalResponse) -> Result<GeminiResponse, ConvertError> {
        let parts = blocks_to_parts(&resp.content)?;

        let total_tokens = resp.usage.input_tokens + resp.usage.output_tokens;

        Ok(GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".into()),
                    parts,
                },
                finish_reason: Some(resp.stop_reason.to_gemini().into()),
                index: Some(0),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(resp.usage.input_tokens),
                candidates_token_count: Some(resp.usage.output_tokens),
                total_token_count: Some(total_tokens),
            }),
            model_version: if resp.model.is_empty() {
                None
            } else {
                Some(resp.model.clone())
            },
        })
    }
}

fn content_to_system(content: &GeminiContent) -> Result<SystemContent, ConvertError> {
    let text: String = content
        .parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    Ok(SystemContent::Text(text))
}

fn system_to_content(system: &SystemContent) -> GeminiContent {
    GeminiContent {
        role: None,
        parts: vec![GeminiPart {
            text: Some(system.as_text()),
            inline_data: None,
            function_call: None,
            function_response: None,
        }],
    }
}

fn gemini_role_to_ir(role: Option<&str>) -> Result<Role, ConvertError> {
    match role.unwrap_or("user") {
        "user" => Ok(Role::User),
        "model" => Ok(Role::Assistant),
        other => Err(ConvertError::InvalidField {
            field: "role".into(),
            reason: format!("unsupported role: {other}"),
        }),
    }
}

fn ir_role_to_gemini(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "model",
    }
}

fn content_to_turn(content: GeminiContent) -> Result<Turn, ConvertError> {
    Ok(Turn {
        role: gemini_role_to_ir(content.role.as_deref())?,
        content: parts_to_blocks(&content.parts)?,
    })
}

fn turn_to_content(turn: &Turn) -> Result<GeminiContent, ConvertError> {
    Ok(GeminiContent {
        role: Some(ir_role_to_gemini(&turn.role).into()),
        parts: blocks_to_parts(&turn.content)?,
    })
}

fn parts_to_blocks(parts: &[GeminiPart]) -> Result<Vec<ContentBlock>, ConvertError> {
    let mut blocks = Vec::new();
    for part in parts {
        if let Some(text) = &part.text {
            blocks.push(ContentBlock::Text { text: text.clone() });
        }
        if let Some(inline) = &part.inline_data {
            blocks.push(ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: inline.mime_type.clone(),
                    data: inline.data.clone(),
                },
            });
        }
        if let Some(fc) = &part.function_call {
            blocks.push(ContentBlock::ToolUse {
                id: fc
                    .id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: fc.name.clone(),
                input: fc.args.clone(),
            });
        }
        if let Some(fr) = &part.function_response {
            blocks.push(ContentBlock::ToolResult {
                tool_use_id: fr
                    .id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                content: response_value_to_content(&fr.response),
                is_error: fr
                    .response
                    .get("error")
                    .map(|e| !e.is_null())
                    .unwrap_or(false),
            });
        }
    }
    Ok(blocks)
}

fn blocks_to_parts(blocks: &[ContentBlock]) -> Result<Vec<GeminiPart>, ConvertError> {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                parts.push(GeminiPart {
                    text: Some(text.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                });
            }
            ContentBlock::Image { source } => match source {
                ImageSource::Base64 { media_type, data } => {
                    parts.push(GeminiPart {
                        text: None,
                        inline_data: Some(GeminiInlineData {
                            mime_type: media_type.clone(),
                            data: data.clone(),
                        }),
                        function_call: None,
                        function_response: None,
                    });
                }
                ImageSource::Url { .. } => {
                    return Err(ConvertError::UnsupportedContentType(
                        "url image".into(),
                    ));
                }
            },
            ContentBlock::ToolUse { id, name, input } => {
                parts.push(GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: Some(GeminiFunctionCall {
                        name: name.clone(),
                        args: input.clone(),
                        id: Some(id.clone()),
                    }),
                    function_response: None,
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let name = tool_result_name(content);
                parts.push(GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some(GeminiFunctionResponse {
                        name,
                        response: content_to_response_value(content, *is_error),
                        id: Some(tool_use_id.clone()),
                    }),
                });
            }
            ContentBlock::Thinking { text, .. } => {
                parts.push(GeminiPart {
                    text: Some(text.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                });
            }
        }
    }
    Ok(parts)
}

fn response_value_to_content(value: &serde_json::Value) -> Vec<ContentBlock> {
    match value {
        serde_json::Value::String(s) => vec![ContentBlock::Text { text: s.clone() }],
        serde_json::Value::Object(obj) => {
            if let Some(result) = obj.get("result").and_then(|v| v.as_str()) {
                vec![ContentBlock::Text {
                    text: result.to_string(),
                }]
            } else {
                vec![ContentBlock::Text {
                    text: serde_json::to_string(value).unwrap_or_default(),
                }]
            }
        }
        other => vec![ContentBlock::Text {
            text: other.to_string(),
        }],
    }
}

fn content_to_response_value(content: &[ContentBlock], is_error: bool) -> serde_json::Value {
    let text: String = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if is_error {
        serde_json::json!({ "error": text })
    } else {
        serde_json::json!({ "result": text })
    }
}

fn tool_result_name(content: &[ContentBlock]) -> String {
    content
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolUse { name, .. } => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "function".into())
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
