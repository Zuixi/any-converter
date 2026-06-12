# Any Converter CHANGELOGS

## Unreleased — Test coverage hardening

### Added

- **Property-based tests** (`property_tests.rs`): Identity conversion, JSON validity, model preservation, StopReason roundtrip, Usage consistency — all driven by `proptest`.
- **Fuzz-style robustness tests** (`fuzz_tests.rs`): Random bytes/JSON/SSE inputs never panic; malformed tool arguments fall back gracefully; Unicode content preserved across all format pairs.
- **Concurrent stress tests** (`concurrent_tests.rs`): 200+ parallel conversions verify thread safety and StreamState isolation for both request/response and streaming paths.
- **Server stress tests** (`stress_test.rs`): 200 concurrent health checks, mixed endpoint requests, and auth rejection under load via tower oneshot.

### Changed

- **Stronger assertions in existing tests**: `converter_matrix.rs` now uses `pretty_assertions` and precise field extraction instead of substring searches. `parameter_mapping.rs` validates exact temperature/top_p/max_tokens/stop_sequence values per format pair (was threshold-based `passed > 0`). `response_deep.rs` asserts finish_reason, usage tokens, and tool names per pair (was `count >= 2/12`). `roundtrip.rs` adds response roundtrip tests and tool name set equality. `stream_matrix.rs` validates format-specific event types and ordering.
- **Test helpers** (`common/mod.rs`): Added ~15 precise field extraction functions (`extract_temperature`, `extract_tool_names`, `extract_response_text`, etc.) and format-aware `minimal_request_json`/`minimal_response_json` builders for proptest generators.

## 0.3.1 — Field safety, thinking/reasoning cross-format mapping, client compatibility

### Fixed

- **Thinking safety**: `chat/resp -> claude` converters now auto-inject `thinking` config when request history contains thinking blocks, preventing 400 errors from Anthropic API.
- **Namespace tool_choice**: When Responses API namespace tools are flattened to qualified names (`mcp__srv__shell`), `tool_choice` references are now corrected to match.
- **Model "default" leak**: Empty model field no longer sends `"default"` to upstream; uses empty string + warning log instead.

### Added

- **Thinking/reasoning cross-format mapping**: Claude `thinking` config maps to Responses `reasoning.effort` and vice versa; thinking is dropped for Gemini targets.
- **`ThinkingConfig` type**: Added to Claude wire types for proper serialization of thinking configuration.
- **Private field filtering**: Request body fields starting with `_` (internal client markers) are stripped before forwarding to upstream providers.
- **Model map longest-prefix matching**: When `model_map` has multiple wildcard patterns, the longest matching pattern wins (e.g. `claude-opus-*` beats `claude-*`).
- **`clean_system_billing_headers`**: Utility to strip `x-anthropic-billing-header:` lines from system prompts.

## 0.3.0 — Model-based intelligent routing, provider failover, and usage logging

### Added

- **Model-based routing** (`[[model_routes]]`): Route requests to upstream providers by model name using glob patterns (`claude-*`, `gpt-*`, `*`). First match wins. Replaces the need for format-only routing.
- **Multi-provider failover**: Configure multiple providers per model route (`providers = ["primary", "backup"]`). On upstream failure (429/5xx), automatically retries with next provider.
- **Route strategy**: `priority` (default, try in order) or `round_robin` per model route.
- **Usage logging**: Token usage automatically extracted from upstream responses and written as JSON Lines to `usage.YYYY-MM-DD.jsonl` files when `logging.dir` is configured. Supports OpenAI, Claude, and Gemini usage formats.
- **Config validation**: Startup checks verify all provider references in `model_routes` and `routes` exist. Missing providers cause an immediate error with actionable messages.
- **`/v1/models` enhancement**: Model list now populated from both `model_routes` (non-wildcard patterns) and `model_map` keys.

### Changed

- `process_request` now extracts model name **before** provider selection, enabling model-based routing.
- Provider resolution order: `model_routes` (first glob match) → legacy `routes` (format match) → 404.
- `config.example.toml` rewritten to showcase model routing with DeepSeek and failover examples.

