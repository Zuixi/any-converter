# Any Converter Architecture

> **What will I learn?** The full system architecture, from bird's-eye view to component internals.
>
> **Prerequisites:** Read [`docs/onboarding.md`](./onboarding.md) first if you are new to this project.
>
> **Progressive disclosure guide:** each section adds more detail. Start at the top and drill down as needed.

---

## Reading Paths

| Goal                       | Read these sections                                                           |
| -------------------------- | ----------------------------------------------------------------------------- |
| **30-second overview**     | Section 1 only                                                                |
| **Understand components**  | Sections 1–2                                                                  |
| **Implement a new format** | Sections 1, 3.1–3.3, then [`crates/core/AGENTS.md`](../crates/core/AGENTS.md) |
| **Understand the server**  | Sections 1, 4, then [`crates/server/AGENTS.md`](../crates/server/AGENTS.md)   |
| **Understand the CLI**     | Sections 1, 5, then [`crates/cli/AGENTS.md`](../crates/cli/AGENTS.md)         |
| **Full deep dive**         | All sections                                                                  |

---

## 1. Bird's-Eye View

**Any Converter** is a Rust workspace that translates between major LLM provider API formats. Its product shape is a long-lived local gateway with thin CLI, Web, and Desktop clients:

| Mode           | Entry Point                  | Use Case                            |
| -------------- | ---------------------------- | ----------------------------------- |
| **Library**    | `any-converter-core` crate   | Embed format conversion in your app |
| **CLI**        | `any-converter` binary       | Offline JSON/SSE conversion         |
| **HTTP Proxy** | `any-converter-server` crate | Transparent API gateway             |

### 1.1 Resident Gateway Contract

The server owns the authoritative canonical home:

- Windows: `%APPDATA%\any-converter`
- Unix: `$XDG_CONFIG_HOME/any-converter`, or `$HOME/.config/any-converter`
- `ANY_CONVERTER_HOME` overrides the root; `ANY_CONVERTER_SECRETS` overrides only OAuth `secrets.json`

`config.toml`, secret references, management authentication, listener readiness, and instance ownership belong to the server. Desktop SQLite is only a one-time migration source and a read-only log adapter. Provider, inference, and management credentials live in OS keychain entries and are represented in TOML by references such as `keychain:any-converter:provider:openai`. Non-default homes use a deterministic keychain service namespace so installations cannot read or overwrite one another's credentials.

The supervised lifecycle is `Stopped → Starting → Running → Stopping → Stopped`, with `Failed` reached from bind failure or unexpected task exit. Kernel-backed instance and configuration locks have no age-based takeover, and `Running` is published only after `TcpListener::bind` succeeds with the observed bound address. Management mutations report `restart_required`; v1 applies them through a controlled restart rather than partial hot reload.

See [`design/desktop-resident-gateway.md`](./design/desktop-resident-gateway.md) for the accepted ownership, authentication, migration, and failure model.

### 1.2 Core Design Pattern: Pairwise Converters

Each (source, target) format pair has a **dedicated converter** that translates requests, responses, and streaming events directly:

```
┌─────────────┐                              ┌─────────────┐
│ Client      │     ┌────────────────────┐   │ Target      │
│ Format JSON │ ──▶ │ Pairwise Converter │──▶│ Format JSON │
│             │     │ (direct mapping)   │   │             │
│  OpenAI     │     └────────────────────┘   │   Claude    │
│  Claude     │                              │   Gemini    │
│  Gemini     │     For streaming:           │  OpenAI     │
│  ...        │     parse → canonical → emit │   ...       │
└─────────────┘                              └─────────────┘
```

**Benefits:**

- No data loss from lossy IR normalization
- Each converter optimized for its specific pair
- Streaming reuses `CanonicalStreamEvent` as a lightweight intermediate

---

## 2. System Components

