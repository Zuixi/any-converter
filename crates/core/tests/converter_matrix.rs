#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use any_converter_core::convert::{Format, convert_request, convert_response};
use common::{
    assert_valid_json, extract_first_user_text, extract_model, extract_response_model,
    extract_response_text, extract_tool_names, format_dir_name, load_fixture,
};
use pretty_assertions::assert_eq;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

fn scenario_stem(fixture: &str) -> &str {
    fixture
        .strip_prefix("request_")
        .or_else(|| fixture.strip_prefix("response_"))
        .and_then(|s| s.strip_suffix(".json"))
        .unwrap_or(fixture)
}

fn output_json_string(output: &Value) -> String {
    output.to_string()
}

fn assert_output_contains(output: &Value, needle: &str) {
    let haystack = output_json_string(output);
    assert!(
        haystack.contains(needle),
        "expected output to contain {needle:?}, got: {haystack}"
    );
}

fn assert_request_target_structure(output: &Value, to: Format) {
    match to {
        Format::OpenAIChat => {
            assert!(
                output.get("model").is_some(),
                "OpenAI Chat request missing model"
            );
            assert!(
                output.get("messages").and_then(|m| m.as_array()).is_some(),
                "OpenAI Chat request missing messages array"
            );
        }
        Format::Claude => {
            assert!(
                output.get("model").is_some(),
                "Claude request missing model"
            );
            assert!(
                output.get("max_tokens").is_some(),
                "Claude request missing max_tokens"
            );
            assert!(
                output.get("messages").and_then(|m| m.as_array()).is_some(),
                "Claude request missing messages array"
            );
        }
        Format::OpenAIResponses => {
            assert!(
                output.get("model").is_some(),
                "OpenAI Responses request missing model"
            );
            assert!(
                output.get("input").is_some(),
                "OpenAI Responses request missing input"
            );
        }
        Format::Gemini => {
            assert!(
                output.get("contents").and_then(|c| c.as_array()).is_some(),
                "Gemini request missing contents array"
            );
        }
    }
}

fn assert_response_target_structure(output: &Value, to: Format) {
    match to {
        Format::OpenAIChat => {
            assert!(
                output.get("model").is_some(),
                "OpenAI Chat response missing model"
            );
            assert!(
                output.get("choices").and_then(|c| c.as_array()).is_some(),
                "OpenAI Chat response missing choices"
            );
        }
        Format::Claude => {
            assert!(
                output.get("model").is_some(),
                "Claude response missing model"
            );
            assert!(
                output.get("content").and_then(|c| c.as_array()).is_some(),
                "Claude response missing content array"
            );
        }
        Format::OpenAIResponses => {
            assert!(
                output.get("model").is_some(),
                "OpenAI Responses response missing model"
            );
            assert!(
                output.get("output").and_then(|o| o.as_array()).is_some(),
                "OpenAI Responses response missing output"
            );
        }
        Format::Gemini => {
            assert!(
                output
                    .get("candidates")
                    .and_then(|c| c.as_array())
                    .is_some(),
                "Gemini response missing candidates"
            );
        }
    }
}

fn assert_model_preserved(source: &Value, output: &Value, from: Format, to: Format) {
    let source_model = extract_model(source, from);
    if let Some(expected) = source_model {
        let out_model = extract_model(output, to);
        assert_eq!(
            out_model.as_deref(),
            Some(expected.as_str()),
            "model should be preserved from {from:?} to {to:?}"
        );
    }
}

fn assert_response_model_preserved(source: &Value, output: &Value, from: Format, to: Format) {
    let source_model = extract_response_model(source, from);
    if let Some(expected) = source_model {
        let out_model = extract_response_model(output, to);
        assert_eq!(
            out_model.as_deref(),
            Some(expected.as_str()),
            "response model should be preserved from {from:?} to {to:?}"
        );
    }
}

