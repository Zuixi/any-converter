#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use any_converter_core::convert::{Format, convert_stream_event};
use any_converter_core::ir::StreamState;
use any_converter_core::sse::parse_sse_block;

use common::{format_dir_name, load_sse_blocks};

fn tool_fixture_name(from: Format) -> &'static str {
    match from {
        Format::OpenAIChat => "stream_tool_calls.sse",
        Format::Claude => "stream_tool_use.sse",
        Format::OpenAIResponses => "stream_tool_calls.sse",
        Format::Gemini => "stream_function_call.sse",
    }
}

fn run_stream_conversion(from: Format, to: Format, fixture: &str) -> (Vec<String>, StreamState) {
    let blocks = load_sse_blocks(format_dir_name(from), fixture);
    let mut state_in = StreamState::new();
    let mut state_out = StreamState::new();
    let mut all_output = Vec::new();

    for block in blocks {
        if let Some(event) = parse_sse_block(&block) {
            let lines =
                convert_stream_event(&event, from, to, &mut state_in, &mut state_out).unwrap();
            all_output.extend(lines);
        }
    }

    (all_output, state_out)
}

fn assert_text_stream_output(from: Format, to: Format, output: &[String], state: &StreamState) {
    assert!(
        !output.is_empty(),
        "expected non-empty SSE output for text stream {from:?} -> {to:?}",
    );

    let combined = output.join("\n");
    assert!(
        combined.contains("Hello"),
        "{from:?} -> {to:?}: expected 'Hello' in text stream output.\nOutput:\n{combined}",
    );

    assert!(
        combined.contains("data:"),
        "{from:?} -> {to:?}: output should contain SSE data lines"
    );

    match to {
        Format::OpenAIChat => {
            assert!(
                combined.contains("chat.completion.chunk"),
                "{from:?} -> OpenAI Chat: should contain chat.completion.chunk"
            );
        }
        Format::Claude => {
            assert!(
                combined.contains("content_block_delta") || combined.contains("text_delta"),
                "{from:?} -> Claude: should contain content_block_delta or text_delta"
            );
        }
        Format::OpenAIResponses => {
            assert!(
                combined.contains("response.output_text.delta")
                    || combined.contains("response.created"),
                "{from:?} -> Responses: should contain response events"
            );
        }
        Format::Gemini => {
            assert!(
                combined.contains("\"text\""),
                "{from:?} -> Gemini: should contain text field"
            );
        }
    }

    let _ = state;
}

fn assert_tool_stream_output(from: Format, to: Format, output: &[String], _state: &StreamState) {
    assert!(
        !output.is_empty(),
        "expected non-empty SSE output for tool stream {from:?} -> {to:?}",
    );

    let combined = output.join("\n");

    assert!(
        combined.contains("get_weather")
            || combined.contains("read_file")
            || combined.contains("function_call")
            || combined.contains("tool_use")
            || combined.contains("tool_calls")
            || combined.contains("functionCall"),
        "{from:?} -> {to:?}: expected tool/function content in output.\nOutput:\n{combined}",
    );

    match to {
        Format::OpenAIResponses => {
            assert!(
                combined.contains("response.output_item.done"),
                "{from:?} -> Responses: should emit response.output_item.done.\nOutput:\n{combined}"
            );
            assert!(
                combined.contains("function_call"),
                "{from:?} -> Responses: should emit function_call.\nOutput:\n{combined}"
            );
        }
        Format::Claude => {
            assert!(
                combined.contains("tool_use") || combined.contains("input_json_delta"),
                "{from:?} -> Claude: should emit tool_use events.\nOutput:\n{combined}"
            );
        }
        Format::OpenAIChat => {
            assert!(
                combined.contains("tool_calls"),
                "{from:?} -> OpenAI Chat: should emit tool_calls.\nOutput:\n{combined}"
            );
        }
        Format::Gemini => {
            assert!(
                combined.contains("functionCall"),
                "{from:?} -> Gemini: should emit functionCall.\nOutput:\n{combined}"
            );
        }
    }
}

