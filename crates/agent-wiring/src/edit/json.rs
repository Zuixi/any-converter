//! Format-preserving JSON and JSONC editor.
//!
//! Parsing is done on a comment/trailing-comma stripped copy of the input, but
//! edits are applied as string surgery on the original text. That keeps line
//! comments, block comments, key order, indentation, CRLF line endings, a
//! UTF-8 BOM, and JSONC trailing commas intact.

use super::{EditError, KV, split_bom};
use serde_json::Value;

/// Reads a value from JSON or JSONC text using a dot-separated path.
///
/// String values are returned without their quotes; every other value is
/// returned as compact JSON. Returns `None` for malformed input or a missing
/// path.
///
/// # Examples
///
/// ```
/// let text = r#"{"env": {"A": "one"}}"#;
/// assert_eq!(agent_wiring::edit::json::get(text, "env.A").as_deref(), Some("one"));
/// ```
pub fn get(path_bytes: &str, key_path: &str) -> Option<String> {
    let value = parse_json(path_bytes).ok()?;
    let mut current = &value;
    for segment in key_path.split('.').filter(|segment| !segment.is_empty()) {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    match current {
        Value::String(text) => Some(text.clone()),
        other => serde_json::to_string(other).ok(),
    }
}

/// Writes one or more values into JSON or JSONC text, preserving formatting.
///
/// New keys are appended before the closing brace using the document's
/// existing indentation. Missing intermediate objects are created.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid JSONC or a
/// path segment that should be an object is a scalar.
pub fn set(original: &str, kvs: &[KV]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    if kvs.is_empty() {
        parse_json(body)?;
        return Ok(original.to_string());
    }

    let normalized = if strip_jsonc(body)?.trim().is_empty() {
        "{}"
    } else {
        body
    };
    parse_json(normalized)?;

    let mut text = normalized.to_string();
    for kv in kvs {
        text = set_one(&text, kv)?;
    }
    Ok(format!("{bom}{text}"))
}

/// Deletes one or more keys from JSON or JSONC text.
///
/// Deleting a key that does not exist is a successful no-op. Comments and the
/// formatting of untouched members are preserved.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid JSONC.
pub fn del(original: &str, keys: &[&str]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    if strip_jsonc(body)?.trim().is_empty() {
        return Ok(original.to_string());
    }
    parse_json(body)?;

    let mut text = body.to_string();
    for key in keys {
        text = del_one(&text, key)?;
    }
    Ok(format!("{bom}{text}"))
}

fn parse_json(src: &str) -> Result<Value, EditError> {
    let (_, body) = split_bom(src);
    let stripped = strip_jsonc(body)?;
    serde_json::from_str(&stripped).map_err(|error| EditError::Parse(error.to_string()))
}

fn set_one(text: &str, kv: &KV) -> Result<String, EditError> {
    parse_json(text)?;
    let segments: Vec<&str> = kv
        .path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return Err(EditError::Parse("empty key path".into()));
    }

    let mut parser = Parser::new(text);
    let root = parser.parse_document()?;
    if !matches!(root.kind, Kind::Object(_)) {
        return Err(EditError::Parse("JSON root is not an object".into()));
    }

    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let unit = detect_indent_unit(text);
    edit_object(text, &root, &segments, &kv.value, newline, &unit)
}

fn del_one(text: &str, key_path: &str) -> Result<String, EditError> {
    parse_json(text)?;
    let segments: Vec<&str> = key_path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return Err(EditError::Parse("empty key path".into()));
    }

    let mut parser = Parser::new(text);
    let root = parser.parse_document()?;
    if !matches!(root.kind, Kind::Object(_)) {
        return Err(EditError::Parse("JSON root is not an object".into()));
    }
    Ok(del_path(text, &root, &segments))
}

fn edit_object(
    text: &str,
    node: &Node,
    segments: &[&str],
    value: &Value,
    newline: &str,
    unit: &str,
) -> Result<String, EditError> {
    let members = match &node.kind {
        Kind::Object(members) => members,
        _ => return Err(EditError::Parse("expected a JSON object".into())),
    };
    let key = segments[0];
    let existing = members.iter().find(|member| member.key == key);

    if segments.len() == 1 {
        if let Some(member) = existing {
            let serialized = serialize(value)?;
            if text[member.value.start..member.value.end] == serialized {
                return Ok(text.to_string());
            }
            return Ok(splice(
                text,
                member.value.start,
                member.value.end,
                &serialized,
            ));
        }
        let indent = member_indent(text, node, unit);
        let member_text = build_member(key, &[], value, &indent, newline, unit)?;
        return Ok(insert_member(
            text,
            node,
            members,
            &member_text,
            newline,
            unit,
        ));
    }

    if let Some(member) = existing {
        if matches!(member.value.kind, Kind::Object(_)) {
            return edit_object(text, &member.value, &segments[1..], value, newline, unit);
        }
        return Err(EditError::Parse(format!(
            "key `{key}` is not a JSON object"
        )));
    }

    let indent = member_indent(text, node, unit);
    let member_text = build_member(key, &segments[1..], value, &indent, newline, unit)?;
    Ok(insert_member(
        text,
        node,
        members,
        &member_text,
        newline,
        unit,
    ))
}