```
┌─────────────────────────────────────────────────────────────┐
│                        CLI Binary                           │
│                   (argument parsing)                        │
└──────────────┬───────────────────────────────┬──────────────┘
               │                               │
               │          depends on           │   depends on
               │                               │
               ▼                               ▼
┌────────────────────────────┐       ┌────────────────────────────┐
│    HTTP Proxy Server       │       │    Conversion Engine       │
│  (Axum + reqwest + tokio)  │  ◄──  │   (pure serde library)     │
│                            │       │                            │
│  • Router                  │       │  • Pairwise Converters     │
│  • Request Pipeline        │       │  • Stream Adapters         │
│  • Proxy Forwarder         │       │  • SSE Utilities           │
│  • Auth                    │       │                            │
└────────────────────────────┘       └────────────────────────────┘
```

### Component Boundaries

| Component             | Responsibility                                                   | External Dependencies                                     |
| --------------------- | ---------------------------------------------------------------- | --------------------------------------------------------- |
| **Conversion Engine** | Parse, transform, serialize LLM API payloads                     | `serde`, `thiserror`                                      |
| **HTTP Proxy Server** | Accept HTTP requests, convert, forward to upstream, convert back | `axum`, `reqwest`, `tokio`, `Conversion Engine`           |
| **CLI**               | Parse arguments, dispatch to Engine or Server                    | `clap`, `tokio`, `Conversion Engine`, `HTTP Proxy Server` |

---

## 3. Component: Conversion Engine

The Conversion Engine is a **pure library** with zero async or network dependencies. It contains three subsystems:

### 3.1 Pairwise Converter Subsystem

Each supported format pair has a dedicated converter module implementing `FormatConverter`:

```rust
pub trait FormatConverter: Send + Sync {
    fn convert_request(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError>;
    fn convert_response(&self, input: &[u8]) -> Result<Vec<u8>, ConvertError>;
    fn convert_stream_event(
        &self,
        event: &SseEvent,
        state_in: &mut StreamState,
        state_out: &mut StreamState,
    ) -> Result<Vec<String>, ConvertError>;
}
```

**Converter modules** (12 total):

| Source \ Target | Claude             | OpenAI Chat      | OpenAI Responses | Gemini             |
| --------------- | ------------------ | ---------------- | ---------------- | ------------------ |
| **Claude**      | identity           | `claude_to_chat` | `claude_to_resp` | `claude_to_gemini` |
| **OpenAI Chat** | `chat_to_claude`   | identity         | `chat_to_resp`   | `chat_to_gemini`   |
| **OpenAI Resp** | `resp_to_claude`   | `resp_to_chat`   | identity         | `resp_to_gemini`   |
| **Gemini**      | `gemini_to_claude` | `gemini_to_chat` | `gemini_to_resp` | identity           |

Identity conversions (same format) pass through raw bytes without parsing.

### 3.2 Streaming Types

Streaming still uses lightweight canonical types as an intermediate between parse and emit:

| Type                   | Purpose                           | Key Fields                                             |
| ---------------------- | --------------------------------- | ------------------------------------------------------ |
| `CanonicalStreamEvent` | One SSE delta in canonical form   | `TextDelta`, `ToolCallStart`, `ToolCallDelta`, `Done`  |
| `StreamState`          | Mutable accumulator for streaming | `accumulated_text`, `accumulated_tool_calls`, `phase`  |
| `StopReason`           | Why generation stopped            | `EndTurn`, `MaxTokens`, `ToolUse`, etc.                |
| `Usage`                | Token counts                      | `input_tokens`, `output_tokens`, optional cache fields |

**Stream conversion flow:**

```
Source SSE → StreamAdapter::parse_sse_event → Vec<CanonicalStreamEvent>
    → StreamAdapter::emit_sse_event → Target SSE lines
```

### 3.3 Conversion Dispatch

The top-level API routes to the correct pairwise converter:

```rust
pub fn convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_stream_event(
    event: &SseEvent, from: Format, to: Format,
    state_in: &mut StreamState, state_out: &mut StreamState,
) -> Result<Vec<String>, ConvertError>;
```

Internally, `get_converter(from, to)` returns a `Box<dyn FormatConverter>` for the pair.