fn assert_event_ordering(output: &[String], from: Format, to: Format) {
    let combined = output.join("\n");
    let lines: Vec<&str> = combined.lines().collect();

    let mut found_data = false;
    let mut found_openai_done = false;
    for line in &lines {
        let is_openai_done = line.contains("[DONE]");
        let is_terminal_event = line.contains("message_stop")
            || line.contains("response.completed")
            || line.contains("response.done");
        let is_content_data = line.starts_with("data:") && !is_openai_done && !is_terminal_event;

        if is_content_data {
            if found_openai_done {
                panic!("{from:?} -> {to:?}: content data after [DONE]: {line}");
            }
            found_data = true;
        }
        if is_openai_done {
            found_openai_done = true;
        }
    }
    assert!(
        found_data,
        "{from:?} -> {to:?}: no data lines found in output"
    );
}

macro_rules! stream_text_test {
    ($name:ident, $from:expr, $to:expr) => {
        #[test]
        fn $name() {
            let (output, state) = run_stream_conversion($from, $to, "stream_text.sse");
            assert_text_stream_output($from, $to, &output, &state);
            assert_event_ordering(&output, $from, $to);
        }
    };
}

macro_rules! stream_tool_test {
    ($name:ident, $from:expr, $to:expr) => {
        #[test]
        fn $name() {
            let fixture = tool_fixture_name($from);
            let (output, state) = run_stream_conversion($from, $to, fixture);
            assert_tool_stream_output($from, $to, &output, &state);
            assert_event_ordering(&output, $from, $to);
        }
    };
}

// OpenAI Chat -> *
stream_text_test!(
    stream_chat_to_claude_text,
    Format::OpenAIChat,
    Format::Claude
);
stream_tool_test!(
    stream_chat_to_claude_tools,
    Format::OpenAIChat,
    Format::Claude
);
stream_text_test!(
    stream_chat_to_resp_text,
    Format::OpenAIChat,
    Format::OpenAIResponses
);
stream_tool_test!(
    stream_chat_to_resp_tools,
    Format::OpenAIChat,
    Format::OpenAIResponses
);
stream_text_test!(
    stream_chat_to_gemini_text,
    Format::OpenAIChat,
    Format::Gemini
);
stream_tool_test!(
    stream_chat_to_gemini_tools,
    Format::OpenAIChat,
    Format::Gemini
);

// Claude -> *
stream_text_test!(
    stream_claude_to_chat_text,
    Format::Claude,
    Format::OpenAIChat
);
stream_tool_test!(
    stream_claude_to_chat_tools,
    Format::Claude,
    Format::OpenAIChat
);
stream_text_test!(
    stream_claude_to_resp_text,
    Format::Claude,
    Format::OpenAIResponses
);
stream_tool_test!(
    stream_claude_to_resp_tools,
    Format::Claude,
    Format::OpenAIResponses
);
stream_text_test!(stream_claude_to_gemini_text, Format::Claude, Format::Gemini);
stream_tool_test!(
    stream_claude_to_gemini_tools,
    Format::Claude,
    Format::Gemini
);

// OpenAI Responses -> *
stream_text_test!(
    stream_resp_to_chat_text,
    Format::OpenAIResponses,
    Format::OpenAIChat
);
stream_tool_test!(
    stream_resp_to_chat_tools,
    Format::OpenAIResponses,
    Format::OpenAIChat
);
stream_text_test!(
    stream_resp_to_claude_text,
    Format::OpenAIResponses,
    Format::Claude
);
stream_tool_test!(
    stream_resp_to_claude_tools,
    Format::OpenAIResponses,
    Format::Claude
);
stream_text_test!(
    stream_resp_to_gemini_text,
    Format::OpenAIResponses,
    Format::Gemini
);
stream_tool_test!(
    stream_resp_to_gemini_tools,
    Format::OpenAIResponses,
    Format::Gemini
);

// Gemini -> *
stream_text_test!(
    stream_gemini_to_chat_text,
    Format::Gemini,
    Format::OpenAIChat
);
stream_tool_test!(
    stream_gemini_to_chat_tools,
    Format::Gemini,
    Format::OpenAIChat
);
stream_text_test!(stream_gemini_to_claude_text, Format::Gemini, Format::Claude);
stream_tool_test!(
    stream_gemini_to_claude_tools,
    Format::Gemini,
    Format::Claude
);
stream_text_test!(
    stream_gemini_to_resp_text,
    Format::Gemini,
    Format::OpenAIResponses
);
stream_tool_test!(
    stream_gemini_to_resp_tools,
    Format::Gemini,
    Format::OpenAIResponses
);