fn build_member(
    key: &str,
    tail: &[&str],
    value: &Value,
    indent: &str,
    newline: &str,
    unit: &str,
) -> Result<String, EditError> {
    let key_text =
        serde_json::to_string(key).map_err(|error| EditError::Parse(error.to_string()))?;
    if tail.is_empty() {
        return Ok(format!("{key_text}: {}", serialize(value)?));
    }
    let child_indent = format!("{indent}{unit}");
    let inner = build_member(tail[0], &tail[1..], value, &child_indent, newline, unit)?;
    Ok(format!(
        "{key_text}: {{{newline}{child_indent}{inner}{newline}{indent}}}"
    ))
}

fn serialize(value: &Value) -> Result<String, EditError> {
    serde_json::to_string(value).map_err(|error| EditError::Parse(error.to_string()))
}

fn insert_member(
    text: &str,
    node: &Node,
    members: &[Member],
    member_text: &str,
    newline: &str,
    unit: &str,
) -> String {
    let indent = member_indent(text, node, unit);
    let close = node.end.saturating_sub(1);
    let close_line = line_start(text, close);
    let close_indent = line_indent(text, close);

    if let Some(last) = members.last() {
        let after_last = next_significant(text, last.value.end);
        let trailing_comma = text.as_bytes().get(after_last) == Some(&b',');
        let comma = if trailing_comma { "," } else { "" };

        if text[close_line..close].trim().is_empty() {
            let mut result = splice(
                text,
                close_line,
                close_line,
                &format!("{indent}{member_text}{comma}{newline}"),
            );
            if !trailing_comma {
                result = splice(&result, last.value.end, last.value.end, ",");
            }
            return result;
        }

        let mut insert = close;
        while insert > close_line {
            match text.as_bytes()[insert - 1] {
                b' ' | b'\t' | b'\r' => insert -= 1,
                _ => break,
            }
        }
        let prefix = if insert == 0 || text.as_bytes()[insert - 1] == b'\n' {
            ""
        } else {
            newline
        };
        let mut result = splice(
            text,
            insert,
            close,
            &format!("{prefix}{indent}{member_text}{comma}{newline}{close_indent}"),
        );
        if !trailing_comma {
            result = splice(&result, last.value.end, last.value.end, ",");
        }
        return result;
    }

    let mut insert = close;
    while insert > close_line {
        match text.as_bytes()[insert - 1] {
            b' ' | b'\t' | b'\r' => insert -= 1,
            _ => break,
        }
    }
    let prefix = if insert == 0 || text.as_bytes()[insert - 1] == b'\n' {
        ""
    } else {
        newline
    };
    splice(
        text,
        insert,
        close,
        &format!("{prefix}{indent}{member_text}{newline}{close_indent}"),
    )
}

fn del_path(text: &str, node: &Node, segments: &[&str]) -> String {
    let members = match &node.kind {
        Kind::Object(members) => members,
        _ => return text.to_string(),
    };
    let key = segments[0];
    let index = members.iter().position(|member| member.key == key);

    if segments.len() == 1 {
        if let Some(index) = index {
            return remove_member(text, node, members, index);
        }
        return text.to_string();
    }

    if let Some(member) = index.map(|index| &members[index]) {
        if matches!(member.value.kind, Kind::Object(_)) {
            return del_path(text, &member.value, &segments[1..]);
        }
    }
    text.to_string()
}

