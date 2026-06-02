# Server Crate

`any-converter-server` — HTTP proxy server for LLM API format conversion.

> **Scope**: Routing, authentication, upstream proxying (streaming + non-streaming), and configuration. Depends on `any-converter-core` for all conversion logic.

## Architecture

```
src/
├── lib.rs        — Server entry: run(config) -> Result
├── router.rs     — Axum route table + path→format detection
├── handlers.rs   — Request handlers: auth, convert, forward, respond
├── proxy.rs      — Upstream HTTP client logic (reqwest)
├── auth.rs       — Client key validation + upstream auth header building
└── config.rs     — TOML config deserialization + provider/route lookup
```

## Domain Constraints

### 1. Never Do Conversion Here
- All JSON/SSE conversion is delegated to `any-converter-core`.
- This crate is a **thin transport layer** — it moves bytes, headers, and status codes.

### 2. Panic Isolation
- Conversion functions in `core` may panic on unimplemented paths.
- Wrap ALL `convert_request`, `convert_response`, and `convert_stream_event` calls with `std::panic::catch_unwind`.
- On panic, return `500 conversion_error` to the client; do NOT crash the server.
- See `handlers.rs` (`safe_convert_request`, `safe_convert_response`) for the pattern.

### 3. Streaming SSE Buffer Management
- Upstream SSE arrives in arbitrary byte chunks. Accumulate in a `String` buffer and split on `\n\n`.
- Process complete blocks immediately; leave partial data in the buffer.
- On stream end, process any remaining non-empty buffer as a final block.
- See `proxy.rs` (`take_complete_sse_blocks`).

### 4. Auth & Secrets
- `api_key` in `ServerConfig` is for **client authentication** (optional).
- `ProviderConfig.api_key` is the **upstream provider key** (always required).
- Use `auth::validate_client_key` for incoming requests.
- Use `auth::build_upstream_auth_headers` when forwarding — each provider has distinct header conventions (Bearer, x-api-key, x-goog-api-key).

### 5. Upstream Error Handling
- Network errors → `502 Bad Gateway` with `upstream_error` type.
- Conversion errors → `500 Internal Server Error` with `conversion_error` type.
- Missing route/provider → `404` / `500` with descriptive JSON body.
- Always log the original error with `tracing::error!` before translating to HTTP response.

### 6. Model Resolution
- `ProviderConfig.resolve_model` maps client model names to upstream names via `model_map`.
- Priority: exact match → wildcard `"*"` → passthrough.
- Patch the upstream model name into the converted request body before forwarding (`patch_model_in_body`).

### 7. Namespace Tool Support (Codex CLI)
- Responses API supports `{type: "namespace", name: "...", tools: [...]}`.
- Extract namespace mapping before conversion (`extract_namespace_map`).
- Patch `function_call` items back to `{name, namespace}` after response conversion (`patch_response_namespaces`, `patch_sse_namespaces`).

## Common Pitfalls

- **Hanging SSE connections**: If the client disconnects mid-stream, the channel sender fails silently. Check `tx.send(...).is_err()` and return early from the spawned task to avoid leaking tasks.
- **Double `\n\n` in SSE**: Some providers send `\r\n\r\n`. The current parser uses `\n\n` only — verify upstream behavior before changing.
- **Config validation**: `ServerConfig` is deserialized from TOML but NOT validated at load time. Invalid routes (e.g., route points to missing provider) are caught at request time, not startup.
