use any_converter_core::convert::Format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Missing,
    Invalid,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Missing => write!(f, "missing or malformed Authorization header"),
            AuthError::Invalid => write!(f, "invalid API key"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Validate the client API key from the Authorization header.
///
/// Skips validation when no client API key is configured.
pub fn validate_client_key(client_api_key: Option<&str>, auth_header: Option<&str>) -> Result<(), AuthError> {
    match client_api_key {
        None => Ok(()),
        Some(expected) => {
            let provided = auth_header
                .and_then(|h| h.strip_prefix("Bearer "))
                .ok_or(AuthError::Missing)?;
            if provided == expected {
                Ok(())
            } else {
                Err(AuthError::Invalid)
            }
        }
    }
}

/// Build upstream authentication headers for the given provider format.
pub fn build_upstream_auth_headers(format: Format, api_key: &str) -> Vec<(String, String)> {
    match format {
        Format::OpenAIChat | Format::OpenAIResponses => {
            vec![(
                "Authorization".to_string(),
                format!("Bearer {api_key}"),
            )]
        }
        Format::Claude => vec![
            ("x-api-key".to_string(), api_key.to_string()),
            (
                "anthropic-version".to_string(),
                "2023-06-01".to_string(),
            ),
        ],
        Format::Gemini => vec![("x-goog-api-key".to_string(), api_key.to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_correct_api_key() {
        let result = validate_client_key(Some("sk-secret"), Some("Bearer sk-secret"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_reject_wrong_api_key() {
        let result = validate_client_key(Some("sk-secret"), Some("Bearer sk-wrong"));
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[test]
    fn test_skip_validation_when_no_key_configured() {
        assert!(validate_client_key(None, None).is_ok());
        assert!(validate_client_key(None, Some("Bearer anything")).is_ok());
    }

    #[test]
    fn test_build_upstream_auth_headers_openai_chat() {
        let headers = build_upstream_auth_headers(Format::OpenAIChat, "sk-test");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-test");
    }

    #[test]
    fn test_build_upstream_auth_headers_responses() {
        let headers = build_upstream_auth_headers(Format::OpenAIResponses, "sk-test");
        assert_eq!(headers[0].1, "Bearer sk-test");
    }

    #[test]
    fn test_build_upstream_auth_headers_claude() {
        let headers = build_upstream_auth_headers(Format::Claude, "sk-ant-test");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("x-api-key".into(), "sk-ant-test".into()));
        assert_eq!(
            headers[1],
            ("anthropic-version".into(), "2023-06-01".into())
        );
    }

    #[test]
    fn test_build_upstream_auth_headers_gemini() {
        let headers = build_upstream_auth_headers(Format::Gemini, "AIza-test");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0], ("x-goog-api-key".into(), "AIza-test".into()));
    }
}
