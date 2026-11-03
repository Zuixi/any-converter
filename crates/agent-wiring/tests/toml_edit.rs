#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use agent_wiring::edit::toml::{del, get, set, set_table};
use agent_wiring::edit::{EditError, KV};
use serde_json::json;

fn kv(path: &str, value: serde_json::Value) -> KV {
    KV::new(path, value)
}

#[test]
fn preserves_comments_when_replacing() {
    let input = "# top comment\n[model]\nname = \"old\" # inline\nother = 1\n";
    let output = set(input, &[kv("model.name", json!("new"))]).unwrap();
    assert!(output.contains("# top comment"));
    assert!(output.contains("# inline"));
    assert!(output.contains("name = \"new\""));
    assert!(output.contains("other = 1"));
}

#[test]
fn preserves_table_order() {
    let input = "[a]\nx = 1\n\n[b]\ny = 2\n";
    let output = set(input, &[kv("a.x", json!(9))]).unwrap();
    assert!(output.find("[a]").unwrap() < output.find("[b]").unwrap());
}

#[test]
fn keeps_crlf_line_endings() {
    let input = "[a]\r\nx = 1\r\n";
    let output = set(input, &[kv("a.x", json!(2))]).unwrap();
    assert_eq!(get(&output, "a.x").as_deref(), Some("2"));
    assert!(!output.replace("\r\n", "").contains('\n'), "{output:?}");
}

#[test]
fn preserves_bom() {
    let input = "\u{feff}[a]\nx = 1\n";
    let output = set(input, &[kv("a.x", json!(2))]).unwrap();
    assert!(output.starts_with('\u{feff}'));
    assert_eq!(get(&output, "a.x").as_deref(), Some("2"));
}

#[test]
fn reads_top_level_and_nested_keys() {
    let input = "model = \"gpt\"\n[a]\nb = 1\n";
    assert_eq!(get(input, "model").as_deref(), Some("gpt"));
    assert_eq!(get(input, "a.b").as_deref(), Some("1"));
    assert_eq!(get(input, "a.missing"), None);
}

#[test]
fn set_creates_missing_tables() {
    let output = set("", &[kv("a.b", json!(1))]).unwrap();
    assert!(output.contains("[a]"), "{output}");
    assert!(output.contains("b = 1"), "{output}");
    assert_eq!(get(&output, "a.b").as_deref(), Some("1"));
}

#[test]
fn set_table_creates_nested_table() {
    let output = set_table(
        "",
        "model_providers.any-converter",
        &[
            kv("name", json!("any-converter")),
            kv("base_url", json!("http://gw/v1")),
            kv("requires_openai_auth", json!(true)),
        ],
    )
    .unwrap();

    assert!(
        output.contains("[model_providers.any-converter]"),
        "{output}"
    );
    assert_eq!(
        get(&output, "model_providers.any-converter.name").as_deref(),
        Some("any-converter")
    );
    assert_eq!(
        get(&output, "model_providers.any-converter.base_url").as_deref(),
        Some("http://gw/v1")
    );
    assert_eq!(
        get(
            &output,
            "model_providers.any-converter.requires_openai_auth"
        )
        .as_deref(),
        Some("true")
    );
}

#[test]
fn set_table_adds_keys_to_existing_table() {
    let input = "[model]\nname = \"old\"\n";
    let output = set_table(input, "model", &[kv("model", json!("gpt"))]).unwrap();
    assert!(output.contains("name = \"old\""));
    assert_eq!(get(&output, "model.model").as_deref(), Some("gpt"));
}

#[test]
fn delete_removes_key_and_keeps_comments() {
    let input = "# keep\n[a]\nx = 1\ny = 2\n";
    let output = del(input, &["a.y"]).unwrap();
    assert!(output.contains("# keep"));
    assert_eq!(get(&output, "a.x").as_deref(), Some("1"));
    assert_eq!(get(&output, "a.y"), None);
}

#[test]
fn delete_removes_whole_table() {
    let input = "[a]\nx = 1\n[b]\ny = 2\n";
    let output = del(input, &["a"]).unwrap();
    assert!(!output.contains("[a]"), "{output}");
    assert_eq!(get(&output, "b.y").as_deref(), Some("2"));
}

#[test]
fn deleting_missing_key_is_a_noop() {
    let input = "[a]\nx = 1\n";
    assert_eq!(del(input, &["a.missing"]).unwrap(), input);
}

#[test]
fn supports_value_types() {
    let mut text = String::new();
    text = set(&text, &[kv("s", json!("hello"))]).unwrap();
    text = set(&text, &[kv("i", json!(42))]).unwrap();
    text = set(&text, &[kv("f", json!(1.5))]).unwrap();
    text = set(&text, &[kv("b", json!(true))]).unwrap();
    text = set(&text, &[kv("a", json!([1, 2]))]).unwrap();
    text = set(&text, &[kv("o", json!({"k": "v"}))]).unwrap();

    assert_eq!(get(&text, "s").as_deref(), Some("hello"));
    assert_eq!(get(&text, "i").as_deref(), Some("42"));
    assert_eq!(get(&text, "f").as_deref(), Some("1.5"));
    assert_eq!(get(&text, "b").as_deref(), Some("true"));
    assert_eq!(get(&text, "a").as_deref(), Some("[1, 2]"));
    assert_eq!(get(&text, "o.k").as_deref(), Some("v"));
}

#[test]
fn empty_input_accepts_top_level_key() {
    let output = set("", &[kv("model", json!("gpt"))]).unwrap();
    assert_eq!(get(&output, "model").as_deref(), Some("gpt"));
}

#[test]
fn delete_then_reset_roundtrip() {
    let input = "[a]\nx = 1\ny = 2\n";
    let removed = del(input, &["a.y"]).unwrap();
    let restored = set(&removed, &[kv("a.y", json!(2))]).unwrap();
    assert_eq!(restored, input);
}

#[test]
fn reads_inline_tables() {
    let input = "[a]\nheaders = { x = 1, y = \"z\" }\n";
    assert_eq!(get(input, "a.headers.x").as_deref(), Some("1"));
    assert_eq!(get(input, "a.headers.y").as_deref(), Some("z"));
}

#[test]
fn corrupt_input_is_parse_error() {
    let original = "[a\nx = ";
    let result = set(original, &[kv("a.x", json!(1))]);
    assert!(matches!(result, Err(EditError::Parse(_))));
    let del_result = del(original, &["a.x"]);
    assert!(matches!(del_result, Err(EditError::Parse(_))));
}

#[test]
fn replacement_keeps_inline_comment_after_value() {
    let input = "[a]\nx = 1 # keep\n";
    let output = set(input, &[kv("a.x", json!(2))]).unwrap();
    assert!(output.contains("x = 2"), "{output}");
    assert!(output.contains("# keep"), "{output}");
}

#[test]
fn new_key_is_appended_to_existing_table() {
    let input = "[a]\nx = 1\n";
    let output = set(input, &[kv("a.y", json!(2))]).unwrap();
    let x = output.find("x = 1").unwrap();
    let y = output.find("y = 2").unwrap();
    assert!(x < y);
}

#[test]
fn null_value_is_rejected() {
    let result = set("[a]\nx = 1\n", &[kv("a.x", json!(null))]);
    assert!(matches!(result, Err(EditError::Parse(_))));
}
