#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use agent_wiring::edit::KV;
use agent_wiring::edit::env::{del, get, set};
use serde_json::json;

fn kv(path: &str, value: serde_json::Value) -> KV {
    KV::new(path, value)
}

#[test]
fn preserves_comments_blank_lines_and_inline_comments() {
    let input = "# top\n\nA=1 # inline\n\nB=2\n";
    let output = set(input, &[kv("A", json!(9))]);
    assert!(output.contains("# top"));
    assert!(output.contains("# inline"));
    assert!(output.contains("\n\nB=2"));
    assert!(output.contains("A=9"));
}

#[test]
fn preserves_export_prefix() {
    let input = "export A=1\n";
    let output = set(input, &[kv("A", json!(2))]);
    assert_eq!(output, "export A=2\n");
}

#[test]
fn preserves_quote_style_of_untouched_lines() {
    let input = "A=\"one\"\nB='two'\n";
    let output = set(input, &[kv("A", json!("three"))]);
    assert!(output.contains("A=\"three\""));
    assert!(output.contains("B='two'"));
}

#[test]
fn changed_key_keeps_its_line_position() {
    let input = "A=1\nB=2\nC=3\n";
    let output = set(input, &[kv("B", json!(9))]);
    assert_eq!(output, "A=1\nB=9\nC=3\n");
}

#[test]
fn new_keys_are_appended() {
    let input = "A=1\n";
    let output = set(input, &[kv("B", json!(2))]);
    assert_eq!(output, "A=1\nB=2\n");
}

#[test]
fn reads_plain_exported_and_quoted_values() {
    let input = "export A='x'\nB=\"y\"\nC=plain\n";
    assert_eq!(get(input, "A").as_deref(), Some("x"));
    assert_eq!(get(input, "B").as_deref(), Some("y"));
    assert_eq!(get(input, "C").as_deref(), Some("plain"));
    assert_eq!(get(input, "missing"), None);
}

#[test]
fn duplicate_keys_read_last_and_write_last() {
    let input = "A=1\nA=2\n";
    assert_eq!(get(input, "A").as_deref(), Some("2"));
    let output = set(input, &[kv("A", json!(3))]);
    assert_eq!(output, "A=1\nA=3\n");
    assert_eq!(get(&output, "A").as_deref(), Some("3"));
}

#[test]
fn delete_removes_assignment_and_keeps_comments() {
    let input = "# c\n\nA=1\nB=2\n";
    let output = del(input, &["A"]);
    assert_eq!(output, "# c\n\nB=2\n");
}

#[test]
fn deleting_missing_key_is_a_noop() {
    let input = "A=1\n";
    assert_eq!(del(input, &["missing"]), input);
}

#[test]
fn keeps_crlf_line_endings() {
    let input = "A=1\r\n";
    let output = set(input, &[kv("A", json!(2)), kv("B", json!(3))]);
    assert!(output.contains("A=2"));
    assert!(output.contains("B=3"));
    assert!(!output.replace("\r\n", "").contains('\n'), "{output:?}");
}

#[test]
fn preserves_bom() {
    let input = "\u{feff}A=1\n";
    let output = set(input, &[kv("A", json!(2))]);
    assert!(output.starts_with('\u{feff}'));
    assert_eq!(get(&output, "A").as_deref(), Some("2"));
}

#[test]
fn supports_value_types() {
    let mut text = String::new();
    text = set(&text, &[kv("s", json!("hello"))]);
    text = set(&text, &[kv("n", json!(42))]);
    text = set(&text, &[kv("b", json!(true))]);
    text = set(&text, &[kv("a", json!([1, 2]))]);
    text = set(&text, &[kv("o", json!({"k": "v"}))]);

    assert_eq!(get(&text, "s").as_deref(), Some("hello"));
    assert_eq!(get(&text, "n").as_deref(), Some("42"));
    assert_eq!(get(&text, "b").as_deref(), Some("true"));
    assert_eq!(get(&text, "a").as_deref(), Some("[1,2]"));
    assert_eq!(get(&text, "o").as_deref(), Some("{\"k\":\"v\"}"));
}

#[test]
fn empty_file_accepts_new_key() {
    assert_eq!(set("", &[kv("A", json!(1))]), "A=1\n");
}

#[test]
fn appends_after_file_without_trailing_newline() {
    let output = set("A=1", &[kv("B", json!(2))]);
    assert_eq!(output, "A=1\nB=2\n");
}

#[test]
fn unquoted_inline_comment_survives_change() {
    let input = "A=old # keep me\n";
    let output = set(input, &[kv("A", json!("new"))]);
    assert_eq!(output, "A=new # keep me\n");
}

#[test]
fn delete_removes_every_duplicate_assignment() {
    let input = "A=1\nB=2\nA=3\n";
    assert_eq!(del(input, &["A"]), "B=2\n");
}

#[test]
fn values_with_spaces_are_quoted_and_roundtrip() {
    let output = set("", &[kv("A", json!("hello world"))]);
    assert!(output.contains("A=\"hello world\""), "{output}");
    assert_eq!(get(&output, "A").as_deref(), Some("hello world"));
}

#[test]
fn changing_single_quoted_value_keeps_single_quotes() {
    let input = "A='old'\n";
    let output = set(input, &[kv("A", json!("new"))]);
    assert_eq!(output, "A='new'\n");
}