### Backward Compatible

- Existing `[[routes]]` format-based config continues to work unchanged as a fallback.
- No changes to request/response conversion logic.

## 0.2.0 — Architecture: Pairwise format converters replace canonical IR

### Changed
- **Conversion architecture**: Replaced hub-and-spoke canonical IR conversion with direct pairwise converters — each (source, target) format pair now has a dedicated converter that translates requests, responses, and streaming events directly without data loss
- **`FormatConverter` trait**: New trait in `crates/core/src/converters/mod.rs` defines `convert_request`, `convert_response`, and `convert_stream_event` for each pair
- **Streaming reuse**: Pairwise converters compose existing `StreamAdapter::parse_sse_event` and `StreamAdapter::emit_sse_event` for streaming conversion

### Added
- 12 pairwise converter modules: `claude_to_chat`, `claude_to_resp`, `claude_to_gemini`, `chat_to_claude`, `chat_to_resp`, `chat_to_gemini`, `resp_to_claude`, `resp_to_chat`, `resp_to_gemini`, `gemini_to_claude`, `gemini_to_chat`, `gemini_to_resp`
- `crates/core/src/converters/shared.rs` — common utilities for converters
- Identity conversion passthrough for same-format requests

### Removed
- `FormatAdapter` trait and all `adapter.rs` / `adapter_tests.rs` files
- Canonical IR types no longer used: `CanonicalRequest`, `CanonicalResponse`, `ContentBlock`, `Turn`, `Role`, `ImageSource`, `SystemContent`, `SystemBlock`, `ToolDef`, `ToolChoice`, `GenerationParams`, `ResponseFormat`
- IR modules: `ir/request.rs`, `ir/message.rs`, `ir/tool.rs`, `ir/params.rs`
- Format-specific `helpers.rs` and `tools.rs` files (functionality moved into converters)

### Retained
- IR streaming types: `CanonicalStreamEvent`, `StreamState`, `StreamPhase`, `StopReason`, `Usage`, `AccumulatedToolCall` — used as the intermediate representation for streaming event conversion

## 0.1.7 — Refactor: Migrate logging from tracing to log crate

### Changed
- **Logging facade**: Replaced `tracing` + `tracing-subscriber` + `tracing-appender` with the `log` crate facade and a custom `MultiLogger` implementation
- **Multi-output support**: Console output (stdout/stderr) and file outputs are independently configurable with per-output level filtering, format (pretty/json), and optional target filtering
- **Config enhancement**: Added `[logging.console]` section with `enabled`, `output`, `level`, `format` fields; extended `[[logging.files]]` with `format` and `rotation` fields
- **Daily file rotation**: File outputs support automatic daily rotation via date-suffixed filenames (e.g. `general.2026-06-11.jsonl`)
- **Backward compatible**: All new config fields have sensible defaults; existing TOML configs work without modification

### Removed
- Dependencies: `tracing`, `tracing-subscriber`, `tracing-appender`

### Added
- Dependencies: `log` (with `std` feature), `chrono` (for timestamps and rotation)

## 0.1.6 — Fix: Namespace tool name dropped in function_call history + resilient error handling

### Fixed
- **Critical**: `parse_function_call_fields` dropped the `namespace` field from `function_call` items in request history — when Codex sends `{"type":"function_call", "namespace":"mcp__playwright", "name":"browser_navigate"}`, the converter was forwarding only `"browser_navigate"` to Claude instead of the qualified name `"mcp__playwright__browser_navigate"`, causing Claude to reject unrecognized tool names and triggering Codex retry loops
- **Critical**: `convert_sse_block` in proxy.rs silently swallowed stream conversion errors (returned empty `Vec`), causing critical SSE events like `response.output_item.done` and `response.completed` to be silently dropped — now emits an `error` SSE event for diagnostics
- **Critical**: Panic handler in `convert_sse_block` now also emits an `error` SSE event instead of returning empty output
- Added test `test_namespace_function_call_in_history_qualifies_name` validating the full roundtrip: namespace function_call in Responses request → qualified ToolUse name in canonical IR → correct Claude tool matching

