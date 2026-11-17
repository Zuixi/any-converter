//! Format-preserving YAML editor.
//!
//! Edits go through `yaml-edit`'s lossless syntax tree, so comments, blank
//! lines, quoting style, and key order survive a connect/disconnect cycle.
//! Paths use dots, mirroring the JSON editor; a segment is escaped as `\.`
//! when a key itself contains a dot.

use std::str::FromStr;

use serde_json::{Map, Value};
use yaml_edit::path::YamlPath;
use yaml_edit::{AsYaml, Document, Mapping, YamlFile, YamlNode};

use super::{EditError, KV, split_bom};

/// Reads a value from YAML text using a dot-separated path.
///
/// String values are returned without their quotes; every other value is
/// returned as compact JSON. Returns `None` for malformed input or a missing
/// path.
pub fn get(src: &str, key_path: &str) -> Option<String> {
    let (_, body) = split_bom(src);
    let document = parse(body).ok()?.document()?;
    let node = document.get_path(key_path)?;
    match node_to_value(&node)? {
        Value::String(text) => Some(text),
        other => serde_json::to_string(&other).ok(),
    }
}

/// Parses YAML into JSON-shaped data for semantic comparison.
///
/// Returns `None` when the document is malformed.
pub(crate) fn to_value(src: &str) -> Option<Value> {
    let (_, body) = split_bom(src);
    if body.trim().is_empty() {
        return Some(Value::Object(Map::new()));
    }
    let document = parse(body).ok()?.document()?;
    document_root(&document).and_then(|node| node_to_value(&node))
}

/// Writes one or more values into YAML text, preserving formatting.
///
/// New keys are appended to their mapping; missing intermediate mappings are
/// created. `null` values are rejected because the gateway never writes them.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid YAML or a
/// path segment that should be a mapping is a scalar.
pub fn set(original: &str, kvs: &[KV]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    let mut text = body.to_owned();
    for kv in kvs {
        text = set_one(&text, &kv.path, &kv.value)?;
    }
    Ok(format!("{bom}{text}"))
}

/// One nesting step of the indentation we emit (two spaces).
const INDENT_STEP: usize = 2;

/// Writes a single value, preserving the existing document.
///
/// `yaml-edit` copies a detached mapping or sequence into the tree with no
/// indentation of its own (`build_content(.., 0, ..)`), so assigning a nested
/// object makes its keys siblings of the parent key. New objects and arrays
/// are therefore emitted as text and spliced in after the last entry of the
/// parent mapping; scalars still go through the editor so they keep their
/// position and surrounding comments.
fn set_one(text: &str, path: &str, value: &Value) -> Result<String, EditError> {
    if value.is_null() {
        return Err(EditError::Parse(format!(
            "cannot write null into YAML at {path}"
        )));
    }
    let segments = split_path(path);
    if segments.iter().any(String::is_empty) {
        return Err(EditError::Parse(format!("invalid YAML path {path}")));
    }
    if text.trim().is_empty() {
        return Ok(emit_value(&nest(&segments, value), 0));
    }

    let file = parse(text)?;
    let document = file
        .document()
        .ok_or_else(|| EditError::Parse("YAML file has no document".to_owned()))?;
    let root = document
        .as_mapping()
        .ok_or_else(|| EditError::Parse("YAML root is not a mapping".to_owned()))?;
    let mut mapping: Mapping = root;
    let mut leaf = false;
    let mut depth = 0;
    while depth < segments.len() {
        let segment = &segments[depth];
        if depth + 1 == segments.len() {
            leaf = mapping.get(segment.as_str()).is_some();
            break;
        }
        match mapping.get_mapping(segment.as_str()) {
            Some(child) => {
                mapping = child;
                depth += 1;
            }
            None => break,
        }
    }

    if leaf {
        if !matches!(value, Value::Object(_) | Value::Array(_)) {
            let key = segments.last().map_or("", String::as_str);
            mapping.set(key, scalar_node(value, path)?);
            return Ok(file.to_string());
        }
        // Replacing an object/array: drop the old entry, then append the new
        // block after the parent's remaining entries.
        remove_path(&document, &segments);
        return insert_new(&file.to_string(), &segments, value);
    }

    insert_new(&file.to_string(), &segments, value)
}

