#![allow(dead_code, clippy::unwrap_used)]

use std::io::Write;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::NamedTempFile;

fn binary() -> Command {
    Command::cargo_bin("any-converter").unwrap()
}

const OPENAI_REQUEST: &str = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}"#;

const SSE_STREAM: &str = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":0,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-123","object":"chat.completion.chunk","created":0,"model":"gpt-4","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":"stop"}]}

data: [DONE]

"#;

const VALID_CONFIG: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "openai"
format = "openai-chat"
base_url = "https://api.openai.com"
api_key = "test-key"

[[routes]]
client_format = "openai-chat"
provider = "openai"
"#;

// ── convert stdin ──────────────────────────────────────────────────────

#[test]
fn convert_stdin() {
    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("claude")
        .arg("--stdin")
        .write_stdin(OPENAI_REQUEST)
        .assert()
        .success()
        .stdout(predicate::str::contains("model"))
        .stdout(predicate::str::contains("messages"));
}

// ── convert file ───────────────────────────────────────────────────────

#[test]
fn convert_file() {
    let mut file = NamedTempFile::new().unwrap();
    write!(file, "{OPENAI_REQUEST}").unwrap();

    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("claude")
        .arg(file.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("model"));
}

// ── convert response ───────────────────────────────────────────────────

#[test]
fn convert_response_flag() {
    // Use a response fixture as input
    let response_json = r#"{"id":"chatcmpl-123","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi!"},"finish_reason":"stop"}]}"#;
    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("claude")
        .arg("--response")
        .arg("--stdin")
        .write_stdin(response_json)
        .assert()
        .success();
}

// ── identity passthrough ───────────────────────────────────────────────

#[test]
fn convert_identity_passthrough() {
    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("openai-chat")
        .arg("--stdin")
        .write_stdin(OPENAI_REQUEST)
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-4"))
        .stdout(predicate::str::contains("messages"));
}

// ── invalid json ───────────────────────────────────────────────────────

#[test]
fn convert_invalid_json() {
    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("claude")
        .arg("--stdin")
        .write_stdin("not valid json {{{")
        .assert()
        .failure();
}

// ── unknown format ─────────────────────────────────────────────────────

#[test]
fn convert_unknown_format() {
    binary()
        .arg("convert")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("nonexistent_format")
        .arg("--stdin")
        .write_stdin(OPENAI_REQUEST)
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

// ── stream ─────────────────────────────────────────────────────────────

#[test]
fn stream_basic() {
    binary()
        .arg("stream")
        .arg("--from")
        .arg("openai-chat")
        .arg("--to")
        .arg("claude")
        .write_stdin(SSE_STREAM)
        .assert()
        .success()
        .stdout(predicate::str::contains("data:"));
}

// ── serve config not found ─────────────────────────────────────────────

#[test]
fn serve_config_not_found() {
    binary()
        .arg("serve")
        .arg("--config")
        .arg("/nonexistent/path/to/config.toml")
        .assert()
        .failure();
}

// ── serve invalid config ───────────────────────────────────────────────

#[test]
fn serve_invalid_config() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"this is not valid toml {").unwrap();

    binary()
        .arg("serve")
        .arg("--config")
        .arg(file.path())
        .assert()
        .failure();
}

// ── serve help ─────────────────────────────────────────────────────────

#[test]
fn serve_help_text() {
    binary()
        .arg("serve")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage"))
        .stdout(predicate::str::contains("serve"));
}
