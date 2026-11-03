//! Format-preserving dotenv editor.
//!
//! Comments, blank lines, `export ` prefixes, quote styles on untouched lines,
//! CRLF line endings, and a UTF-8 BOM are preserved. Changed keys stay at their
//! original line position; new keys are appended.

use serde_json::Value;

use super::{KV, split_bom};

/// Reads a variable from dotenv text.
///
/// The last assignment wins when a key is duplicated. Quoted values are
/// returned unquoted. Returns `None` when the key is absent.
///
/// # Examples
///
/// ```
/// let text = "export TOKEN='abc'\n";
/// assert_eq!(agent_wiring::edit::env::get(text, "TOKEN").as_deref(), Some("abc"));
/// ```
pub fn get(original: &str, key: &str) -> Option<String> {
    let (_, body) = split_bom(original);
    let mut result = None;
    for line in body.split_inclusive('\n') {
        let (content, _) = split_line_end(line);
        if let Some(assignment) = parse_line(content) {
            if assignment.key == key {
                result = Some(value_from_assignment(content, &assignment));
            }
        }
    }
    result
}

/// Writes one or more variables into dotenv text.
///
/// Existing assignments keep their line position, indentation, `export `
/// prefix, and quote style. New variables are appended using the file's line
/// ending style.
pub fn set(original: &str, kvs: &[KV]) -> String {
    let (bom, body) = split_bom(original);
    let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = body.split_inclusive('\n').map(str::to_string).collect();

    for kv in kvs {
        let raw = kv_raw_value(&kv.value);
        let mut replaced = false;

        for line in lines.iter_mut().rev() {
            let (content, ending) = split_line_end(line);
            let Some(assignment) = parse_line(content) else {
                continue;
            };
            if assignment.key != kv.path {
                continue;
            }
            let new_content = replace_value(content, &assignment, &raw);
            *line = format!("{new_content}{ending}");
            replaced = true;
            break;
        }

        if !replaced {
            if let Some(last) = lines.last_mut() {
                if !last.ends_with('\n') {
                    last.push_str(newline);
                }
            }
            lines.push(format!("{}={}{newline}", kv.path, quote_env(&raw)));
        }
    }

    format!("{bom}{}", lines.concat())
}

/// Removes variables from dotenv text.
///
/// Every assignment of a listed key is removed. Missing keys are ignored.
/// Comments and blank lines are preserved.
pub fn del(original: &str, keys: &[&str]) -> String {
    let (bom, body) = split_bom(original);
    let mut output = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let (content, _) = split_line_end(line);
        let remove = parse_line(content)
            .map(|assignment| keys.contains(&assignment.key))
            .unwrap_or(false);
        if !remove {
            output.push_str(line);
        }
    }
    format!("{bom}{output}")
}

fn split_line_end(line: &str) -> (&str, &str) {
    if let Some(stripped) = line.strip_suffix('\n') {
        if let Some(content) = stripped.strip_suffix('\r') {
            (content, "\r\n")
        } else {
            (stripped, "\n")
        }
    } else {
        (line, "")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

#[derive(Debug)]
struct Assignment<'a> {
    key: &'a str,
    value_span: (usize, usize),
    quote: Quote,
}

fn parse_line(content: &str) -> Option<Assignment<'_>> {
    let bytes = content.as_bytes();
    let mut index = 0;

    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] == b'#' {
        return None;
    }

    if content[index..].starts_with("export") {
        let after = index + "export".len();
        if after < bytes.len() && matches!(bytes[after], b' ' | b'\t') {
            index = after;
            while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
                index += 1;
            }
        }
    }

    let key_start = index;
    while index < bytes.len() && !matches!(bytes[index], b'=' | b' ' | b'\t' | b'#' | b'\r') {
        index += 1;
    }
    if index == key_start {
        return None;
    }
    let key = &content[key_start..index];

    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b'=' {
        return None;
    }
    index += 1;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }

    let value_start = index;
    let (quote, value_end) = if index < bytes.len() && bytes[index] == b'"' {
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index = (index + 2).min(bytes.len());
            } else if bytes[index] == b'"' {
                index += 1;
                break;
            } else {
                index += 1;
            }
        }
        (Quote::Double, index.min(bytes.len()))
    } else if index < bytes.len() && bytes[index] == b'\'' {
        index += 1;
        while index < bytes.len() && bytes[index] != b'\'' {
            index += 1;
        }
        if index < bytes.len() {
            index += 1;
        }
        (Quote::Single, index.min(bytes.len()))
    } else {
        let mut end = index;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'#' && (index == value_start || matches!(bytes[index - 1], b' ' | b'\t')) {
                break;
            }
            if !matches!(byte, b' ' | b'\t') {
                end = index + 1;
            }
            index += 1;
        }
        (Quote::None, end)
    };

    Some(Assignment {
        key,
        value_span: (value_start, value_end),
        quote,
    })
}

fn value_from_assignment(content: &str, assignment: &Assignment<'_>) -> String {
    let raw = &content[assignment.value_span.0..assignment.value_span.1];
    match assignment.quote {
        Quote::Double => {
            let inner = raw
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(raw);
            unescape_double(inner)
        }
        Quote::Single => raw
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
            .unwrap_or(raw)
            .to_string(),
        Quote::None => raw.to_string(),
    }
}

fn replace_value(content: &str, assignment: &Assignment<'_>, raw: &str) -> String {
    let value = match assignment.quote {
        Quote::Double => format!("\"{}\"", escape_double(raw)),
        Quote::Single => {
            if raw.contains('\'') || raw.contains('\n') || raw.contains('\r') {
                format!("\"{}\"", escape_double(raw))
            } else {
                format!("'{raw}'")
            }
        }
        Quote::None => quote_env(raw),
    };

    let mut output = String::with_capacity(content.len() + value.len());
    output.push_str(&content[..assignment.value_span.0]);
    output.push_str(&value);
    output.push_str(&content[assignment.value_span.1..]);
    output
}

fn kv_raw_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn quote_env(raw: &str) -> String {
    let needs_quotes = raw.is_empty()
        || raw
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '#' | '"' | '\'' | '\\'));
    if needs_quotes {
        format!("\"{}\"", escape_double(raw))
    } else {
        raw.to_string()
    }
}

fn escape_double(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output
}

fn unescape_double(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some('"') => output.push('"'),
            Some('\'') => output.push('\''),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}
