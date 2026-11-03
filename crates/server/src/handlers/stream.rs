//! Whether one request asks its upstream for a stream.

use super::*;

/// Detect whether the request should be streamed.
pub fn is_streaming_request(body: &[u8], route_info: RouteInfo) -> bool {
    if route_info.path_streaming {
        return true;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}