**Dual state design:** Streaming conversion carries two `StreamState` instances because input and output formats may both need independent accumulation. For example:

- **Input** (OpenAI Chat) fragments tool call arguments across multiple SSE chunks
- **Output** (Claude) needs to frame content in `content_block_start` / `content_block_stop` pairs

### 3.4 SSE Utilities

A small module for parsing and emitting Server-Sent Events:

- **Parse** raw SSE blocks (handling `event:` + `data:` lines, multi-line data, comments)
- **Split** a byte stream into complete SSE blocks (delimited by `\n\n`)
- **Format** SSE events for emission (`data:` only or `event:` + `data:`)
- **Detect** provider-specific terminators (`[DONE]`, `message_stop`, `response.completed`)

---

## 4. Component: HTTP Proxy Server

The HTTP Proxy accepts requests in any supported format, converts them to the upstream provider's format, forwards them, and converts the response back.

### 4.1 Configuration

Loaded from the canonical `config.toml` through `ConfigStore` at startup. Secret values are resolved from the OS keychain:

```toml
[management]
api_key = "keychain:any-converter:server:management"

[server]
host = "127.0.0.1"
port = 1228
api_key = "keychain:any-converter:server:inference"

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "keychain:any-converter:provider:openai"

[providers.model_map]
"claude-sonnet-4" = "gpt-4.1"
"*" = "gpt-4.1-mini"          # wildcard fallback

[[model_routes]]
pattern = "*"
providers = ["openai"]
```

**Model resolution priority:** exact match → wildcard `*` → passthrough (original name).

### 4.2 Router

The Router maps incoming HTTP paths to detected client formats:

| Path Pattern                                   | Detected Format  | Streaming Source            |
| ---------------------------------------------- | ---------------- | --------------------------- |
| `/v1/chat/completions`                         | OpenAI Chat      | request body `stream` field |
| `/v1/messages`                                 | Claude           | request body `stream` field |
| `/v1/responses`                                | OpenAI Responses | request body `stream` field |
| `/v1beta/models/{model}:generateContent`       | Gemini           | path suffix (non-streaming) |
| `/v1beta/models/{model}:streamGenerateContent` | Gemini           | path suffix (streaming)     |

### 4.3 Request Processing Pipeline

**Request Flow:**

```
                             Client Request
                                   │
                                   ▼
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   Router    │───▶│    Auth     │───▶│   Convert   │───▶│   Proxy     │
│ detect path │    │ validate key│    │ req + model │    │ to upstream │
│   → Format  │    │             │    │   patch     │    │             │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                                                              │
                                                              ▼
                                                       ┌─────────────┐
                                                       │   Upstream  │
                                                       │  Provider   │
                                                       └─────────────┘
```

**Response Flow:**

```
                                                       ┌─────────────┐
                                                       │   Upstream  │
                                                       │  Provider   │
                                                       └──────┬──────┘
                                                              │
                                                              ▼
                                                       ┌──────┬──────┐
                                                       │   Proxy     │
                                                       │  response   │
                                                       │   back      │
                                                       └──────┬──────┘
                                                              │
                ┌───────────────────────────────────────────────┘
                │
                ▼
         ┌─────────────┐    ┌─────────────┐
         │   Pairwise  │───▶│  Response   │
         │  Converter  │    │  to client  │
         │  (direct)   │    │             │
         └─────────────┘    └─────────────┘
```

**Pipeline stages:**

1. **Router** — detect client format from URL path
2. **Auth** — validate client API key (if configured); build upstream auth headers
3. **Convert** — pairwise converter transforms request body to upstream format; patch model name using `model_map`
4. **Proxy** — forward to upstream provider
5. **Convert back** — pairwise converter transforms upstream response to client format
6. **Namespace patch** — restore namespaced tool names for OpenAI Responses API

### 4.4 Proxy Forwarder

Two forwarding paths:

**Non-streaming:**

- POST converted body to upstream URL
- Read full response body
- Convert response body back to client format
- Return as JSON

**Streaming:**

