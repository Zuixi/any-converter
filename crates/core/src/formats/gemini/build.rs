//! IR → Gemini wire. Ported from `converters/{chat,claude,resp}_to_gemini.rs`;
//! where those three disagree, the majority wins (see `read` for the reverse).
//! `pub fn build(ir: IrRequest) -> GeminiRequest` consumes the IR: strings and
//! payloads are moved, never copied.
use serde_json::Value;

use crate::convert::Format;
use crate::converters::shared::parse_data_url;
use crate::formats::gemini::*;
use crate::ir::*;

pub fn build(mut ir: IrRequest) -> GeminiRequest {
    let config_extra = ir.extra.take(Format::Gemini);

    let system_instruction = {
        let mut parts = Vec::new();
        for part in ir.system {
            if let IrPart::Text { text: text, .. } = part {
                if !text.is_empty() {
                    parts.push(GeminiPart::text(text));
                }
            }
        }
        (!parts.is_empty()).then_some(GeminiContent { role: None, parts })
    };

    let contents = ir
        .messages
        .into_iter()
        .map(|msg| GeminiContent {
            role: Some(
                match msg.role {
                    Role::User => "user",
                    Role::Assistant => "model",
                }
                .into(),
            ),
            parts: build_parts(msg.parts),
        })
        .collect();

    let generation_config = build_generation_config(ir.sampling, config_extra);
    let tools = build_tools(ir.tools);
    let tool_config = build_tool_config(ir.tool_choice, tools.is_some());

    GeminiRequest {
        model: Some(ir.model),
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    }
}

fn build_parts(parts: Vec<IrPart>) -> Vec<GeminiPart> {
    let mut out = Vec::new();
    for part in parts {
        push_part(&mut out, part);
    }
    out
}

fn push_part(out: &mut Vec<GeminiPart>, part: IrPart) {
    match part {
        IrPart::Text { text: text, .. } => {
            if !text.is_empty() {
                out.push(GeminiPart::text(text));
            }
        }
        // Gemini has no image URL field, so a non-inline image degrades to
        // its URL as text — what both URL-taking writers did.
        IrPart::Image { url, .. } => out.push(match parse_data_url(&url) {
            Some((mime_type, data)) => GeminiPart::inline_data(mime_type, data),
            None => GeminiPart::text(url),
        }),
        IrPart::ToolCall { id, name, args, .. } => {
            out.push(GeminiPart::function_call_with_id(name, args, id));
        }
        IrPart::ToolResult {
            call_id, content, ..
        } => {
            let text = content
                .into_iter()
                .filter_map(|p| match p {
                    IrPart::Text { text: text, .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            out.push(GeminiPart::function_response_with_id(
                "function",
                serde_json::json!({ "result": text }),
                call_id,
            ));
        }
        // Reasoning reaches Gemini as plain text in every existing writer.
        IrPart::Thinking { text, .. } => {
            if !text.is_empty() {
                out.push(GeminiPart::text(text));
            }
        }
        // Only the format that sealed it may replay it; Gemini never does.
        IrPart::Sealed { .. } => {}
    }
}

fn build_generation_config(
    sampling: Sampling,
    extra: Vec<(String, Value)>,
) -> Option<GeminiGenerationConfig> {
    let Sampling {
        max_tokens,
        temperature,
        top_p,
        top_k,
        stop,
    } = sampling;

    let mut config = GeminiGenerationConfig {
        temperature,
        top_p,
        top_k,
        max_output_tokens: max_tokens,
        stop_sequences: (!stop.is_empty()).then_some(stop),
        seed: None,
        response_mime_type: None,
        response_schema: None,
    };

    for (name, value) in extra {
        match name.as_str() {
            "seed" => config.seed = value.as_i64(),
            "responseMimeType" => {
                config.response_mime_type = value.as_str().map(String::from);
            }
            "responseSchema" => config.response_schema = Some(value),
            // No Gemini wire field: dropped rather than leaked.
            _ => {}
        }
    }

    let empty = config.temperature.is_none()
        && config.top_p.is_none()
        && config.top_k.is_none()
        && config.max_output_tokens.is_none()
        && config.stop_sequences.is_none()
        && config.seed.is_none()
        && config.response_mime_type.is_none()
        && config.response_schema.is_none();
    (!empty).then_some(config)
}

fn build_tools(tools: Vec<IrTool>) -> Option<Vec<GeminiToolDeclaration>> {
    if tools.is_empty() {
        return None;
    }
    Some(vec![GeminiToolDeclaration {
        function_declarations: tools
            .into_iter()
            .map(|t| GeminiFunctionDeclaration {
                name: t.name,
                // An empty description is how a reader represents an absent one.
                description: (!t.description.is_empty()).then_some(t.description),
                parameters: Some(t.schema),
            })
            .collect(),
    }])
}

/// An explicit choice maps to Gemini's `functionCallingConfig` exactly as the
/// pairwise builders did; an absent one stays off the wire.
fn build_tool_config(choice: Option<IrToolChoice>, has_tools: bool) -> Option<Value> {
    let config = match choice? {
        // Gemini's own default; only written when tools could be called.
        IrToolChoice::Auto if has_tools => serde_json::json!({"mode": "AUTO"}),
        IrToolChoice::Auto => return None,
        IrToolChoice::None => serde_json::json!({"mode": "NONE"}),
        IrToolChoice::Required => serde_json::json!({"mode": "ANY"}),
        IrToolChoice::Named(name) => {
            serde_json::json!({"mode": "ANY", "allowedFunctionNames": [name]})
        }
    };
    Some(serde_json::json!({ "functionCallingConfig": config }))
}

pub fn build_response(ir: IrResponse) -> GeminiResponse {
    let mut parts = Vec::new();
    for part in ir.parts {
        match part {
            IrPart::Text { .. } | IrPart::ToolCall { .. } | IrPart::Thinking { .. } => {
                push_part(&mut parts, part);
            }
            // A reply carries no images or tool results in any existing writer.
            IrPart::Image { .. } | IrPart::ToolResult { .. } | IrPart::Sealed { .. } => {}
        }
    }

    let total = ir.usage.total_tokens();
    GeminiResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".into()),
                parts,
            },
            finish_reason: Some(ir.stop_reason.to_gemini().to_string()),
            index: Some(0),
        }],
        usage_metadata: Some(GeminiUsageMetadata {
            prompt_token_count: Some(ir.usage.input_tokens),
            candidates_token_count: Some(ir.usage.output_tokens),
            total_token_count: Some(total),
        }),
        model_version: Some(ir.model),
    }
}