fn remove_member(text: &str, node: &Node, members: &[Member], index: usize) -> String {
    let member = &members[index];

    if members.len() == 1 {
        let interior = &text[node.start + 1..node.end - 1];
        if !interior.contains("//") && !interior.contains("/*") {
            return splice(text, node.start, node.end, "{}");
        }
        let line = line_start(text, member.key_start);
        let start = if line > node.start && text[line..member.key_start].trim().is_empty() {
            line
        } else {
            member.key_start
        };
        let mut end = member_tail_end(text, member.value.end);
        if start == line {
            end = extend_line_end(text, end);
        }
        return splice(text, start, end, "");
    }

    if let Some(next) = members.get(index + 1) {
        let line = line_start(text, member.key_start);
        let on_own_line = line > node.start && text[line..member.key_start].trim().is_empty();
        if on_own_line {
            let end = extend_line_end(text, member_tail_end(text, member.value.end));
            return splice(text, line, end, "");
        }
        return splice(text, member.key_start, next.key_start, "");
    }

    if let Some(previous) = index.checked_sub(1).and_then(|index| members.get(index)) {
        let after = next_significant(text, member.value.end);
        let had_trailing_comma = text.as_bytes().get(after) == Some(&b',');
        let end = member_tail_end(text, member.value.end);
        let mut result = splice(text, previous.value.end, end, "");
        if had_trailing_comma {
            result = splice(&result, previous.value.end, previous.value.end, ",");
        }
        return result;
    }

    text.to_string()
}

fn member_tail_end(text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    if pos < bytes.len() && bytes[pos] == b',' {
        pos += 1;
    }
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'/' {
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
    } else if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
        pos += 2;
        while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
            pos += 1;
        }
        if pos + 1 < bytes.len() {
            pos += 2;
        }
    }
    pos
}

fn extend_line_end(text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    if pos < bytes.len() && bytes[pos] == b'\r' {
        pos += 1;
    }
    if pos < bytes.len() && bytes[pos] == b'\n' {
        pos += 1;
    }
    pos
}

fn next_significant(text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'/' {
            pos += 2;
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
            pos += 2;
            while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                pos += 1;
            }
            pos = (pos + 2).min(bytes.len());
            continue;
        }
        return pos;
    }
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |index| index + 1)
}

fn line_indent(text: &str, pos: usize) -> String {
    let start = line_start(text, pos);
    text[start..pos]
        .chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .collect()
}

fn member_indent(text: &str, node: &Node, unit: &str) -> String {
    if let Kind::Object(members) = &node.kind {
        if let Some(first) = members.first() {
            if text[node.start..first.key_start].contains('\n') {
                return line_indent(text, first.key_start);
            }
        }
    }
    format!("{}{}", line_indent(text, node.start), unit)
}

fn detect_indent_unit(text: &str) -> String {
    let mut previous: Option<usize> = None;
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.trim().is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if let Some(previous_indent) = previous {
            if indent > previous_indent {
                return line[previous_indent..indent].to_string();
            }
        }
        previous = Some(indent);
    }
    "  ".to_string()
}

fn splice(text: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut output = String::with_capacity(text.len() + replacement.len());
    output.push_str(&text[..start]);
    output.push_str(replacement);
    output.push_str(&text[end..]);
    output
}

fn strip_jsonc(src: &str) -> Result<String, EditError> {
    let bytes = src.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
        } else if byte == b'"' {
            in_string = true;
            output.push(byte);
            index += 1;
        } else if byte == b'/' && index + 1 < bytes.len() && bytes[index + 1] == b'/' {
            output.push(b' ');
            output.push(b' ');
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(if bytes[index] == b'\r' { b'\r' } else { b' ' });
                index += 1;
            }
        } else if byte == b'/' && index + 1 < bytes.len() && bytes[index + 1] == b'*' {
            output.push(b' ');
            output.push(b' ');
            index += 2;
            while index < bytes.len() {
                if bytes[index] == b'*' && index + 1 < bytes.len() && bytes[index + 1] == b'/' {
                    output.push(b' ');
                    output.push(b' ');
                    index += 2;
                    break;
                }
                output.push(if bytes[index] == b'\n' || bytes[index] == b'\r' {
                    bytes[index]
                } else {
                    b' '
                });
                index += 1;
            }
        } else {
            output.push(byte);
            index += 1;
        }
    }

    strip_trailing_commas(&output)
}