- POST converted body to upstream URL
- Read response as async byte stream
- Buffer chunks into complete SSE blocks
- Convert each block through the pairwise converter
- Stream converted SSE lines back to client via async channel

**Key design choices:**

- **Buffer accumulation** — reqwest yields arbitrary byte chunks; the proxy buffers until a complete SSE block (`\n\n`) is seen
- **Panic safety** — `catch_unwind` around conversion prevents malformed upstream events from crashing the server
- **Namespace patching** — applied to every emitted SSE line for Codex CLI compatibility

### 4.5 Auth

Two responsibilities:

1. **Client validation** — verify `Authorization: Bearer <key>` header against configured `api_key` (optional)
2. **Upstream header building** — generate provider-specific auth headers:

- **OpenAI Chat / Responses:** `Authorization: Bearer <key>`
- **Claude:** `x-api-key: <key>` + `anthropic-version: 2023-06-01`
- **Gemini:** `x-goog-api-key: <key>`

### 4.6 Request/Response Logging

An optional audit logger captures the full lifecycle of every request.

**SQLite is the single query source.** The schema is versioned with `pragma user_version`
(`crates/server/src/storage/schema.rs`) so existing databases are migrated in place.

| Table            | Holds                                                 | Read by                |
| ---------------- | ----------------------------------------------------- | ---------------------- |
| `request_logs`   | narrow metadata + token columns (one row per request) | aggregation, ledger    |
| `request_bodies` | the wide `record_json` blob (bodies, trace)           | ledger drill-down only |
| `rollup_hourly`  | pre-aggregate by `(hour, provider, model, agent)`     | every summary query    |

The split exists because a wide row makes aggregation scan megabytes per call. Measured
on 1M rows: a provider×model ranking took 91.8 s with the bodies inline, 1.40 s split
out, and 0.008 s from the rollup.

- **Config:** `[logging.request_log] enabled = true` plus `logging.dir`.
- **Retention:** `logging.retention_days` (default 30, `0` disables). The background
  sweep deletes older rows and their bodies.
- **Writing:** records are buffered and committed in one transaction per 256 records or
  200 ms, on a background task.
- **Capture:**
  - Non-streaming: full JSON request body, upstream request body, and response body.
  - Streaming: request bodies plus every converted SSE line emitted to the client.
- **Timing:** `latency_ms` is total elapsed; `ttft_ms` is the streaming
  time-to-first-token and `0` for a non-streaming reply.
- **Usage:** token counts from the upstream reply, including Anthropic's 1-hour cache
  writes (`cache_write_1h`). `cache_unknown` marks a vendor that reports no cache tokens
  at all, so its `0` reads as "no data" rather than "a miss".
- **Trace summary:** each record may include `trace.client`, `trace.upstream`, and
  `trace.response` summaries. These extract message previews, tool definitions, tool
  calls, and tool results from OpenAI Responses, OpenAI Chat, Claude Messages, Gemini
  JSON, and captured SSE lines.
- **Privacy:** sensitive headers and body keys (`api_key`, `authorization`, etc.) are
  redacted; bodies are truncated at `max_capture_bytes` and marked `truncated: true`.
- **JSONL mirror:** `logging.jsonl_mirror` (default `false`) additionally writes each
  record to `{dir}/usage.YYYY-MM-DD.jsonl`. Nothing reads it for queries; it exists for
  `tail`, `jq` and backup.
- **Read path:** the Web UI opens `any-converter.sqlite3` read-only. There is no JSONL
  fallback any more — a fallback that silently switches data source hides storage bugs,
  so an empty database answers zeroed totals instead.

**Statistics.** `SqliteStorage::usage_summary` answers a `UsageQuery`
(`from`/`to` half-open UTC range, `hour|day|week|month` granularity, optional
provider/model/agent filter) with totals, a bucketed series, and provider/model/agent
rankings. `Totals::hit_rate` is `cache_read / (input + cache_read + cache_write −
cache_unknown)` and is `None` when nothing is left, so a vendor that counts no cache
never reads as a 0% hit rate. `ledger_page` answers "which call was that?" with keyset
paging (never `offset`) and a filter over day, provider, model, agent, account, failure
and a case-insensitive text query.

