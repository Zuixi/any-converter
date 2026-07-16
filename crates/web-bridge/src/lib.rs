#![allow(clippy::unwrap_used)]

use any_converter_core::convert::{Format, convert_request, convert_response};
use napi::bindgen_prelude::*;
use napi_derive::napi;

fn parse_format(format: &str) -> Result<Format> {
    Format::parse(format).map_err(|err| Error::new(Status::InvalidArg, format!("{err}")))
}

/// Convert a request JSON string from one format to another.
#[napi]
pub fn convert_request_string(input: String, from: String, to: String) -> Result<String> {
    let from_format = parse_format(&from)?;
    let to_format = parse_format(&to)?;
    let output = convert_request(input.as_bytes(), from_format, to_format)
        .map_err(|err| Error::new(Status::GenericFailure, format!("{err}")))?;
    String::from_utf8(output).map_err(|err| Error::new(Status::GenericFailure, err.to_string()))
}

/// Convert a response JSON string from one format to another.
#[napi]
pub fn convert_response_string(input: String, from: String, to: String) -> Result<String> {
    let from_format = parse_format(&from)?;
    let to_format = parse_format(&to)?;
    let output = convert_response(input.as_bytes(), from_format, to_format)
        .map_err(|err| Error::new(Status::GenericFailure, format!("{err}")))?;
    String::from_utf8(output).map_err(|err| Error::new(Status::GenericFailure, err.to_string()))
}
