/**
 * Typed errors from the `server_request` IPC command. The Rust side rejects
 * with this shape (Tauri serializes `ProxyError` as a tagged object), so a
 * page can show the likely next step instead of an opaque transport error.
 */
export interface ProxyErrorShape {
  kind: "not_running" | "unreachable" | "bad_request" | "key_unavailable" | "http";
  message?: string;
}
