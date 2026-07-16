use serde::{Deserialize, Serialize};

const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "cookie",
    "set-cookie",
];

const SENSITIVE_BODY_KEYS: &[&str] = &["api_key", "apiKey", "authorization", "x-api-key", "secret"];

/// A body that has been sanitized for logging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SanitizedBody {
    /// JSON-serialized body if it was valid JSON, otherwise the raw text.
    pub text: String,
    /// Whether the body was truncated because it exceeded the capture limit.
    #[serde(skip_serializing_if = "is_not_truncated")]
    pub truncated: bool,
}

fn is_not_truncated(value: &bool) -> bool {
    !value
}

/// Sanitize a list of headers by redacting sensitive values.
pub fn sanitize_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(key, value)| {
            let lower = key.to_lowercase();
            let sanitized = if SENSITIVE_HEADERS.contains(&lower.as_str()) {
                "[REDACTED]".to_string()
            } else {
                value.clone()
            };
            (key.clone(), sanitized)
        })
        .collect()
}

/// Sanitize a request/response body for logging.
///
/// - If the body exceeds `max_bytes`, it is truncated and marked as such.
/// - If the body is valid JSON, sensitive keys are redacted recursively.
/// - Otherwise, the body is returned as a raw string.
pub fn sanitize_body(body: &[u8], max_bytes: usize) -> SanitizedBody {
    if body.len() > max_bytes {
        let prefix = &body[..max_bytes];
        return SanitizedBody {
            text: String::from_utf8_lossy(prefix).to_string(),
            truncated: true,
        };
    }

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        let sanitized = sanitize_json_value(value);
        let text = serde_json::to_string(&sanitized).unwrap_or_default();
        SanitizedBody {
            text,
            truncated: false,
        }
    } else {
        SanitizedBody {
            text: String::from_utf8_lossy(body).to_string(),
            truncated: false,
        }
    }
}

fn sanitize_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, val) in map {
                let new_val = if SENSITIVE_BODY_KEYS.contains(&key.as_str()) {
                    serde_json::Value::String("[REDACTED]".to_string())
                } else {
                    sanitize_json_value(val)
                };
                sanitized.insert(key, new_val);
            }
            serde_json::Value::Object(sanitized)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sanitize_json_value).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_headers_redacts_sensitive() {
        let headers = vec![
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("X-Api-Key".to_string(), "key-123".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let sanitized = sanitize_headers(&headers);
        assert_eq!(sanitized[0].1, "[REDACTED]");
        assert_eq!(sanitized[1].1, "[REDACTED]");
        assert_eq!(sanitized[2].1, "application/json");
    }

    #[test]
    fn test_sanitize_body_redacts_nested_keys() {
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"api_key":"sk-secret"}"#;
        let sanitized = sanitize_body(body, 1024);
        let parsed: serde_json::Value = serde_json::from_str(&sanitized.text).unwrap();
        assert_eq!(parsed["api_key"], "[REDACTED]");
        assert_eq!(parsed["model"], "gpt-4");
    }

    #[test]
    fn test_sanitize_body_truncates_large_body() {
        let body = b"x".repeat(100);
        let sanitized = sanitize_body(&body, 50);
        assert_eq!(sanitized.text.len(), 50);
        assert!(sanitized.truncated);
    }

    #[test]
    fn test_sanitize_body_non_json_preserved() {
        let body = b"plain text body";
        let sanitized = sanitize_body(body, 1024);
        assert_eq!(sanitized.text, "plain text body");
        assert!(!sanitized.truncated);
    }
}
