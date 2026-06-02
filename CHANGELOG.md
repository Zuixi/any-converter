# Any Converter CHANGELOGS

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
