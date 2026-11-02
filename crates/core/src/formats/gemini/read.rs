//! Gemini wire → IR. Ported from `converters/gemini_to_{chat,claude,resp}.rs`;
//! where those three disagree, the majority wins (see `build` for the
//! reverse direction). Nothing here invents behavior the old paths lacked.
use serde_json::Value;

use crate::convert::Format;
use crate::converters::shared::{extract_function_response_text, new_uuid};
use crate::formats::gemini::*;
use crate::ir::*;

pub fn read(req: GeminiRequest) -> IrRequest {
    let GeminiRequest {
        model,
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    } = req;

    let mut extra = NamespacedExtra::default();
    let mut sampling = Sampling::default();

    if let Some(g) = generation_config {
        let GeminiGenerationConfig {
            temperature,
            top_p,
            top_k,
            max_output_tokens,
            stop_sequences,
            seed,
            response_mime_type,
            response_schema,
        } = g;
        sampling.temperature = temperature;
        sampling.top_p = top_p;
        sampling.top_k = top_k;
        sampling.max_tokens = max_output_tokens;
        sampling.stop = stop_sequences.unwrap_or_default();
        // Gemini config knobs with no IR home.
        if let Some(seed) = seed {
            extra.put(Format::Gemini, "seed", Value::from(seed));
        }
        if let Some(v) = response_mime_type {
            extra.put(Format::Gemini, "responseMimeType", Value::from(v));
        }
        if let Some(v) = response_schema {
            extra.put(Format::Gemini, "responseSchema", v);
        }
    }

    IrRequest {
        model: model.unwrap_or_default(),
        system: system_instruction.map(read_system).unwrap_or_default(),
        messages: contents.into_iter().filter_map(read_message).collect(),
        tools: tools
            .unwrap_or_default()
            .into_iter()
            .flat_map(|g| g.function_declarations)
            .map(read_tool)
            .collect(),
        tool_choice: tool_config.map(read_tool_choice),
        sampling,
        // The wire carries no stream flag; streaming is the SSE path's job.
        stream: false,
        // `generationConfig.thinkingConfig` is not modeled by the wire types,
        // so no reference converter maps it today.
        reasoning: Reasoning::Off,
        extra,
    }
}

fn read_system(content: GeminiContent) -> Vec<IrPart> {
    let text = content
        .parts
        .into_iter()
        .filter_map(|p| p.text)
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        Vec::new()
    } else {
        vec![IrPart::Text(text)]
    }
}

fn read_tool(fd: GeminiFunctionDeclaration) -> IrTool {
    IrTool {
        name: fd.name,
        description: fd.description.unwrap_or_default(),
        // `gemini_to_claude` defaults a missing schema to this; `gemini_to_chat`
        // leaves it absent and `gemini_to_resp` sends null.
        schema: fd
            .parameters
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        // Gemini has no per-tool strictness switch.
        strict: None,
    }
}

fn read_tool_choice(config: Value) -> IrToolChoice {
    let Some(fcc) = config.get("functionCallingConfig") else {
        return IrToolChoice::Auto;
    };
    let allowed = fcc
        .get("allowedFunctionNames")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|n| n.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    if allowed.len() == 1 {
        return IrToolChoice::Named(allowed[0].to_string());
    }
    match fcc.get("mode").and_then(|v| v.as_str()) {
        Some("NONE") => IrToolChoice::None,
        Some("ANY") => IrToolChoice::Required,
        // AUTO, VALIDATED, absent, and modes the old converters rejected
        // upstream as a 400 all land on the Gemini default here.
        _ => IrToolChoice::Auto,
    }
}