/// Appends `segments` (the first missing key and below) to its parent mapping.
fn insert_new(text: &str, segments: &[String], value: &Value) -> Result<String, EditError> {
    let file = parse(text)?;
    let document = file
        .document()
        .ok_or_else(|| EditError::Parse("YAML file has no document".to_owned()))?;
    let root = document
        .as_mapping()
        .ok_or_else(|| EditError::Parse("YAML root is not a mapping".to_owned()))?;
    let mut mapping: Mapping = root;
    let mut depth = 0;
    while depth + 1 < segments.len() {
        match mapping.get_mapping(segments[depth].as_str()) {
            Some(child) => {
                mapping = child;
                depth += 1;
            }
            None => break,
        }
    }

    // Render the whole file so comments before the first key are kept.
    let rendered = file.to_string();
    let range = mapping.byte_range();
    let start = range.start as usize;
    let end = range.end as usize;
    let empty = mapping.iter().next().is_none();
    let entry_indent = line_indent(&rendered, start) + if empty { INDENT_STEP } else { 0 };
    let block = emit_value(&nest(&segments[depth..], value), entry_indent);

    let mut out = rendered;
    if empty {
        // No existing entries: start a new block after the parent key's line.
        let insert_at = out[start..]
            .find('\n')
            .map_or(out.len(), |offset| start + offset + 1);
        out.insert_str(insert_at, &block);
    } else {
        let prefix = if out[..end].ends_with('\n') { "" } else { "\n" };
        out.insert_str(end, &format!("{prefix}{block}"));
    }
    Ok(out)
}

/// Rebuilds `a.b.c: value` as `{a: {b: {c: value}}}`.
fn nest(segments: &[String], value: &Value) -> Value {
    let mut nested = value.clone();
    for key in segments.iter().rev() {
        let mut map = Map::new();
        map.insert(key.clone(), nested);
        nested = Value::Object(map);
    }
    nested
}

/// Splits a dot path, honouring `\.` and `\\` escapes.
fn split_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '.' => segments.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    segments.push(current);
    segments
}

/// Leading spaces of the line containing byte `position`.
fn line_indent(text: &str, position: usize) -> usize {
    let line_start = text[..position].rfind('\n').map_or(0, |offset| offset + 1);
    text[line_start..position]
        .chars()
        .take_while(|ch| *ch == ' ')
        .count()
}

fn emit_value(value: &Value, indent: usize) -> String {
    match value {
        Value::Object(entries) => entries
            .iter()
            .map(|(key, item)| emit_entry(key, item, indent))
            .collect(),
        Value::Array(_) => emit_sequence_items(value, indent),
        scalar => format!("{}{}\n", " ".repeat(indent), emit_scalar(scalar)),
    }
}

fn emit_entry(key: &str, value: &Value, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let name = emit_key(key);
    match value {
        Value::Object(entries) if entries.is_empty() => format!("{pad}{name}: {{}}\n"),
        Value::Array(items) if items.is_empty() => format!("{pad}{name}: []\n"),
        Value::Object(_) => format!("{pad}{name}:\n{}", emit_value(value, indent + INDENT_STEP)),
        Value::Array(_) => format!(
            "{pad}{name}:\n{}",
            emit_sequence_items(value, indent + INDENT_STEP)
        ),
        scalar => format!("{pad}{name}: {}\n", emit_scalar(scalar)),
    }
}

fn emit_sequence_items(value: &Value, indent: usize) -> String {
    let items = match value {
        Value::Array(items) => items,
        _ => return emit_value(value, indent),
    };
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for item in items {
        match item {
            Value::Object(entries) if entries.is_empty() => out.push_str(&format!("{pad}- {{}}\n")),
            Value::Array(nested) if nested.is_empty() => out.push_str(&format!("{pad}- []\n")),
            Value::Object(_) => out.push_str(&format!(
                "{pad}-\n{}",
                emit_value(item, indent + INDENT_STEP)
            )),
            Value::Array(_) => out.push_str(&format!(
                "{pad}-\n{}",
                emit_sequence_items(item, indent + INDENT_STEP)
            )),
            scalar => out.push_str(&format!("{pad}- {}\n", emit_scalar(scalar))),
        }
    }
    out
}