### Root cause analysis
Codex CLI sends `function_call` history items with separate `namespace` and `name` fields for MCP tools. The `helpers::parse_function_call_fields` function was only reading `name` and ignoring `namespace`, so when the tool call was forwarded to Claude, the tool name didn't match any defined tool (Claude had `mcp__playwright__browser_navigate` but received `browser_navigate`). Claude would then return an error or unexpected response, causing Codex to retry indefinitely.

## 0.1.5 — Fix: Codex CLI MCP tool call dead loop (missing function_call item `id`)

### Fixed
- **Critical**: Codex CLI enters infinite loop with MCP tools — function_call items in streaming SSE responses were missing the `id` field (e.g. `fc_{call_id}`), causing Codex to silently discard tool calls and re-send the same request endlessly
- **Root cause**: Codex CLI checks `if (!item.id) return [state, NO_EVENTS]` when processing `response.output_item.added` — without `id`, tool calls are never executed, model keeps re-issuing the same tool call
- Added `id` field (`fc_{call_id}` format) to all function_call items in `response.output_item.added`, `response.output_item.done`, and `response.completed` output arrays
- Added `status` field (`"in_progress"` / `"completed"`) to function_call items matching the real OpenAI Responses API contract
- Added `item_id` field to `response.function_call_arguments.delta` and `response.function_call_arguments.done` events for proper delta routing across parallel tool calls
- Added `response.in_progress` SSE event after `response.created` (matching cc-switch / real API behavior)
- Added `created_at` timestamp to response objects in `response.created` and `response.completed` events

## 0.1.4 — Fix: Codex CLI model metadata warning + Feature: Structured logging system

### Fixed
- **Codex CLI warning**: "Model metadata for `GPT-5.4` not found" caused by case-sensitive slug matching — model names in `config.toml` normalized to lowercase (`gpt-5.4`) to match Codex bundled catalog
- **Root cause**: Codex CLI's `find_model_by_longest_prefix()` uses `model.starts_with(&candidate.slug)` which is case-sensitive; bundled `models.json` uses lowercase slugs; Codex does NOT fetch `/models` from custom base URLs with API key auth (only ChatGPT backend or command auth)
- **Production code audit**: eliminated `unwrap()` from `gemini/tools.rs` (response_schema) and `cli/main.rs` (read_input)

### Added
- **Codex-compatible `/v1/models` endpoint**: detects `client_version` query param (Codex signature) and returns `{ "models": [...] }` format with all required `ModelInfo` fields (`slug`, `display_name`, `shell_type`, `visibility`, `truncation_policy`, etc.)
- **Structured logging system** (`[logging]` config section) with:
  - Multi-layer tracing-subscriber: console (human-readable) + JSON file layers (non-blocking via `tracing-appender`)
  - Per-module/level log file splitting: `general.jsonl`, `error.jsonl`, `conversion.jsonl` + custom files via `[[logging.files]]`
  - Conversion before/after logging (`target: "conversion"`): captures original request, converted request, upstream response, and converted response with `request_id` correlation
  - Daily log rotation via `tracing_appender::rolling::daily`
  - Disk quota management: background task (every 5 min) enforces `max_disk_mb` by removing oldest log files
  - `WorkerGuard`-based crash protection: flushes pending writes on process exit
- **`ModelMetadata` extension**: `display_name` and `description` optional fields for Codex-compatible model info
- **`LoggingConfig`** / **`LogFileConfig`** structs in server config with sensible defaults

### Changed
- CLI `main.rs` now initializes multi-layer tracing instead of plain `fmt().init()`
- `process_request()` now generates `request_id` (UUID) for all log events, enabling request correlation
- Replaced `print!` in `Stream` command with `io::stdout().write_all()` for consistency with `println!` ban

## 0.1.3 — Refactor: Split OpenAI Responses adapter into focused modules

