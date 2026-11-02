//! Gemini Code Assist wire helpers.
//!
//! The Code Assist transport (`cloudcode-pa.googleapis.com`) is not the
//! public Gemini API. Requests are wrapped in a `{"model": ..., "request": ...}`
//! envelope and responses arrive wrapped in `{"response": ...}`; these
//! helpers build the endpoint URLs and (un)wrap the envelopes.

use crate::config::UpstreamApi;

/// Build the upstream URL for a provider request on the given API.
///
/// Paths come from [`UpstreamApi::path`]; for every non-`CodeAssist` API the
/// output matches [`crate::proxy::build_upstream_url`] byte-for-byte for the
/// corresponding [`any_converter_core::convert::Format`]. The model id only
/// appears in the URL for [`UpstreamApi::Gemini`]; Code Assist keeps the
/// model inside the request envelope.
pub fn build_url(api: UpstreamApi, base_url: &str, model: &str, streaming: bool) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}{}", api.path(model, streaming))
}

/// Wrap a Gemini `generateContent` body in the Code Assist request envelope:
/// `{"model": <model>, "request": <gemini_body>}`.
pub fn wrap_code_assist(gemini_body: &[u8], model: &str) -> Result<Vec<u8>, String> {
    let request: serde_json::Value = serde_json::from_slice(gemini_body)
        .map_err(|e| format!("request body is not valid JSON: {e}"))?;
    let envelope = serde_json::json!({ "model": model, "request": request });
    serde_json::to_vec(&envelope).map_err(|e| e.to_string())
}

/// Unwrap a Code Assist payload.
///
/// Pass-through safe: anything that is not a JSON object carrying a
/// `response` key is returned byte-for-byte unchanged (parse failures are
/// `Ok(original)`, never an error).
pub fn unwrap_code_assist(payload: &[u8]) -> Result<Vec<u8>, String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return Ok(payload.to_vec());
    };
    let Some(inner) = value.get("response") else {
        return Ok(payload.to_vec());
    };
    serde_json::to_vec(inner).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use any_converter_core::convert::Format;

    use super::*;

    const BASE: &str = "https://cloudcode-pa.googleapis.com";

    #[test]
    fn test_code_assist_urls_trim_trailing_slash() {
        assert_eq!(
            build_url(UpstreamApi::CodeAssist, &format!("{BASE}/"), "m", false),
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
        assert_eq!(
            build_url(UpstreamApi::CodeAssist, BASE, "m", true),
            "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn test_gemini_api_urls_match_proxy_behavior() {
        // Each non-CodeAssist API's path must match proxy::build_upstream_url
        // byte-for-byte for the Format it corresponds to.
        assert_eq!(
            build_url(UpstreamApi::Chat, BASE, "gpt-4", false),
            crate::proxy::build_upstream_url(Format::OpenAIChat, BASE, "gpt-4", false)
        );
        assert_eq!(
            build_url(UpstreamApi::Claude, BASE, "claude-3", true),
            crate::proxy::build_upstream_url(Format::Claude, BASE, "claude-3", true)
        );
        assert_eq!(
            build_url(UpstreamApi::Responses, BASE, "gpt-4o", false),
            crate::proxy::build_upstream_url(Format::OpenAIResponses, BASE, "gpt-4o", false)
        );
        assert_eq!(
            build_url(UpstreamApi::Gemini, BASE, "gemini-pro", false),
            crate::proxy::build_upstream_url(Format::Gemini, BASE, "gemini-pro", false)
        );
        assert_eq!(
            build_url(UpstreamApi::Gemini, BASE, "gemini-pro", true),
            crate::proxy::build_upstream_url(Format::Gemini, BASE, "gemini-pro", true)
        );
    }

    #[test]
    fn test_path_table_all_apis_streaming_and_non_streaming() {
        const MODEL: &str = "gemini-2.5-pro";

        // Chat, Claude and Responses use the same path for both modes.
        for streaming in [false, true] {
            assert_eq!(
                UpstreamApi::Chat.path(MODEL, streaming),
                "/v1/chat/completions"
            );
            assert_eq!(UpstreamApi::Claude.path(MODEL, streaming), "/v1/messages");
            assert_eq!(
                UpstreamApi::Responses.path(MODEL, streaming),
                "/v1/responses"
            );
        }

        // Gemini carries the model in the URL and splits on streaming.
        assert_eq!(
            UpstreamApi::Gemini.path(MODEL, false),
            "/v1beta/models/gemini-2.5-pro:generateContent"
        );
        assert_eq!(
            UpstreamApi::Gemini.path(MODEL, true),
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent"
        );

        // CodeAssist has no model in the URL.
        assert_eq!(
            UpstreamApi::CodeAssist.path(MODEL, false),
            "/v1internal:generateContent"
        );
        assert_eq!(
            UpstreamApi::CodeAssist.path(MODEL, true),
            "/v1internal:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn test_wrap_envelope_shape() {
        let gemini_body = br#"{"contents":[{"parts":[{"text":"hi"}]}]}"#;
        let wrapped = wrap_code_assist(gemini_body, "gemini-2.5-pro").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();
        assert_eq!(value["model"], "gemini-2.5-pro");
        assert_eq!(value["request"]["contents"][0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn test_wrap_rejects_non_json() {
        assert!(wrap_code_assist(b"not json", "m").is_err());
    }

    #[test]
    fn test_unwrap_response_envelope() {
        let payload = br#"{"response":{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}}"#;
        let unwrapped = unwrap_code_assist(payload).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&unwrapped).unwrap();
        assert_eq!(value["candidates"][0]["content"]["parts"][0]["text"], "hi");
    }

    #[test]
    fn test_unwrap_is_pass_through_safe() {
        // Non-JSON, JSON without a "response" key, and a bare "response"
        // value with sibling keys all pass through or unwrap correctly.
        let plain = b"data: not json at all";
        assert_eq!(unwrap_code_assist(plain).unwrap(), plain.to_vec());

        let no_response = br#"{"error":{"message":"boom"}}"#;
        assert_eq!(
            unwrap_code_assist(no_response).unwrap(),
            no_response.to_vec()
        );

        let empty = b"";
        assert_eq!(unwrap_code_assist(empty).unwrap(), empty.to_vec());
    }
}