fn emit_scalar(value: &Value) -> String {
    match value {
        Value::String(_) => serde_json::to_string(value).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// Emits a plain key when that is unambiguous, otherwise a quoted one.
fn emit_key(key: &str) -> String {
    let mut chars = key.chars();
    let plain = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !matches!(
            key.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        );
    if plain {
        key.to_owned()
    } else {
        serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""))
    }
}

/// Deletes one or more keys from YAML text.
///
/// Deleting a key that does not exist is a successful no-op.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid YAML.
pub fn del(original: &str, keys: &[&str]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    if body.trim().is_empty() {
        return Ok(original.to_owned());
    }
    let file = parse(body)?;
    let Some(document) = file.document() else {
        return Ok(original.to_owned());
    };
    for key in keys {
        remove_path(&document, &split_path(key));
    }
    Ok(format!("{bom}{file}"))
}

/// Deletes `segments` by detaching the matching entry.
///
/// `Document::remove_path`/`Mapping::remove` additionally strip a newline
/// from the previous entry when the removed key is last; for a previous entry
/// whose value is a nested block that splices off the whole value. Detaching
/// the entry node (which carries its own trailing newline) is safe.
fn remove_path(document: &Document, segments: &[String]) -> bool {
    let Some((last, parents)) = segments.split_last() else {
        return false;
    };
    let Some(mut mapping) = document.as_mapping() else {
        return false;
    };
    for segment in parents {
        match mapping.get_mapping(segment.as_str()) {
            Some(child) => mapping = child,
            None => return false,
        }
    }
    let Some(entry) = mapping.find_entry_by_key(last.as_str()) else {
        return false;
    };
    entry.remove();
    true
}

/// Parses a non-empty document. `YamlFile` is used rather than `Document`
/// because comments before the first key live on the file's ROOT node, not on
/// the document, and would otherwise be dropped on save.
fn parse(body: &str) -> Result<YamlFile, EditError> {
    YamlFile::from_str(body).map_err(|error| EditError::Parse(error.to_string()))
}

fn document_root(document: &Document) -> Option<YamlNode> {
    if let Some(mapping) = document.as_mapping() {
        return Some(YamlNode::Mapping(mapping));
    }
    if let Some(sequence) = document.as_sequence() {
        return Some(YamlNode::Sequence(sequence));
    }
    document.as_scalar().map(YamlNode::Scalar)
}

fn scalar_node(value: &Value, path: &str) -> Result<YamlNode, EditError> {
    match value {
        Value::Bool(flag) => scalar(*flag),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                scalar(integer)
            } else if let Some(float) = number.as_f64() {
                scalar(float)
            } else {
                None
            }
        }
        Value::String(text) => scalar(text.as_str()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
    .ok_or_else(|| EditError::Parse(format!("expected a scalar at {path}")))
}

/// Wraps a scalar in a detached node so it can be assigned like any other.
fn scalar(value: impl AsYaml) -> Option<YamlNode> {
    let holder = Mapping::new();
    holder.set("value", value);
    holder.get("value")
}

fn node_to_value(node: &YamlNode) -> Option<Value> {
    match node {
        YamlNode::Scalar(scalar) => Some(scalar_to_value(scalar)),
        YamlNode::Mapping(mapping) => {
            let mut entries = Map::new();
            for (key, value) in mapping.iter() {
                entries.insert(key.as_scalar()?.as_string(), node_to_value(&value)?);
            }
            Some(Value::Object(entries))
        }
        YamlNode::Sequence(sequence) => {
            let mut items = Vec::with_capacity(sequence.len());
            for index in 0..sequence.len() {
                items.push(node_to_value(&sequence.get(index)?)?);
            }
            Some(Value::Array(items))
        }
        // Aliases and custom tags are not used by agent configs; treat them as
        // opaque so a file we cannot reason about is never rewritten.
        YamlNode::Alias(_) | YamlNode::TaggedNode(_) => None,
    }
}

fn scalar_to_value(scalar: &yaml_edit::Scalar) -> Value {
    if scalar.is_null() {
        return Value::Null;
    }
    if let Some(flag) = scalar.as_bool() {
        return Value::Bool(flag);
    }
    if let Some(integer) = scalar.as_i64() {
        return Value::from(integer);
    }
    if let Some(float) = scalar.as_f64() {
        return Value::from(float);
    }
    Value::String(scalar.as_string())
}