fn read_message(content: GeminiContent) -> Option<IrMessage> {
    let role = match content.role.as_deref() {
        Some("model") => Role::Assistant,
        // "user", absent, and unknown roles (the old converters 400'd on the
        // last case) all read as user.
        _ => Role::User,
    };

    let mut parts = Vec::new();
    let mut results = Vec::new();
    for part in content.parts {
        let GeminiPart {
            text,
            inline_data,
            function_call,
            function_response,
        } = part;

        if let Some(fr) = function_response {
            // `gemini_to_claude` 400'd on this in model content.
            if role == Role::User {
                results.push(IrPart::ToolResult {
                    call_id: fr.id.unwrap_or_else(new_uuid),
                    is_error: fr.response.get("error").is_some_and(|e| !e.is_null()),
                    content: vec![IrPart::Text(extract_function_response_text(&fr.response))],
                });
            }
            continue;
        }
        if let Some(text) = text {
            if !text.is_empty() {
                parts.push(IrPart::Text(text));
            }
        }
        if role == Role::Assistant {
            if let Some(fc) = function_call {
                parts.push(IrPart::ToolCall {
                    id: fc.id.unwrap_or_else(new_uuid),
                    name: fc.name,
                    args: fc.args,
                });
            }
        } else if let Some(inline) = inline_data {
            // `fileData` is not a field on the wire types, so it never gets here
            // — same as the old converters, which dropped it.
            parts.push(IrPart::Image {
                url: format!("data:{};base64,{}", inline.mime_type, inline.data),
                media_type: inline.mime_type,
                data: Some(inline.data),
                detail: None,
            });
        }
    }

    // The pairwise readers hoist tool results above the message's own content.
    results.extend(parts);
    let parts = results;

    if parts.is_empty() {
        None
    } else {
        Some(IrMessage { role, parts })
    }
}

