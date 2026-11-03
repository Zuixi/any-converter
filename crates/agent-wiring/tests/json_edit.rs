#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use agent_wiring::edit::json::{del, get, set};
use agent_wiring::edit::{EditError, KV};
use serde_json::json;

fn kv(path: &str, value: serde_json::Value) -> KV {
    KV::new(path, value)
}

#[test]
fn preserves_line_and_inline_comments() {
    let input = "{\n  // keep me\n  \"a\": 1, // inline\n  /* block */\n  \"b\": 2\n}\n";
    let output = set(input, &[kv("a", json!(3))]).unwrap();
    assert!(output.contains("// keep me"));
    assert!(output.contains("// inline"));
    assert!(output.contains("/* block */"));
    assert!(output.contains("\"a\": 3"));
    assert!(output.contains("\"b\": 2"));
}

#[test]
fn preserves_key_order() {
    let input = "{\"b\": 1, \"a\": 2}";
    let output = set(input, &[kv("b", json!(9))]).unwrap();
    assert_eq!(output, "{\"b\": 9, \"a\": 2}");
}

#[test]
fn keeps_crlf_line_endings() {
    let input = "{\r\n  \"a\": 1\r\n}\r\n";
    let output = set(input, &[kv("b", json!(2))]).unwrap();
    assert!(output.contains("\"b\": 2"));
    assert!(!output.replace("\r\n", "").contains('\n'));
}

#[test]
fn preserves_bom() {
    let input = "\u{feff}{\"a\": 1}";
    let output = set(input, &[kv("a", json!(2))]).unwrap();
    assert_eq!(output, "\u{feff}{\"a\": 2}");
    assert_eq!(get(&output, "a").as_deref(), Some("2"));
}

#[test]
fn preserves_jsonc_trailing_commas() {
    let input = "{\n  \"a\": 1,\n}\n";
    let output = set(input, &[kv("a", json!(2)), kv("b", json!(3))]).unwrap();
    assert!(output.contains("\"a\": 2,"), "{output}");
    assert!(output.contains("\"b\": 3,"), "{output}");
    assert!(output.trim_end().ends_with('}'));
}

#[test]
fn nested_dot_path_get_and_set() {
    let input = "{\"env\": {\"ANTHROPIC_BASE_URL\": \"old\", \"KEEP\": 1}}";
    assert_eq!(get(input, "env.ANTHROPIC_BASE_URL").as_deref(), Some("old"));
    let output = set(input, &[kv("env.ANTHROPIC_BASE_URL", json!("new"))]).unwrap();
    assert_eq!(
        get(&output, "env.ANTHROPIC_BASE_URL").as_deref(),
        Some("new")
    );
    assert_eq!(get(&output, "env.KEEP").as_deref(), Some("1"));
}

#[test]
fn duplicate_keys_read_last_and_edit_first() {
    let input = "{\"a\": 1, \"a\": 2}";
    assert_eq!(get(input, "a").as_deref(), Some("2"));
    let output = set(input, &[kv("a", json!(3))]).unwrap();
    assert_eq!(output, "{\"a\": 3, \"a\": 2}");
}

#[test]
fn corrupt_input_is_parse_error_and_never_written() {
    let original = "{not json";
    let set_result = set(original, &[kv("a", json!(1))]);
    assert!(matches!(set_result, Err(EditError::Parse(_))));
    let del_result = del(original, &["a"]);
    assert!(matches!(del_result, Err(EditError::Parse(_))));
    assert_eq!(original, "{not json");
}

#[test]
fn empty_file_and_empty_object_accept_new_keys() {
    let expected = "{\n  \"a\": 1\n}";
    assert_eq!(set("", &[kv("a", json!(1))]).unwrap(), expected);
    assert_eq!(set("{}", &[kv("a", json!(1))]).unwrap(), expected);
    assert_eq!(set("  \r\n", &[kv("a", json!(1))]).unwrap(), expected);
}