fn assert_request_tools_structure(output: &Value, to: Format) {
    match to {
        Format::OpenAIChat => {
            let tools = output
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("tools fixture should produce tools array");
            assert!(!tools.is_empty());
            let first = &tools[0];
            assert_eq!(first.get("type").and_then(|t| t.as_str()), Some("function"));
            assert!(first.get("function").and_then(|f| f.get("name")).is_some());
        }
        Format::Claude => {
            let tools = output
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("tools fixture should produce tools array");
            assert!(!tools.is_empty());
            let first = &tools[0];
            assert!(first.get("name").is_some());
            assert!(first.get("input_schema").is_some());
            if let Some(tc) = output.get("tool_choice") {
                assert_eq!(tc.get("type").and_then(|t| t.as_str()), Some("auto"));
            }
        }
        Format::OpenAIResponses => {
            let tools = output
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("tools fixture should produce tools array");
            assert!(!tools.is_empty());
            let first = &tools[0];
            assert_eq!(first.get("type").and_then(|t| t.as_str()), Some("function"));
            assert!(first.get("name").is_some());
        }
        Format::Gemini => {
            let tools = output
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("tools fixture should produce tools array");
            assert!(!tools.is_empty());
            let decls = tools[0]
                .get("functionDeclarations")
                .or_else(|| tools[0].get("function_declarations"))
                .and_then(|d| d.as_array())
                .expect("Gemini tools should have functionDeclarations");
            assert!(!decls.is_empty());
            assert!(decls[0].get("name").is_some());
        }
    }
}

fn assert_request_scenario_content(output: &Value, from: Format, to: Format, scenario: &str) {
    match scenario {
        "simple" => {
            let text = extract_first_user_text(output, to);
            assert!(
                text.is_some(),
                "{from:?} -> {to:?} (simple): user text missing"
            );
            assert_eq!(
                text.unwrap(),
                "Hello, how are you?",
                "{from:?} -> {to:?} (simple): user text mismatch"
            );
        }
        "tools" => {
            let tool_names = extract_tool_names(output, to);
            assert!(
                tool_names.contains(&"get_weather".to_string()),
                "{from:?} -> {to:?} (tools): tool 'get_weather' missing, got: {tool_names:?}"
            );
            assert_output_contains(output, "Tokyo");
        }
        "multi_turn" => {
            assert_output_contains(output, "And 3+3?");
            assert_output_contains(output, "2+2 equals 4.");
        }
        "image" => {
            let text = extract_first_user_text(output, to);
            if let Some(t) = text {
                assert_eq!(
                    t, "What is in this image?",
                    "{from:?} -> {to:?} (image): text mismatch"
                );
            } else {
                assert_output_contains(output, "What is in this image?");
            }
        }
        "system_message" => assert_output_contains(output, "You are a helpful assistant."),
        "reasoning" => match from {
            Format::OpenAIResponses => assert_output_contains(output, "Solve this step by step"),
            _ => assert_output_contains(output, "continue our conversation"),
        },
        "thinking" => assert_output_contains(output, "continue our conversation"),
        _ => {}
    }
}

