use any_converter_core::convert::Format;
use serde_json::Value;

pub(crate) fn strip_private_fields(body: &[u8]) -> Vec<u8> {
    if let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(obj) = val.as_object_mut() {
            obj.retain(|key, _| !key.starts_with('_'));
        }
        serde_json::to_vec(&val).unwrap_or_else(|_| body.to_vec())
    } else {
        body.to_vec()
    }
}

pub(crate) fn extract_model_from_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string))
}

pub(crate) fn patch_model_in_body(
    body: &mut Vec<u8>,
    format: Format,
    model: &str,
) -> Result<(), serde_json::Error> {
    let mut value: Value = serde_json::from_slice(body)?;
    match format {
        Format::Gemini => {
            if value.get("model").is_some() {
                value["model"] = Value::String(model.to_string());
            }
        }
        _ => {
            value["model"] = Value::String(model.to_string());
        }
    }
    *body = serde_json::to_vec(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_private_fields_removes_underscore_keys() {
        let body = br#"{"model":"gpt-4.1","_stream_tokens":true,"messages":[]}"#;
        let result = strip_private_fields(body);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert!(parsed.get("model").is_some());
        assert!(parsed.get("messages").is_some());
        assert!(parsed.get("_stream_tokens").is_none());
    }

    #[test]
    fn test_strip_private_fields_preserves_normal_keys() {
        let body = br#"{"model":"gpt-4.1","stream":true}"#;
        let result = strip_private_fields(body);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed.get("model").unwrap().as_str().unwrap(), "gpt-4.1");
        assert!(parsed.get("stream").unwrap().as_bool().unwrap());
    }
}