### Changed
- **Refactored** `openai_resp/adapter.rs` from 1078 lines to 544 lines by extracting concerns into sub-modules
- **Extracted** `tools.rs` (189 lines) — tool definition parsing, namespace flattening, tool_choice, response_format serialization
- **Extracted** `helpers.rs` (57 lines) — shared function_call parsing/serialization and status-to-stop_reason mapping (used by both adapter and stream)
- **Extracted** `adapter_tests.rs` (314 lines) — all 12 adapter unit tests via `#[path]` attribute
- **Eliminated** `_impl` indirection — FormatAdapter trait methods now contain logic directly
- **Deduplicated** function_call parsing (was in both `parse_input_item` and `parse_output_item`) into `helpers::parse_function_call_fields`
- **Deduplicated** function_call serialization (was in both `turns_to_input` and `content_blocks_to_output`) into `helpers::emit_function_call_json`
- **Unified** status↔StopReason mapping that was triplicated across adapter.rs and stream.rs into `helpers::status_to_stop_reason` / `helpers::stop_reason_to_status`

## 0.1.2 — Fix: MCP tool discovery/execution and model metadata for Codex CLI

### Fixed
- **Critical**: Codex CLI MCP tools (type `namespace`) were silently dropped during request conversion — they are now flattened into individual `function` tools with qualified names (`namespace__tool_name`) for upstream model compatibility
- **Critical**: Streaming SSE responses now correctly split qualified tool names back into separate `namespace` + `name` fields in `function_call` events, matching Codex CLI's `ToolName{namespace, name}` lookup format
- **Critical**: Non-streaming responses also patch `function_call` output items with proper `namespace` field
- `response.completed` and `response.output_item.done` / `response.output_item.added` SSE events all correctly patched

### Added
- `[model_metadata]` configuration section in `config.toml` — allows serving rich model metadata (`context_window`, `max_context_window`, `supports_parallel_tool_calls`) via `/v1/models` endpoint, eliminating Codex CLI's "Model metadata not found" warning
- `extract_namespace_map()` extracts namespace→tool mapping from Responses API requests before conversion
- `patch_response_namespaces()` restores `namespace` + short `name` in non-streaming responses
- `patch_sse_namespaces()` restores `namespace` + short `name` in streaming SSE events
- `slug` field added to `/v1/models` response for Codex CLI compatibility
- Unit tests for namespace tool flattening, name conflict disambiguation, and roundtrip

## 0.1.1 — Fix: Streaming tool calls not triggering execution in Codex CLI

### Fixed
- **Critical**: OpenAI Responses streaming emitter now emits `response.output_item.done` for `function_call` items — this is required by Codex CLI to trigger tool execution
- **Critical**: `response.function_call_arguments.done` event now emitted when tool call arguments streaming completes
- **Critical**: `response.completed` payload now includes all `function_call` items in the `output` array (previously only included text messages)
- **Critical**: Claude adapter now merges consecutive same-role turns before serializing to Claude messages — fixes 400 errors when parallel tool calls (multiple `function_call` items) produce multiple adjacent assistant turns that violate Claude's strict alternating role requirement
- Added `AccumulatedToolCall` to `StreamState` to track tool call data (id, name, arguments) during streaming
- Root cause (streaming): Codex CLI relies on `response.output_item.done` with a complete `FunctionCall` item to trigger tool execution
- Root cause (request): Parallel tool calls in Responses format produce separate `function_call` input items, each parsed as an independent assistant Turn; without merging, this creates consecutive assistant messages rejected by Claude-format APIs

### Added
- Cross-format integration tests: OpenAI Chat → Responses and Claude → Responses streaming tool call conversion
- Unit tests for tool call accumulation in streaming state

## 0.1.0 — MVP

### Added
- **Core conversion library** (`any-converter-core`) with typed Canonical IR and bidirectional conversion between 4 LLM API formats
- **Format adapters** for OpenAI Chat Completions, Claude Messages, OpenAI Responses, and Google Gemini
- **Streaming SSE adapters** for all 4 formats (parse + emit)
- **HTTP proxy server** (`any-converter-server`) with path-based format detection, client API key auth, upstream auth injection, model mapping, and SSE stream proxying
- **CLI tool** with `convert`, `stream`, and `serve` subcommands
- **156 unit tests** across core (124) and server (32) crates
- Example configuration file `config.example.toml`