fn assert_response_scenario_content(output: &Value, from: Format, to: Format, scenario: &str) {
    match scenario {
        "simple" => {
            let text = extract_response_text(output, to);
            assert!(
                text.is_some(),
                "{from:?} -> {to:?} (response simple): response text missing"
            );
            assert_eq!(
                text.unwrap(),
                "I'm doing well, thank you for asking!",
                "{from:?} -> {to:?} (response simple): text mismatch"
            );
        }
        "tools" => assert_output_contains(output, "get_weather"),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Test generation macros
// ---------------------------------------------------------------------------

macro_rules! test_request_conversion {
    ($name:ident, $from:expr, $to:expr, $fixture:literal) => {
        #[test]
        fn $name() {
            let from = $from;
            let to = $to;
            let from_dir = format_dir_name(from);
            let input = load_fixture(from_dir, $fixture);
            let source = assert_valid_json(&input);

            let result = convert_request(&input, from, to);
            assert!(result.is_ok(), "conversion failed: {:?}", result.err());
            let output_bytes = result.unwrap();
            let output = assert_valid_json(&output_bytes);

            assert_request_target_structure(&output, to);
            assert_model_preserved(&source, &output, from, to);

            let scenario = scenario_stem($fixture);
            if scenario == "tools" {
                assert_request_tools_structure(&output, to);
            }
            assert_request_scenario_content(&output, from, to, scenario);
        }
    };
}

macro_rules! test_response_conversion {
    ($name:ident, $from:expr, $to:expr, $fixture:literal) => {
        #[test]
        fn $name() {
            let from = $from;
            let to = $to;
            let from_dir = format_dir_name(from);
            let input = load_fixture(from_dir, $fixture);
            let source = assert_valid_json(&input);

            let result = convert_response(&input, from, to);
            assert!(result.is_ok(), "conversion failed: {:?}", result.err());
            let output_bytes = result.unwrap();
            let output = assert_valid_json(&output_bytes);

            assert_response_target_structure(&output, to);
            assert_response_model_preserved(&source, &output, from, to);

            let scenario = scenario_stem($fixture);
            assert_response_scenario_content(&output, from, to, scenario);
        }
    };
}

macro_rules! request_pair_fixtures {
    ($from:expr, $to:expr, $($test_name:ident, $fixture:literal),* $(,)?) => {
        $(
            test_request_conversion!($test_name, $from, $to, $fixture);
        )*
    };
}

macro_rules! response_pair_fixtures {
    (
        $from:expr,
        $to:expr,
        $simple_test:ident,
        $tools_test:ident
    ) => {
        test_response_conversion!($simple_test, $from, $to, "response_simple.json");
        test_response_conversion!($tools_test, $from, $to, "response_tools.json");
    };
}

// ---------------------------------------------------------------------------
// Request conversion matrix (12 pairs × source-available fixtures)
// ---------------------------------------------------------------------------

// OpenAI Chat → others (6 fixtures × 3 targets = 18 tests)
request_pair_fixtures!(
    Format::OpenAIChat,
    Format::Claude,
    request_openai_chat_to_claude_simple,
    "request_simple.json",
    request_openai_chat_to_claude_tools,
    "request_tools.json",
    request_openai_chat_to_claude_multi_turn,
    "request_multi_turn.json",
    request_openai_chat_to_claude_image,
    "request_image.json",
    request_openai_chat_to_claude_system_message,
    "request_system_message.json",
    request_openai_chat_to_claude_reasoning,
    "request_reasoning.json",
);

request_pair_fixtures!(
    Format::OpenAIChat,
    Format::OpenAIResponses,
    request_openai_chat_to_openai_resp_simple,
    "request_simple.json",
    request_openai_chat_to_openai_resp_tools,
    "request_tools.json",
    request_openai_chat_to_openai_resp_multi_turn,
    "request_multi_turn.json",
    request_openai_chat_to_openai_resp_image,
    "request_image.json",
    request_openai_chat_to_openai_resp_system_message,
    "request_system_message.json",
    request_openai_chat_to_openai_resp_reasoning,
    "request_reasoning.json",
);

request_pair_fixtures!(
    Format::OpenAIChat,
    Format::Gemini,
    request_openai_chat_to_gemini_simple,
    "request_simple.json",
    request_openai_chat_to_gemini_tools,
    "request_tools.json",
    request_openai_chat_to_gemini_multi_turn,
    "request_multi_turn.json",
    request_openai_chat_to_gemini_image,
    "request_image.json",
    request_openai_chat_to_gemini_system_message,
    "request_system_message.json",
    request_openai_chat_to_gemini_reasoning,
    "request_reasoning.json",
);

// Claude → others (6 fixtures × 3 targets = 18 tests)
request_pair_fixtures!(
    Format::Claude,
    Format::OpenAIChat,
    request_claude_to_openai_chat_simple,
    "request_simple.json",
    request_claude_to_openai_chat_tools,
    "request_tools.json",
    request_claude_to_openai_chat_multi_turn,
    "request_multi_turn.json",
    request_claude_to_openai_chat_image,
    "request_image.json",
    request_claude_to_openai_chat_system_message,
    "request_system_message.json",
    request_claude_to_openai_chat_thinking,
    "request_thinking.json",
);

request_pair_fixtures!(
    Format::Claude,
    Format::OpenAIResponses,
    request_claude_to_openai_resp_simple,
    "request_simple.json",
    request_claude_to_openai_resp_tools,
    "request_tools.json",
    request_claude_to_openai_resp_multi_turn,
    "request_multi_turn.json",
    request_claude_to_openai_resp_image,
    "request_image.json",
    request_claude_to_openai_resp_system_message,
    "request_system_message.json",
    request_claude_to_openai_resp_thinking,
    "request_thinking.json",
);

request_pair_fixtures!(
    Format::Claude,
    Format::Gemini,
    request_claude_to_gemini_simple,
    "request_simple.json",
    request_claude_to_gemini_tools,
    "request_tools.json",
    request_claude_to_gemini_multi_turn,
    "request_multi_turn.json",
    request_claude_to_gemini_system_message,
    "request_system_message.json",
    request_claude_to_gemini_thinking,
    "request_thinking.json",
);

// OpenAI Responses → others (4 fixtures × 3 targets = 12 tests)
request_pair_fixtures!(
    Format::OpenAIResponses,
    Format::OpenAIChat,
    request_openai_resp_to_openai_chat_simple,
    "request_simple.json",
    request_openai_resp_to_openai_chat_tools,
    "request_tools.json",
    request_openai_resp_to_openai_chat_multi_turn,
    "request_multi_turn.json",
    request_openai_resp_to_openai_chat_reasoning,
    "request_reasoning.json",
);

request_pair_fixtures!(
    Format::OpenAIResponses,
    Format::Claude,
    request_openai_resp_to_claude_simple,
    "request_simple.json",
    request_openai_resp_to_claude_tools,
    "request_tools.json",
    request_openai_resp_to_claude_multi_turn,
    "request_multi_turn.json",
    request_openai_resp_to_claude_reasoning,
    "request_reasoning.json",
);

request_pair_fixtures!(
    Format::OpenAIResponses,
    Format::Gemini,
    request_openai_resp_to_gemini_simple,
    "request_simple.json",
    request_openai_resp_to_gemini_tools,
    "request_tools.json",
    request_openai_resp_to_gemini_multi_turn,
    "request_multi_turn.json",
    request_openai_resp_to_gemini_reasoning,
    "request_reasoning.json",
);

// Gemini → others (3 fixtures × 3 targets = 9 tests)
request_pair_fixtures!(
    Format::Gemini,
    Format::OpenAIChat,
    request_gemini_to_openai_chat_simple,
    "request_simple.json",
    request_gemini_to_openai_chat_tools,
    "request_tools.json",
    request_gemini_to_openai_chat_multi_turn,
    "request_multi_turn.json",
);

request_pair_fixtures!(
    Format::Gemini,
    Format::Claude,
    request_gemini_to_claude_simple,
    "request_simple.json",
    request_gemini_to_claude_tools,
    "request_tools.json",
    request_gemini_to_claude_multi_turn,
    "request_multi_turn.json",
);

request_pair_fixtures!(
    Format::Gemini,
    Format::OpenAIResponses,
    request_gemini_to_openai_resp_simple,
    "request_simple.json",
    request_gemini_to_openai_resp_tools,
    "request_tools.json",
    request_gemini_to_openai_resp_multi_turn,
    "request_multi_turn.json",
);

// ---------------------------------------------------------------------------
// Response conversion matrix (12 pairs × 2 fixtures = 24 tests)
// ---------------------------------------------------------------------------

response_pair_fixtures!(
    Format::OpenAIChat,
    Format::Claude,
    response_openai_chat_to_claude_simple,
    response_openai_chat_to_claude_tools
);
response_pair_fixtures!(
    Format::OpenAIChat,
    Format::OpenAIResponses,
    response_openai_chat_to_openai_resp_simple,
    response_openai_chat_to_openai_resp_tools
);
response_pair_fixtures!(
    Format::OpenAIChat,
    Format::Gemini,
    response_openai_chat_to_gemini_simple,
    response_openai_chat_to_gemini_tools
);

response_pair_fixtures!(
    Format::Claude,
    Format::OpenAIChat,
    response_claude_to_openai_chat_simple,
    response_claude_to_openai_chat_tools
);
response_pair_fixtures!(
    Format::Claude,
    Format::OpenAIResponses,
    response_claude_to_openai_resp_simple,
    response_claude_to_openai_resp_tools
);
response_pair_fixtures!(
    Format::Claude,
    Format::Gemini,
    response_claude_to_gemini_simple,
    response_claude_to_gemini_tools
);

response_pair_fixtures!(
    Format::OpenAIResponses,
    Format::OpenAIChat,
    response_openai_resp_to_openai_chat_simple,
    response_openai_resp_to_openai_chat_tools
);
response_pair_fixtures!(
    Format::OpenAIResponses,
    Format::Claude,
    response_openai_resp_to_claude_simple,
    response_openai_resp_to_claude_tools
);
response_pair_fixtures!(
    Format::OpenAIResponses,
    Format::Gemini,
    response_openai_resp_to_gemini_simple,
    response_openai_resp_to_gemini_tools
);

response_pair_fixtures!(
    Format::Gemini,
    Format::OpenAIChat,
    response_gemini_to_openai_chat_simple,
    response_gemini_to_openai_chat_tools
);
response_pair_fixtures!(
    Format::Gemini,
    Format::Claude,
    response_gemini_to_claude_simple,
    response_gemini_to_claude_tools
);
response_pair_fixtures!(
    Format::Gemini,
    Format::OpenAIResponses,
    response_gemini_to_openai_resp_simple,
    response_gemini_to_openai_resp_tools
);
