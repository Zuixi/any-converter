//! Keeping secrets out of the logs: request bodies and headers are cut down
//! and their sensitive values replaced before anything is written.

use super::*;

pub(crate) fn truncated_body(body: &[u8], max_len: usize) -> String {
    let s = String::from_utf8_lossy(body);
    let text = if s.len() <= max_len {
        s.into_owned()
    } else {
        let prefix = utf8_prefix(&s, max_len);
        format!("{}...<truncated {} bytes>", prefix, s.len() - prefix.len())
    };
    sanitize_log_body(&text)
}

const SENSITIVE_KEYS: &[&str] = &["api_key", "apiKey", "authorization", "x-api-key", "secret"];

/// Header names whose values are redacted when logging upstream requests.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "proxy-authorization",
];

pub(crate) fn sanitize_headers_for_log(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let name_lower = name.to_ascii_lowercase();
            if SENSITIVE_HEADERS.contains(&name_lower.as_str()) {
                format!("{name}=[REDACTED]")
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_log_body(text: &str) -> String {
    let mut result = text.to_string();
    for key in SENSITIVE_KEYS {
        let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
        for pat in &patterns {
            while let Some(start) = result.find(pat.as_str()) {
                let val_start = start + pat.len();
                if let Some(end) = result[val_start..].find('"') {
                    let replacement = format!("{pat}[REDACTED]\"");
                    result = format!(
                        "{}{}{}",
                        &result[..start],
                        replacement,
                        &result[val_start + end + 1..]
                    );
                } else {
                    break;
                }
            }
        }
    }
    result
}
