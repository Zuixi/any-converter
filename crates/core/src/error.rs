use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConvertError {
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unsupported format conversion: {from} -> {to}")]
    UnsupportedConversion { from: String, to: String },

    #[error("missing required field: {0}")]
    MissingField(String),

    #[error("invalid field value for '{field}': {reason}")]
    InvalidField { field: String, reason: String },

    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),

    #[error("SSE parse error: {0}")]
    SseParse(String),

    #[error("stream state error: {0}")]
    StreamState(String),
}
