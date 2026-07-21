# Server Crate

`any-converter-server` — HTTP proxy server for LLM API format conversion.

> **Scope**: Routing, authentication, upstream proxying (streaming + non-streaming), and configuration. Depends on `any-converter-core` for all conversion logic.

## Architecture

```
src/
├── lib.rs           — Server entry: run(config) -> Result
├── router.rs        — Axum route table + path→format detection
├── handlers.rs      — Request handlers: model extraction, routing, failover, forward, respond
├── proxy.rs         — Upstream HTTP client logic (reqwest)
├── auth.rs          — Client key validation + upstream auth header building
├── config.rs        — TOML config: providers, model_routes, legacy routes, logging
├── model.rs         — Model extraction, model patching, private-field filtering
├── namespace.rs     — Responses namespace tool map extraction and response patching
├── route_strategy.rs — Provider pool ordering (priority / round_robin)
├── usage.rs         — Async usage logger: UsageRecord → JSON Lines files
├── disk_quota.rs    — Background disk quota manager for the logging directory
├── observability.rs — Structured message/tool trace summaries for request logs
├── storage.rs       — SQLite mirror for structured request/usage logs
└── request_log.rs   — Async full request/response logger → JSON Lines files
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
- Use `auth::build_upstream_auth_headers_for_provider` when forwarding — defaults are derived from format, and provider-level `auth.scheme` / `auth.headers` may override them.

### 5. Upstream Error Handling
- Network errors → `502 Bad Gateway` with `upstream_error` type.
- Conversion errors → `500 Internal Server Error` with `conversion_error` type.
- Missing route/provider → `404` / `500` with descriptive JSON body.
- Always log the original error with `log::error!` before translating to HTTP response.

### 6. Model-Based Routing
- `ServerConfig::resolve_provider(client_format, model)` resolves providers via:
  1. `model_routes` — glob-pattern matching (`claude-*`, `gpt-*`, `*`), first match wins
  2. Legacy `routes` — format-based fallback
- Returns `ResolvedRoute` with a **provider pool** (multiple names for failover).
- `ProviderConfig.resolve_model` further maps client model names to upstream names via `model_map`.
- `route_strategy::order_provider_names` orders the provider pool. `priority` preserves config order; `round_robin` rotates the starting provider while preserving failover order.
- `process_request` iterates the ordered provider pool: on retryable upstream errors (429/5xx), tries next provider.
- Patch the upstream model name into the converted request body before forwarding (`patch_model_in_body`).

### 7. Usage Logging
- `UsageLogger` writes `UsageRecord` to daily `usage.YYYY-MM-DD.jsonl` files via async mpsc channel.
- When `logging.dir` is configured, `UsageLogger` also mirrors records into `{logging.dir}/any-converter.sqlite3`; JSONL remains the fallback if SQLite initialization or writes fail.
- Enabled when `logging.dir` is configured; created in `create_router` and stored in `AppState`.
- Token usage extracted from upstream response bodies (`usage::extract_usage_from_response`).
- Records include: request_id, timestamp, client/upstream model, provider, tokens, latency, status.

### 8. Request/Response Logging
- `RequestLogger` writes full request/response audit records to daily `requests.YYYY-MM-DD.jsonl` files via async mpsc channel.
- Request records are also mirrored into `{logging.dir}/any-converter.sqlite3` through `storage::SqliteStorage`; SQLite write failures must never prevent JSONL fallback writes.
- Concurrent UI readers (Desktop Logs/Usage) must use `SqliteStorage::open_readonly_in_log_dir` so they do not re-run schema/`journal_mode` setup against a live writer.
- Enabled by `[logging.request_log] enabled = true` and requires `logging.dir`.
- Captures both non-streaming (full JSON body) and streaming (SSE lines) responses.
- Streaming latency is measured as **time-to-first-byte** (TTFB) of the upstream response.
- Token usage is extracted from upstream response bodies for non-streaming requests; streaming usage comes from `StreamState.accumulated_usage`.
- Structured trace summaries extract message previews, tool definitions, tool calls, and tool results into `trace.client`, `trace.upstream`, and `trace.response`.
- Request bodies, upstream request bodies, and response bodies are truncated at `max_capture_bytes` and marked with `truncated: true`.
- Sensitive headers and body keys (`api_key`, `authorization`, etc.) are redacted before writing.
- The background disk quota manager (`disk_quota.rs`) enforces `logging.max_disk_mb` by deleting oldest files first.

### 9. Namespace Tool Support (Codex CLI)
- Responses API supports `{type: "namespace", name: "...", tools: [...]}`.
- Extract namespace mapping before conversion (`extract_namespace_map`).
- Patch `function_call` items back to `{name, namespace}` after response conversion (`patch_response_namespaces`, `patch_sse_namespaces`).

### 10. Request Preprocessing
- **Private field filtering**: Before forwarding, `process_request` strips all `_`-prefixed fields from the JSON body. These are internal client markers (e.g. `_stream_tokens`, `_internal_id`) that could cause upstream API rejection.
- **Empty model defense**: If the request body has no `model` field and no Gemini path model, an empty string is used instead of the old `"default"` hardcode — preventing the literal string `"default"` from reaching upstream.

## Common Pitfalls

- **Hanging SSE connections**: If the client disconnects mid-stream, the channel sender fails silently. Check `tx.send(...).is_err()` and return early from the spawned task to avoid leaking tasks.
- **Double `\n\n` in SSE**: Some providers send `\r\n\r\n`. The current parser uses `\n\n` only — verify upstream behavior before changing.
- **Config validation**: `ServerConfig::validate()` is called at startup. Invalid provider references cause an immediate exit with actionable error messages.

## Documentation Maintenance

Before concluding work on this crate, verify:

- [ ] **This AGENTS.md** — Did you add/modify crate constraints, architecture, or pitfalls?
- [ ] **Root AGENTS.md** — Did you introduce a new crate-level pattern that affects cross-crate routing?
- [ ] **`../docs/memory/known-gotchas.md`** — Did you discover a new edge case specific to this crate?
- [ ] **`../docs/architecture.md`** — Did you change this crate's public interface or data flow?
- [ ] **`../CHANGELOG.md`** — Is this a user-visible change?

**Rule:** If any box is checked, update the corresponding file before ending the session.
