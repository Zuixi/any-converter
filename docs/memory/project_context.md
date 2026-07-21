# PROJECT CONTEXT MEMORY

SUMMARIZE global project status/system environment/user preferences/etc BY USING ONE SIMPLE SENTENCES.

## Memory Lists
- Project uses Rust 2024 edition with a mixed workspace layout: Rust crates (`core`, `server`, `cli`, `web-bridge`), user-facing apps (`apps/web`, `apps/desktop`), and shared frontend packages (`packages/core`, `views`, `shared`, `ui`, `bridge`, `tsconfig`)
- Desktop React shell uses `react-router-dom` `HashRouter` with pages under `apps/desktop/src/pages/` and typed Tauri IPC wrappers in `apps/desktop/src/lib/api.ts`
- Core architecture: typed Canonical IR (CanonicalRequest/Response) with FormatAdapter + StreamAdapter traits per format
- 4 formats supported: OpenAI Chat, Claude Messages, OpenAI Responses, Gemini — each under crates/core/src/formats/
- Server uses axum 0.8, path-based format detection, config.toml with provider/route/model_map system
- Provider transport can override default format-derived endpoint paths via `[providers.endpoints] path` / `stream_path` and auth via `[providers.auth] scheme` / `headers`
- Server route strategy supports priority failover and round_robin starting-provider rotation via `route_strategy::order_provider_names`
- Server request pipeline helpers are split into `model.rs` (model/private fields), `namespace.rs` (Responses namespace patches), and `route_strategy.rs` (provider pool ordering)
- CLI has 3 subcommands: convert (offline), stream (SSE pipe), serve (HTTP proxy)
- All format adapters support both non-streaming and streaming SSE conversion
- Tool arguments: OpenAI uses JSON string, Claude/Gemini use JSON object — adapter handles stringify/parse
- Claude max_tokens is required — default 4096 when source format doesn't specify
- Claude temperature range is [0,1] — clamped when converting from [0,2] formats
- Gemini model is in URL path not request body — stored in CanonicalRequest.model field
- Gemini uses camelCase, others use snake_case — serde rename attributes handle this
- StreamState.accumulated_tool_calls tracks in-flight tool calls for proper Done emission (required for Codex CLI compatibility)
- Claude adapter merges consecutive same-role Turns before serialization (merge_consecutive_same_role_turns) — critical for parallel tool calls from Responses format
- Namespace tools (type:"namespace" in Responses API, used by Codex CLI for MCP tools) are flattened to qualified names (`ns__tool`) for Claude, then split back to `namespace`+`name` fields in response via server-layer patching
- Server-layer namespace patching: `extract_namespace_map()` (request) → `patch_response_namespaces()` (non-streaming) / `patch_sse_namespaces()` (streaming)
- Codex CLI dispatches tool calls via `ToolName{namespace, name}` HashMap lookup — the `function_call` response MUST include separate `namespace` and `name` fields, not a combined qualified name
- SSE format is `event: <type>\ndata: <json>\n\n` — patch functions must find `\ndata: ` within the line, not assume it starts with `data:`
- `[model_metadata]` in config.toml provides context_window/max_context_window/supports_parallel_tool_calls for `/v1/models` — eliminates Codex "Model metadata not found" warning
- OpenAI Responses adapter uses sub-module split: adapter.rs (core logic ~544L), tools.rs (tool/namespace/format ~189L), helpers.rs (shared function_call+status ~57L), adapter_tests.rs (tests ~314L) — other adapters still monolithic
- Shared helpers pattern: `helpers::parse_function_call_fields` and `helpers::emit_function_call_json` deduplicate function_call JSON handling across adapter+stream; `helpers::status_to_stop_reason`/`stop_reason_to_status` unify status mapping
- Codex CLI model slug matching is CASE-SENSITIVE (`model.starts_with(&candidate.slug)`) — config.toml model names MUST be lowercase to match bundled catalog (e.g. `gpt-5.4` not `GPT-5.4`)
- Codex only fetches remote `/models` when `uses_codex_backend()` or `has_command_auth()` — with API key auth it uses bundled `models.json` exclusively; `/v1/models` endpoint detects Codex via `client_version` query param and returns Codex-compatible `{ "models": [...] }` format
- Structured logging via tracing + tracing-appender: multi-layer subscriber (console + JSON files), non-blocking IO, daily rotation, disk quota management (background task); config in `[logging]` section
- Conversion logging uses `target: "conversion"` with 4 phases: `request_original`, `request_converted`, `response_original`, `response_converted`; correlated by `request_id` (UUID)
- CLI modules: `logging.rs` (subscriber init), `disk_manager.rs` (quota enforcement); guards must be held for application lifetime to ensure flush on exit
- Code quality: no `println!` in any crate, no `unwrap()`/`expect()` in production paths (only in `#[cfg(test)]`)
- Codex CLI Responses SSE critical fields: function_call items MUST have both `id` (`fc_{call_id}`) and `call_id`; argument delta/done events use `item_id` (the `fc_` id) for routing; `response.in_progress` must follow `response.created`; response objects need `created_at` timestamp
- **Namespace tool history**: Codex sends `function_call` items with separate `namespace` and `name` fields (e.g., `namespace:"mcp__playwright"`, `name:"browser_navigate"`). `parse_function_call_fields` MUST re-qualify the name as `{namespace}__{name}` before forwarding to Claude, otherwise Claude won't match the tool definition. The reverse split (qualified→namespace+name) is handled by `patch_sse_namespaces` on the response path.
- **Stream conversion error handling**: `convert_sse_block` in proxy.rs must NEVER return empty Vec on error — silent drops cause Codex to miss critical events (`response.completed` especially) and enter "stream closed before response.completed" error state, triggering retries
- Agent documentation now follows a layered `AGENTS.md` hierarchy: root workspace -> domain (`crates`, `apps`, `packages`, `docs`, `tests`, `scripts`) -> component-level entrypoints
