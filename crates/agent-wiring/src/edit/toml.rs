//! Format-preserving TOML editor built on `toml_edit`.
//!
//! `toml_edit` retains comments, table order, and whitespace while allowing
//! individual values to be replaced. This module adds dot-path navigation,
//! BOM/CRLF preservation, and JSON value conversion.

use toml_edit::{Array, Document, InlineTable, Item, Table, Value};

use super::{EditError, KV, split_bom};

/// Reads a value from TOML text using a dot-separated path.
///
/// String values are returned without their quotes; every other value uses its
/// TOML representation. Returns `None` for malformed input or a missing path.
///
/// # Examples
///
/// ```
/// let text = "[model]\nname = \"one\"\n";
/// assert_eq!(agent_wiring::edit::toml::get(text, "model.name").as_deref(), Some("one"));
/// ```
pub fn get(original: &str, key_path: &str) -> Option<String> {
    let (_, body) = split_bom(original);
    let document: Document = body.parse().ok()?;
    let segments: Vec<&str> = key_path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    lookup_table(document.as_table(), &segments)
}

/// Writes one or more values into TOML text using dot-separated paths.
///
/// Missing intermediate tables are created. Existing comments, table order,
/// and whitespace are preserved.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid TOML, when
/// an intermediate path is not a table, or when a JSON value has no TOML
/// equivalent (for example `null`).
pub fn set(original: &str, kvs: &[KV]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    let crlf = body.contains("\r\n");
    let mut document: Document = body
        .parse()
        .map_err(|error: toml_edit::TomlError| EditError::Parse(error.to_string()))?;

    for kv in kvs {
        let segments: Vec<&str> = kv
            .path
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect();
        let (key, table_path) = segments
            .split_last()
            .ok_or_else(|| EditError::Parse("empty key path".into()))?;
        let table = ensure_table(document.as_table_mut(), table_path)?;
        set_key(table, key, &kv.value)?;
    }

    Ok(render(&document, bom, crlf))
}

/// Writes one or more values inside a specific table, creating the table when
/// needed.
///
/// `table_path` is dot-separated (`model_providers.any-converter`). Each
/// [`KV::path`] is relative to that table and may itself be dotted.
///
/// # Errors
///
/// Returns [`EditError::Parse`] for invalid TOML, a non-table intermediate
/// path, or a JSON value with no TOML equivalent.
pub fn set_table(original: &str, table_path: &str, kvs: &[KV]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    let crlf = body.contains("\r\n");
    let mut document: Document = body
        .parse()
        .map_err(|error: toml_edit::TomlError| EditError::Parse(error.to_string()))?;

    let table_segments: Vec<&str> = table_path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();

    for kv in kvs {
        let target = ensure_table(document.as_table_mut(), &table_segments)?;
        let segments: Vec<&str> = kv
            .path
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect();
        let (key, parent_path) = segments
            .split_last()
            .ok_or_else(|| EditError::Parse("empty key path".into()))?;
        let parent = ensure_table(target, parent_path)?;
        set_key(parent, key, &kv.value)?;
    }

    Ok(render(&document, bom, crlf))
}

/// Deletes one or more keys or tables from TOML text.
///
/// Deleting a missing key is a successful no-op. Empty intermediate tables are
/// removed as well.
///
/// # Errors
///
/// Returns [`EditError::Parse`] when the original text is not valid TOML.
pub fn del(original: &str, keys: &[&str]) -> Result<String, EditError> {
    let (bom, body) = split_bom(original);
    let crlf = body.contains("\r\n");
    let mut document: Document = body
        .parse()
        .map_err(|error: toml_edit::TomlError| EditError::Parse(error.to_string()))?;

    for key in keys {
        let segments: Vec<&str> = key
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect();
        remove_path(document.as_table_mut(), &segments);
    }

    Ok(render(&document, bom, crlf))
}

fn render(document: &Document, bom: &str, crlf: bool) -> String {
    let text = document.to_string();
    let text = if crlf {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text
    };
    format!("{bom}{text}")
}