fn strip_trailing_commas(bytes: &[u8]) -> Result<String, EditError> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
        } else if byte == b'"' {
            in_string = true;
            output.push(byte);
            index += 1;
        } else if byte == b',' {
            let mut lookahead = index + 1;
            while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
                lookahead += 1;
            }
            if lookahead < bytes.len() && matches!(bytes[lookahead], b'}' | b']') {
                output.push(b' ');
            } else {
                output.push(byte);
            }
            index += 1;
        } else {
            output.push(byte);
            index += 1;
        }
    }

    String::from_utf8(output).map_err(|error| EditError::Parse(error.to_string()))
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    start: usize,
    end: usize,
}
#[derive(Debug)]
enum Kind {
    Object(Vec<Member>),
    Array,
    Scalar,
}
#[derive(Debug)]
struct Member {
    key: String,
    key_start: usize,
    value: Node,
}
struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn parse_document(&mut self) -> Result<Node, EditError> {
        self.skip_trivia();
        if self.pos >= self.bytes.len() {
            return Err(EditError::Parse("empty JSON document".into()));
        }
        let node = self.parse_value()?;
        self.skip_trivia();
        if self.pos != self.bytes.len() {
            return Err(self.error("trailing data after JSON document"));
        }
        Ok(node)
    }

    fn parse_value(&mut self) -> Result<Node, EditError> {
        self.skip_trivia();
        let start = self.pos;
        if start >= self.bytes.len() {
            return Err(self.error("unexpected end of JSON input"));
        }
        match self.bytes[start] {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => {
                self.scan_string()?;
                Ok(Node {
                    kind: Kind::Scalar,
                    start,
                    end: self.pos,
                })
            }
            _ => {
                while self.pos < self.bytes.len() {
                    let byte = self.bytes[self.pos];
                    if byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']' | b'/') {
                        break;
                    }
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(self.error("unexpected character in JSON value"));
                }
                Ok(Node {
                    kind: Kind::Scalar,
                    start,
                    end: self.pos,
                })
            }
        }
    }

    fn parse_object(&mut self) -> Result<Node, EditError> {
        let start = self.pos;
        self.pos += 1;
        let mut members = Vec::new();

        loop {
            self.skip_trivia();
            if self.pos >= self.bytes.len() {
                return Err(self.error("unterminated JSON object"));
            }
            match self.bytes[self.pos] {
                b'}' => {
                    self.pos += 1;
                    break;
                }
                b',' => {
                    self.pos += 1;
                }
                b'"' => {
                    let key_start = self.pos;
                    self.scan_string()?;
                    let raw_key = &self.src[key_start..self.pos];
                    let key: String = serde_json::from_str(raw_key)
                        .map_err(|error| EditError::Parse(error.to_string()))?;
                    self.skip_trivia();
                    if self.pos >= self.bytes.len() || self.bytes[self.pos] != b':' {
                        return Err(self.error("expected ':' after JSON object key"));
                    }
                    self.pos += 1;
                    let value = self.parse_value()?;
                    members.push(Member {
                        key,
                        key_start,
                        value,
                    });
                    self.skip_trivia();
                    if self.pos < self.bytes.len() && self.bytes[self.pos] == b',' {
                        self.pos += 1;
                    }
                }
                _ => return Err(self.error("expected a JSON object key")),
            }
        }

        Ok(Node {
            kind: Kind::Object(members),
            start,
            end: self.pos,
        })
    }

    fn parse_array(&mut self) -> Result<Node, EditError> {
        let start = self.pos;
        self.pos += 1;

        loop {
            self.skip_trivia();
            if self.pos >= self.bytes.len() {
                return Err(self.error("unterminated JSON array"));
            }
            match self.bytes[self.pos] {
                b']' => {
                    self.pos += 1;
                    break;
                }
                b',' => {
                    self.pos += 1;
                }
                _ => {
                    self.parse_value()?;
                    self.skip_trivia();
                    if self.pos < self.bytes.len() && self.bytes[self.pos] == b',' {
                        self.pos += 1;
                    }
                }
            }
        }

        Ok(Node {
            kind: Kind::Array,
            start,
            end: self.pos,
        })
    }

    fn scan_string(&mut self) -> Result<(), EditError> {
        self.pos += 1;
        let mut escaped = false;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                self.pos += 1;
                return Ok(());
            }
            self.pos += 1;
        }
        Err(self.error("unterminated JSON string"))
    }

    fn skip_trivia(&mut self) {
        let bytes = self.bytes;
        loop {
            while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos + 1 < bytes.len() && bytes[self.pos] == b'/' {
                if bytes[self.pos + 1] == b'/' {
                    self.pos += 2;
                    while self.pos < bytes.len() && bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    continue;
                }
                if bytes[self.pos + 1] == b'*' {
                    self.pos += 2;
                    while self.pos + 1 < bytes.len()
                        && !(bytes[self.pos] == b'*' && bytes[self.pos + 1] == b'/')
                    {
                        self.pos += 1;
                    }
                    if self.pos + 1 < bytes.len() {
                        self.pos += 2;
                    }
                    continue;
                }
            }
            return;
        }
    }

    fn error(&self, message: &str) -> EditError {
        EditError::Parse(format!("{message} at byte {}", self.pos))
    }
}
