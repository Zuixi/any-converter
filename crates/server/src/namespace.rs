use std::collections::HashMap;

use any_converter_core::convert::Format;
use serde_json::Value;

/// Extract namespace mapping from Responses API request tools.
/// Returns a map: qualified_name -> (namespace, short_name).
pub(crate) fn extract_namespace_map(
    body: &[u8],
    client_format: Format,
) -> HashMap<String, (String, String)> {
    if client_format != Format::OpenAIResponses {
        return HashMap::new();
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return HashMap::new();
    };
    let Some(tools) = value.get("tools").and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for tool in tools {
        if tool.get("type").and_then(|v| v.as_str()) != Some("namespace") {
            continue;
        }
        let ns = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if ns.is_empty() {
            continue;
        }
        let Some(children) = tool.get("tools").and_then(|v| v.as_array()) else {
            continue;
        };
        for child in children {
            if let Some(name) = child.get("name").and_then(|v| v.as_str()) {
                let qualified = format!("{ns}__{name}");
                map.insert(qualified, (ns.to_string(), name.to_string()));
            }
        }
    }
    map
}

/// Patch function_call items in a Responses API response body to include
/// the `namespace` field and restore the short tool name.
pub(crate) fn patch_response_namespaces(
    body: &mut Vec<u8>,
    ns_map: &HashMap<String, (String, String)>,
) {
    if ns_map.is_empty() {
        return;
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return;
    };
    let mut patched = false;
    if let Some(output) = value.get_mut("output").and_then(|v| v.as_array_mut()) {
        for item in output {
            if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                continue;
            }
            if let Some(name) = item.get("name").and_then(|v| v.as_str()).map(String::from) {
                if let Some((ns, short)) = ns_map.get(&name) {
                    item["name"] = Value::String(short.clone());
                    item["namespace"] = Value::String(ns.clone());
                    patched = true;
                }
            }
        }
    }
    if patched {
        if let Ok(bytes) = serde_json::to_vec(&value) {
            *body = bytes;
        }
    }
}
