# Server Crate

`any-converter-server` — HTTP proxy server for LLM API format conversion.

> **Scope**: Routing, authentication, upstream proxying (streaming + non-streaming), and configuration. Depends on `any-converter-core` for all conversion logic.

## Architecture

```
src/
├── lib.rs           — Server entry: run(config) -> Result
├── router.rs        — Axum route table + path→format detection
├── handlers.rs      — Request handlers: model extraction, routing, failover, forward, respond
├── logging.rs       — Shared CLI/Desktop runtime logger with console and file outputs
├── proxy.rs         — Upstream HTTP client logic (reqwest)
├── auth.rs          — Client key validation + upstream auth header building
├── config.rs        — TOML config: providers, model_routes, legacy routes, logging
├── model.rs         — Model extraction, model patching, private-field filtering
├── namespace.rs     — Responses namespace tool map extraction and response patching
├── route_strategy.rs — Provider pool ordering (priority / round_robin)
├── usage.rs         — Async usage logger: UsageRecord → JSON Lines mirror (off by default)
├── period.rs        — Period presets → half-open UTC ranges (today/7d/30d/all/last:/..)
├── disk_quota.rs    — Background sweep: file quota, sqlite retention, pragma optimize
├── observability.rs — Structured message/tool trace summaries for request logs
├── storage.rs       — SqliteStorage: the only module that touches SQLite
├── storage/
│   ├── schema.rs    — Versioned migrations, tracked in `pragma user_version`
│   ├── query.rs     — Granularity, UsageQuery/Summary, Totals (+hit_rate), LedgerFilter
│   └── ledger.rs    — LedgerRow/Page/By, keyset paging, CSV export
└── request_log.rs   — Async full request/response logger
    ├── redactor.rs  — Body/header redaction and truncation
    └── usage.rs     — Per-format usage and served-model extraction
```

## Storage Model

Three layers, one owner each:

| Layer | Owner | Storage |
|---|---|---|
| Write | `RequestLogger` batch task (one transaction per 256 records / 200 ms) | `request_logs` (narrow) + `request_bodies` (wide) |
| Read (aggregate) | `SqliteStorage::usage_summary` | `rollup_hourly` pre-aggregate |
| Read (detail) | `SqliteStorage::ledger_page` | `request_logs`, keyset paging |

**`storage.rs` and its submodules are the only code that may issue SQL.** Handlers,
period resolution and the API layer stay above it.

**Single writer invariant.** One process writes; everything else opens read-only
(`SqliteStorage::open_readonly_in_log_dir`, which deliberately skips migration so it
never fights the writer's lock). Desktop and the embedded server share one file —
adding a second writer will produce `SQLITE_BUSY` under load.

**The rollup is a cache.** It is written in the same transaction as the detail row,
but only for a first-time `request_id` (a replay updates the detail and leaves the
rollup contribution alone). `rebuild_rollup()` recomputes it from `request_logs` and
is the only way back to the truth; run it after any suspected drift.

**Cost is frozen at write time.** `cost_usd` and `price_version` are stored with the
row. A later price change must not rewrite history: it needs an explicit recompute,
not a re-read.

**`pragma optimize` is not optional.** Without statistics SQLite picks the wrong
index and aggregation degrades by ~65×. The background sweep runs it hourly.

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
- When `logging.dir` is configured, `UsageLogger` also mirrors records into `{logging.dir}/any-converter.sqlite3`; request and usage writers share one SQLite connection pool, and JSONL remains the fallback if SQLite initialization or writes fail.
- Enabled when `logging.dir` is configured; created in `create_router` and stored in `AppState`.
- Token usage extracted from upstream response bodies (`usage::extract_usage_from_response`).
- Records include: request_id, timestamp, client/upstream model, provider, tokens, latency, status.

### 8. Request/Response Logging
- `RequestLogger` writes full request/response audit records to 10 MiB daily segments (`requests.YYYY-MM-DD.000.jsonl`, `.001`, ...) via async mpsc channel.
- Request records are also mirrored into `{logging.dir}/any-converter.sqlite3` through `storage::SqliteStorage`; SQLite write failures must never prevent JSONL fallback writes.
- Concurrent UI readers (Desktop Logs/Usage) must use `SqliteStorage::open_readonly_in_log_dir` so they do not re-run schema/`journal_mode` setup against a live writer.
- Enabled by `[logging.request_log] enabled = true` and requires `logging.dir`.
- Captures both non-streaming (full JSON body) and streaming (SSE lines) responses.
- Streaming latency is measured as **time-to-first-byte** (TTFB) of the upstream response.
- Token usage is extracted from upstream response bodies for non-streaming requests; streaming usage comes from `StreamState.accumulated_usage`.
- Structured trace summaries extract message previews, tool definitions, tool calls, and tool results into `trace.client`, `trace.upstream`, and `trace.response`.
- Request bodies, upstream request bodies, and response bodies are truncated at `max_capture_bytes` and marked with `truncated: true`.
- Sensitive headers and body keys (`api_key`, `authorization`, etc.) are redacted before writing.
- The background disk quota manager (`disk_quota.rs`) enforces `logging.max_disk_mb` by deleting oldest log files first while preserving the active SQLite database, WAL, and SHM files.
- `[logging.upstream_headers_log] = true` logs the headers sent to upstream providers (sensitive values redacted) for debugging client header passthrough.

### 9. Namespace Tool Support (Codex CLI)
- Responses API supports `{type: "namespace", name: "...", tools: [...]}`.
- Extract namespace mapping before conversion (`extract_namespace_map`).
- Patch `function_call` items back to `{name, namespace}` after response conversion (`patch_response_namespaces`, `patch_sse_namespaces`).

### 10. Request Preprocessing
- **Private field filtering**: Before forwarding, `process_request` strips all `_`-prefixed fields from the JSON body. These are internal client markers (e.g. `_stream_tokens`, `_internal_id`) that could cause upstream API rejection.
- **Empty model defense**: If the request body has no `model` field and no Gemini path model, an empty string is used instead of the old `"default"` hardcode — preventing the literal string `"default"` from reaching upstream.

### 11. Client Header Passthrough

- Client headers are forwarded to upstream providers by default, so tool-specific metadata (e.g. `anthropic-*`, `openai-organization`, `x-stainless-*`, `x-requested-with`, `user-agent`) reaches the upstream.
- The proxy always removes hop-by-hop headers (`connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`) and request-control headers (`host`, `content-length`, `content-type`, `authorization`).
- Provider authentication headers are always injected and take precedence over any client headers with the same name.
- Use `[server.pass_through_headers]` in config to switch to deny-list mode (`mode = "deny"`) or to add custom `allow`/`deny` glob patterns.

## Common Pitfalls

- **UTF-8 log previews**: Never slice `&str` with a raw byte index (e.g. `&block[..200]`). Multibyte CJK text will panic at runtime. Use `observability::utf8_prefix`.
- **`SanitizedBody.truncated` round-trip**: Writers omit `truncated` when `false`; readers must use `#[serde(default)]` or log replay/SQLite reads fail with `missing field truncated`.
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