#[test]
fn supports_all_value_types() {
    let mut text = String::from("{}");
    text = set(&text, &[kv("s", json!("hello"))]).unwrap();
    text = set(&text, &[kv("n", json!(42))]).unwrap();
    text = set(&text, &[kv("b", json!(true))]).unwrap();
    text = set(&text, &[kv("a", json!([1, "x"]))]).unwrap();
    text = set(&text, &[kv("o", json!({"k": "v"}))]).unwrap();

    assert_eq!(get(&text, "s").as_deref(), Some("hello"));
    assert_eq!(get(&text, "n").as_deref(), Some("42"));
    assert_eq!(get(&text, "b").as_deref(), Some("true"));
    assert_eq!(get(&text, "a").as_deref(), Some("[1,\"x\"]"));
    assert_eq!(get(&text, "o").as_deref(), Some("{\"k\":\"v\"}"));
    serde_json::from_str::<serde_json::Value>(&text).unwrap();
}

#[test]
fn delete_then_reset_roundtrip_is_semantically_equal() {
    let input = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    let removed = del(input, &["a"]).unwrap();
    let restored = set(&removed, &[kv("a", json!(1))]).unwrap();
    let before: serde_json::Value = serde_json::from_str(input).unwrap();
    let after: serde_json::Value = serde_json::from_str(&restored).unwrap();
    assert_eq!(before, after);
}

#[test]
fn appends_missing_nested_path_with_matching_indent() {
    let input = "{}\n";
    let output = set(input, &[kv("env.ANTHROPIC_BASE_URL", json!("http://gw"))]).unwrap();
    assert_eq!(
        output,
        "{\n  \"env\": {\n    \"ANTHROPIC_BASE_URL\": \"http://gw\"\n  }\n}\n"
    );
}

#[test]
fn new_key_uses_existing_indent() {
    let input = "{\n    \"a\": 1\n}\n";
    let output = set(input, &[kv("b", json!(2))]).unwrap();
    assert!(output.contains("\n    \"b\": 2"), "{output}");
}

#[test]
fn deleting_missing_key_is_a_noop() {
    let input = "{\n  \"a\": 1\n}\n";
    assert_eq!(del(input, &["missing"]).unwrap(), input);
}

#[test]
fn delete_preserves_comments_and_other_keys() {
    let input = "{\n  // top\n  \"a\": 1,\n  \"b\": 2, // b comment\n  \"c\": 3\n}\n";
    let output = del(input, &["b"]).unwrap();
    assert!(output.contains("// top"));
    assert!(!output.contains("b comment"));
    assert!(output.contains("\"a\": 1"));
    assert!(output.contains("\"c\": 3"));
    assert_eq!(get(&output, "a").as_deref(), Some("1"));
    assert_eq!(get(&output, "c").as_deref(), Some("3"));
    assert_eq!(get(&output, "b"), None);
}

#[test]
fn array_index_lookup_and_missing_paths() {
    let input = "{\"a\": [10, 20]}";
    assert_eq!(get(input, "a.1").as_deref(), Some("20"));
    assert_eq!(get(input, "a.5"), None);
    assert_eq!(get(input, "missing"), None);
}

#[test]
fn delete_keeps_comment_before_following_key() {
    let input = "{\n  \"a\": 1,\n  \"b\": 2,\n  // describes c\n  \"c\": 3\n}\n";
    let output = del(input, &["b"]).unwrap();
    assert!(output.contains("// describes c"), "{output}");
    assert_eq!(get(&output, "a").as_deref(), Some("1"));
    assert_eq!(get(&output, "c").as_deref(), Some("3"));
}

#[test]
fn deleting_last_key_keeps_trailing_comma_style() {
    let input = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    let output = del(input, &["b"]).unwrap();
    assert_eq!(output, "{\n  \"a\": 1\n}\n");
}