### 4.7 Disk Quota

The server spawns a background task that, every five minutes:

1. enforces `logging.max_disk_mb` on JSONL files, deleting the oldest first;
2. applies `logging.retention_days` to `request_logs` (bodies cascade), on a blocking
   thread so it never occupies a runtime worker;
3. every twelfth tick (≈1 h) runs `pragma optimize` to refresh the query planner's
   statistics.

The SQLite database and its WAL/SHM files are never deleted by file quota cleanup.

---

## 5. Component: CLI

A thin wrapper that wires the Conversion Engine and HTTP Proxy Server to command-line arguments.

**Commands:**

| Command                                                  | Purpose                                       |
| -------------------------------------------------------- | --------------------------------------------- |
| `convert --from X --to Y [file]`                         | Offline JSON conversion (request or response) |
| `stream --from X --to Y`                                 | Pipe SSE stream through stdin → stdout        |
| `serve --config file.toml`                               | Start HTTP proxy server with full config      |
| `serve --port 8080 --provider X --format Y --base-url …` | Quick-start single-provider mode              |

---

## 6. Data Flow Deep Dives

### 6.1 Non-Streaming Request/Response

**Request Flow (Direct Pairwise):**

```
┌─────────────┐   ┌────────────────────┐   ┌─────────────┐
│   Client    │   │ Pairwise Converter │   │   Upstream  │
│  (OpenAI    │──▶│  (direct JSON-to-  │──▶│  Provider   │
│   Chat)     │   │   JSON mapping)    │   │  (Claude)   │
└─────────────┘   └────────────────────┘   └─────────────┘
```

**Response Flow (reverse):**

```
┌─────────────┐   ┌────────────────────┐   ┌─────────────┐
│   Upstream  │   │ Pairwise Converter │   │   Client    │
│  Provider   │──▶│  (direct JSON-to-  │──▶│  (OpenAI    │
│  (Claude)   │   │   JSON mapping)    │   │   Chat)     │
└─────────────┘   └────────────────────┘   └─────────────┘
```

### 6.2 Streaming Tool Call Conversion

Tool calls in streaming mode are the most complex path because argument JSON is **fragmented across multiple SSE chunks**.

**Example: OpenAI Chat → OpenAI Responses**

The upstream sends tool call deltas:

```
data: {"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]}}
data: {"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}
```

The `StreamState` accumulates these fragments by `tool_call_index`:

```rust
pub struct AccumulatedToolCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub arguments: String,  // appended chunk by chunk
}
```

When a `finish_reason: "tool_calls"` arrives, the adapter emits a complete `response.output_item.done` event with the fully assembled `function_call`.

**Stream phases:**

```
┌─────────┐    ┌──────────┐    ┌─────────────┐    ┌─────────┐
│  Init   │───▶│ Content  │───▶│  ToolCalls  │───▶│  Done   │
│         │    │ (text)   │    │ (assemble)  │    │         │
└─────────┘    └──────────┘    └─────────────┘    └─────────┘
```

---

## 7. Key Design Decisions

| Decision                                        | Rationale                                                                                             |
| ----------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| **Pairwise converters** (not hub-and-spoke IR)  | No data loss from IR normalization. Each converter can preserve format-specific fields.               |
| **Canonical stream events** (lightweight IR)    | Streaming deltas are inherently similar across formats; a canonical event avoids N² stream parsers.   |
| **Separate Conversion Engine crate**            | Pure library, no async runtime dependency. Testable in isolation without network.                     |
| **StreamState for accumulation**                | Tool call arguments span multiple SSE chunks; state must persist across `convert_stream_event` calls. |
| **Dual StreamState** (`state_in` + `state_out`) | Input and output formats may both need independent accumulation during streaming.                     |
| **Panic boundaries** (`catch_unwind`)           | Malformed upstream data must not crash the proxy server.                                              |
| **Namespace tool support**                      | Codex CLI dispatches tools via `ToolName{namespace, name}` for MCP server routing.                    |
| **Model wildcard `*`**                          | Simplifies config when all client model names map to one upstream model.                              |
| **Buffer-based SSE parsing**                    | reqwest yields arbitrary byte chunks, not event-aligned blocks.                                       |