fn ensure_table<'a>(root: &'a mut Table, segments: &[&str]) -> Result<&'a mut Table, EditError> {
    let mut table = root;
    let last_index = segments.len().saturating_sub(1);
    for (index, segment) in segments.iter().enumerate() {
        let explicit = index == last_index;
        let item = table.entry(segment).or_insert_with(|| {
            let mut child = Table::new();
            child.set_implicit(!explicit);
            Item::Table(child)
        });
        table = match item {
            Item::Table(child) => child,
            _ => {
                return Err(EditError::Parse(format!("`{segment}` is not a TOML table")));
            }
        };
    }
    Ok(table)
}

fn set_key(table: &mut Table, key: &str, value: &serde_json::Value) -> Result<(), EditError> {
    let existing = table.get_mut(key);
    match existing {
        Some(existing) => {
            let decor = existing.as_value().map(|value| value.decor().clone());
            let mut new_value = json_to_value(value)?;
            if let Some(decor) = decor {
                *new_value.decor_mut() = decor;
            }
            *existing = Item::Value(new_value);
        }
        None => {
            table.insert(key, Item::Value(json_to_value(value)?));
        }
    }
    Ok(())
}

fn json_to_value(value: &serde_json::Value) -> Result<Value, EditError> {
    match value {
        serde_json::Value::Null => Err(EditError::Parse("TOML has no null value".into())),
        serde_json::Value::Bool(flag) => Ok(Value::from(*flag)),
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(Value::from(integer))
            } else if let Some(float) = number.as_f64() {
                Ok(Value::from(float))
            } else {
                Err(EditError::Parse(format!("unsupported number: {number}")))
            }
        }
        serde_json::Value::String(text) => Ok(Value::from(text.clone())),
        serde_json::Value::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(json_to_value(item)?);
            }
            Ok(Value::Array(array))
        }
        serde_json::Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, item) in map {
                table.insert(key, json_to_value(item)?);
            }
            Ok(Value::InlineTable(table))
        }
    }
}

fn lookup_table(table: &Table, segments: &[&str]) -> Option<String> {
    let (first, rest) = segments.split_first()?;
    let item = table.get(first)?;
    item_string(item, rest)
}

fn item_string(item: &Item, rest: &[&str]) -> Option<String> {
    if rest.is_empty() {
        return match item {
            Item::None => None,
            Item::Value(value) => Some(value_string(value)),
            _ => Some(item.to_string()),
        };
    }
    match item {
        Item::Table(table) => lookup_table(table, rest),
        Item::Value(Value::InlineTable(table)) => lookup_inline(table, rest),
        _ => None,
    }
}

fn lookup_inline(table: &InlineTable, segments: &[&str]) -> Option<String> {
    let (first, rest) = segments.split_first()?;
    let value = table.get(first)?;
    if rest.is_empty() {
        return Some(value_string(value));
    }
    match value {
        Value::InlineTable(table) => lookup_inline(table, rest),
        _ => None,
    }
}

fn value_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.value().clone(),
        Value::Integer(number) => number.value().to_string(),
        Value::Float(number) => number.value().to_string(),
        Value::Boolean(flag) => flag.value().to_string(),
        Value::Datetime(datetime) => datetime.value().to_string(),
        Value::Array(array) => array.to_string().trim().to_string(),
        Value::InlineTable(table) => table.to_string().trim().to_string(),
    }
}

fn remove_path(table: &mut Table, segments: &[&str]) {
    let (first, rest) = match segments.split_first() {
        Some(parts) => parts,
        None => return,
    };

    if rest.is_empty() {
        table.remove(first);
        return;
    }

    let mut remove_child = false;
    if let Some(Item::Table(child)) = table.get_mut(first) {
        remove_path(child, rest);
        remove_child = child.is_empty();
    } else if let Some(Item::Value(Value::InlineTable(child))) = table.get_mut(first) {
        remove_inline(child, rest);
        remove_child = child.is_empty();
    }

    if remove_child {
        table.remove(first);
    }
}

fn remove_inline(table: &mut InlineTable, segments: &[&str]) {
    let (first, rest) = match segments.split_first() {
        Some(parts) => parts,
        None => return,
    };

    if rest.is_empty() {
        table.remove(first);
        return;
    }

    let mut remove_child = false;
    if let Some(Value::InlineTable(child)) = table.get_mut(first) {
        remove_inline(child, rest);
        remove_child = child.is_empty();
    }

    if remove_child {
        table.remove(first);
    }
}