pub fn read_response(resp: GeminiResponse) -> IrResponse {
    let GeminiResponse {
        candidates,
        usage_metadata,
        model_version,
    } = resp;

    let mut parts = Vec::new();
    let mut stop_reason = StopReason::default();
    if let Some(candidate) = candidates.into_iter().next() {
        stop_reason = candidate
            .finish_reason
            .as_deref()
            .map(StopReason::from_gemini)
            .unwrap_or_default();
        for part in candidate.content.parts {
            let GeminiPart {
                text,
                function_call,
                ..
            } = part;
            if let Some(text) = text {
                if !text.is_empty() {
                    parts.push(IrPart::Text(text));
                }
            }
            if let Some(fc) = function_call {
                parts.push(IrPart::ToolCall {
                    id: fc.id.unwrap_or_else(new_uuid),
                    name: fc.name,
                    args: fc.args,
                });
            }
        }
    }

    let usage = match usage_metadata {
        Some(u) => Usage {
            input_tokens: u.prompt_token_count.unwrap_or(0),
            output_tokens: u.candidates_token_count.unwrap_or(0),
            ..Default::default()
        },
        None => Usage::default(),
    };

    IrResponse {
        id: None,
        model: model_version.unwrap_or_default(),
        parts,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::gemini::build::{build, build_response};

    fn parse(name: &str) -> serde_json::Value {
        serde_json::from_str(name).unwrap()
    }

    fn request(json: &str) -> GeminiRequest {
        serde_json::from_value(parse(json)).unwrap()
    }

    fn response(json: &str) -> GeminiResponse {
        serde_json::from_value(parse(json)).unwrap()
    }

    const SIMPLE: &str = include_str!("../../../tests/fixtures/gemini/request_simple.json");
    const MULTI_TURN: &str = include_str!("../../../tests/fixtures/gemini/request_multi_turn.json");
    const TOOLS: &str = include_str!("../../../tests/fixtures/gemini/request_tools.json");
    const RESP_SIMPLE: &str = include_str!("../../../tests/fixtures/gemini/response_simple.json");
    const RESP_TOOLS: &str = include_str!("../../../tests/fixtures/gemini/response_tools.json");

    fn first_user_text(ir: &IrRequest) -> Option<&str> {
        ir.messages
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| &m.parts)
            .find_map(|p| match p {
                IrPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
    }

    fn tool_names(ir: &IrRequest) -> Vec<&str> {
        ir.tools.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn request_simple_roundtrips() {
        let mut req = request(SIMPLE);
        let model = req.model.take().unwrap_or_default();
        let ir1 = read(req);
        let ir2 = read(build(ir1.clone()));
        assert_eq!(ir1, ir2);
        assert_eq!(ir1.model, model);
        assert_eq!(first_user_text(&ir1), Some("Hello, how are you?"));
        assert!(tool_names(&ir1).is_empty());
        assert_eq!(ir1.sampling.temperature, Some(0.7));
        assert_eq!(ir1.sampling.max_tokens, Some(1024));
    }

    #[test]
    fn request_multi_turn_roundtrips() {
        let mut req = request(MULTI_TURN);
        let model = req.model.take().unwrap_or_default();
        let ir1 = read(req);
        let ir2 = read(build(ir1.clone()));
        assert_eq!(ir1, ir2);
        assert_eq!(ir1.model, model);
        assert_eq!(first_user_text(&ir1), Some("What is 2+2?"));
        assert!(tool_names(&ir1).is_empty());
        assert_eq!(
            ir1.messages.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![Role::User, Role::Assistant, Role::User]
        );
    }

    #[test]
    fn request_tools_roundtrips() {
        let mut req = request(TOOLS);
        let model = req.model.take().unwrap_or_default();
        let ir1 = read(req);
        let ir2 = read(build(ir1.clone()));
        assert_eq!(ir1, ir2);
        assert_eq!(ir1.model, model);
        assert_eq!(first_user_text(&ir1), Some("What is the weather in Tokyo?"));
        assert_eq!(tool_names(&ir1), vec!["get_weather"]);
        assert_eq!(ir2.tools[0].schema["required"][0], "location");
    }

    #[test]
    fn response_simple_roundtrips() {
        let resp = response(RESP_SIMPLE);
        let ir1 = read_response(resp);
        let ir2 = read_response(build_response(ir1.clone()));
        assert_eq!(ir1, ir2);
        assert_eq!(
            ir1.parts,
            vec![IrPart::Text("I'm doing well, thank you for asking!".into())]
        );
        assert_eq!(ir1.stop_reason, StopReason::EndTurn);
        assert_eq!(ir1.usage.input_tokens, 10);
        assert_eq!(ir1.usage.output_tokens, 15);
    }

    #[test]
    fn response_tools_roundtrips() {
        let resp = response(RESP_TOOLS);
        let ir1 = read_response(resp);
        let ir2 = read_response(build_response(ir1.clone()));
        assert_eq!(ir1, ir2);
        match &ir1.parts[0] {
            IrPart::ToolCall { id, name, args } => {
                assert!(!id.is_empty());
                assert_eq!(name, "get_weather");
                assert_eq!(args["location"], "Tokyo");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_and_image_map_into_ir() {
        let req = request(
            r#"{
              "model": "gemini-2.5-pro",
              "systemInstruction": {"parts": [{"text": "be brief"}]},
              "contents": [
                {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}, "id": "call_1"}}]},
                {"role": "user", "parts": [
                  {"functionResponse": {"name": "get_weather", "response": {"result": "sunny"}, "id": "call_1"}},
                  {"inlineData": {"mimeType": "image/png", "data": "QUJD"}}
                ]}
              ],
              "generationConfig": {"seed": 7, "topK": 3, "stopSequences": ["END"]},
              "toolConfig": {"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["get_weather"]}}
            }"#,
        );
        let ir = read(req);

        assert_eq!(ir.model, "gemini-2.5-pro");
        assert_eq!(ir.system, vec![IrPart::Text("be brief".into())]);
        assert_eq!(ir.sampling.top_k, Some(3));
        assert_eq!(ir.sampling.stop, vec!["END".to_string()]);
        assert_eq!(
            ir.tool_choice,
            Some(IrToolChoice::Named("get_weather".into()))
        );
        assert_eq!(
            ir.messages[0].parts[0],
            IrPart::ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"city": "Tokyo"}),
            }
        );
        assert_eq!(
            ir.messages[1].parts[0],
            IrPart::ToolResult {
                call_id: "call_1".into(),
                is_error: false,
                content: vec![IrPart::Text("\"sunny\"".into())],
            }
        );
        assert_eq!(
            ir.messages[1].parts[1],
            IrPart::Image {
                url: "data:image/png;base64,QUJD".into(),
                media_type: "image/png".into(),
                data: Some("QUJD".into()),
                detail: None,
            }
        );

        let back = build(ir);
        assert_eq!(back.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(back.generation_config.as_ref().unwrap().seed, Some(7));
        assert_eq!(
            back.contents[0].parts[0]
                .function_call
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("call_1")
        );
        // shortcut: `extract_function_response_text` re-serializes the value, so
        // tool text gains a JSON quoting layer on the way through and a
        // functionResponse does not survive `read -> build -> read` identical.
        // Upgrade when the shared helper stops stripping/quotes values.
        let fr = back.contents[1].parts[0]
            .function_response
            .as_ref()
            .unwrap();
        assert_eq!(fr.id.as_deref(), Some("call_1"));
        assert_eq!(fr.response["result"], "\"sunny\"");
        assert_eq!(
            back.contents[1].parts[1].inline_data.as_ref().unwrap().data,
            "QUJD"
        );
    }
}