---

## 8. Testing Strategy

| Layer                    | Scope                          | What to Verify                                                    |
| ------------------------ | ------------------------------ | ----------------------------------------------------------------- |
| **IR unit tests**        | StopReason, Usage, StreamState | Roundtrip serialization, conversion helpers                       |
| **Stream adapter tests** | Each format's stream adapter   | SSE block → canonical events → SSE lines                          |
| **Converter tests**      | Pairwise converters            | Request/response JSON roundtrip, field mapping                    |
| **Integration tests**    | Full conversion pipeline       | End-to-end request/response/stream conversion across format pairs |
| **Server tests**         | Router + handlers              | Route detection, auth rejection, missing route handling           |
| **Proxy tests**          | Forwarder logic                | URL building, SSE block extraction, buffer drain                  |

Run all tests:

```bash
cargo test --workspace
```

---

## 9. Extending the System

### 9.1 Adding a New LLM API Format

1. **Add variant** to the `Format` enum in `convert.rs`
2. **Create format module** under `formats/{new_format}/`:
   - Define **wire-format types** matching the provider's JSON schema (`types.rs`)
   - Implement **`StreamAdapter`** for SSE parsing and emitting (`stream.rs`)
3. **Create converter modules** in `converters/`:
   - One module per existing format in each direction (e.g., for 4 existing formats: 8 new modules)
   - Each implements `FormatConverter` for direct request/response and streaming conversion
4. **Register** all converters in `converters/mod.rs` `get_converter`
5. **Add server route** (if the format has distinct HTTP paths) in the Router
6. **Add auth headers** in the Auth component
7. **Add tests** for all new converter methods

### 9.2 Adding Format-Specific Fields

1. Update the relevant converter(s) to handle the new field
2. Only converters that involve the affected format need changes
3. Add tests for the new field mapping

---

## 10. Component Interface Summary

### Conversion Engine Public API

```rust
pub fn convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_stream_event(
    event: &SseEvent,
    from: Format,
    to: Format,
    state_in: &mut StreamState,
    state_out: &mut StreamState,
) -> Result<Vec<String>, ConvertError>;

pub enum Format {
    OpenAIChat,
    Claude,
    OpenAIResponses,
    Gemini,
}

pub enum ConvertError {
    Json(serde_json::Error),
    UnsupportedConversion { from: String, to: String },
    MissingField(String),
    InvalidField { field: String, reason: String },
    UnsupportedContentType(String),
    SseParse(String),
    StreamState(String),
    Other(String),
}
```

### HTTP Proxy Server Public API

```rust
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>>;

pub struct ServerConfig {
    pub server: ServerSettings,
    pub providers: Vec<ProviderConfig>,
    pub model_routes: Vec<ModelRouteConfig>,
    pub routes: Vec<RouteConfig>,
    pub model_metadata: HashMap<String, ModelMetadata>,
}

pub struct ProviderConfig {
    pub name: String,
    pub format: Format,
    pub base_url: String,
    pub api_key: String,
    pub model_map: HashMap<String, String>,
    pub endpoints: ProviderEndpointConfig,
    pub auth: ProviderAuthConfig,
}

pub struct ProviderEndpointConfig {
    pub path: Option<String>,
    pub stream_path: Option<String>,
}

pub struct ProviderAuthConfig {
    pub scheme: Option<AuthScheme>,
    pub headers: HashMap<String, String>,
}

pub enum AuthScheme {
    Bearer,
    ApiKeyHeader,
    XApiKey,
    GoogleApiKey,
    Anthropic,
    None,
}

pub struct ModelRouteConfig {
    pub pattern: String,
    pub provider: Option<String>,
    pub providers: Vec<String>,
    pub upstream_model: Option<String>,
    pub strategy: RouteStrategy,
}

pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
}
```
