# Any Converter Architecture

> Progressive disclosure guide: each section adds more detail. Start at the top and drill down as needed.

---

## 1. Bird's-Eye View

**Any Converter** is a Rust workspace that translates between major LLM provider API formats. It operates in three modes:


| Mode           | Entry Point                  | Use Case                            |
| -------------- | ---------------------------- | ----------------------------------- |
| **Library**    | `any-converter-core` crate   | Embed format conversion in your app |
| **CLI**        | `any-converter` binary       | Offline JSON/SSE conversion         |
| **HTTP Proxy** | `any-converter-server` crate | Transparent API gateway             |


### 1.1 Core Design Pattern: Canonical IR

Instead of writing N² format-to-format converters, we use a **typed Intermediate Representation**:

```
┌─────────────┐     ┌─────────────────────┐     ┌─────────────┐
│ Client      │     │ Canonical IR        │     │ Target      │
│ Format JSON │ ──▶ │ (typed Rust structs)│ ──▶ │ Format JSON │
│             │     │                     │     │             │
│  OpenAI     │     │ CanonicalRequest    │     │   Claude    │
│  Claude     │     │ CanonicalResponse   │     │   Gemini    │
│  Gemini     │     │ CanonicalStreamEvent│     │  OpenAI     │
│  ...        │     │                     │     │   ...       │
└─────────────┘     └─────────────────────┘     └─────────────┘
```

**Benefit:** Adding a new format requires only **2 adapters** (parse + serialize), not N new converters.

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
│  • Router                  │       │  • Canonical IR            │
│  • Request Pipeline        │       │  • Format Adapters         │
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

The Conversion Engine is a **pure library** with zero async or network dependencies. It contains four subsystems:

### 3.1 IR Subsystem (Intermediate Representation)

The IR defines a **universal schema** for LLM chat requests and responses:


| IR Type                | Purpose                            | Key Fields                                              |
| ---------------------- | ---------------------------------- | ------------------------------------------------------- |
| `CanonicalRequest`     | Normalized chat completion request | `model`, `system`, `turns`, `tools`, `params`, `stream` |
| `CanonicalResponse`    | Normalized completion response     | `id`, `model`, `content`, `stop_reason`, `usage`        |
| `CanonicalStreamEvent` | One SSE delta in canonical form    | `TextDelta`, `ToolCallStart`, `ToolCallDelta`, `Done`   |
| `StreamState`          | Mutable accumulator for streaming  | `accumulated_text`, `accumulated_tool_calls`, `phase`   |


**Core abstraction — `ContentBlock`:**

```rust
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: Vec<ContentBlock>, is_error: bool },
    Thinking { text: String, signature: Option<String> },
}
```

This recursive enum unifies all provider content types (text, images, tool calls, tool results, reasoning chains) into a single type.

### 3.2 Adapter Subsystem

Each supported LLM API format implements two traits:

`**FormatAdapter**` — non-streaming request/response conversion:

```rust
pub trait FormatAdapter {
    type Request: DeserializeOwned + Serialize;
    type Response: DeserializeOwned + Serialize;

    fn parse_request(json: &[u8]) -> Result<Self::Request, ConvertError>;
    fn request_to_canonical(req: Self::Request) -> Result<CanonicalRequest, ConvertError>;
    fn request_from_canonical(req: &CanonicalRequest) -> Result<Self::Request, ConvertError>;

    fn parse_response(json: &[u8]) -> Result<Self::Response, ConvertError>;
    fn response_to_canonical(resp: Self::Response) -> Result<CanonicalResponse, ConvertError>;
    fn response_from_canonical(resp: &CanonicalResponse) -> Result<Self::Response, ConvertError>;
}
```

`**StreamAdapter**` — streaming SSE conversion:

```rust
pub trait StreamAdapter {
    fn parse_sse_event(event: &SseEvent, state: &mut StreamState)
        -> Result<Vec<CanonicalStreamEvent>, ConvertError>;
    fn emit_sse_event(event: &CanonicalStreamEvent, state: &mut StreamState)
        -> Result<Vec<String>, ConvertError>;
}
```

Each format has its own adapter module (Claude, OpenAI Chat, OpenAI Responses, Gemini) containing:

- **Wire-format types** — structs matching the provider's JSON schema
- **Adapter implementation** — `FormatAdapter` + `StreamAdapter` for that format

### 3.3 Conversion Dispatch

The top-level API is a **match dispatcher** that routes to the correct adapter pair:

```
convert_request(input, from, to)
  ├── parse_request_to_canonical(input, from)  → CanonicalRequest
  └── serialize_request_from_canonical(ir, to) → Vec<u8>
```

For streaming:

```
convert_stream_event(event, from, to, state_in, state_out)
  ├── parse_sse_event(event, from, state_in)   → Vec<CanonicalStreamEvent>
  └── emit_sse_event(canonical, to, state_out) → Vec<String>
```

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

Loaded from a TOML file at startup:

```toml
[server]
host = "127.0.0.1"
port = 8080

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[providers.model_map]
"claude-sonnet-4" = "gpt-4.1"
"*" = "gpt-4.1-mini"          # wildcard fallback

[[routes]]
client_format = "claude"
provider = "openai"
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
         ┌─────────────┐    ┌─────────────┐    ┌─────────────┐        
         │   Convert   │───▶│   Convert   │───▶│  Response   │        
         │ (upstream   │    │  (client    │    │  to client  │        
         │  → canon)   │    │   format)   │    │             │        
         └─────────────┘    └─────────────┘    └─────────────┘        
```

**Pipeline stages:**

1. **Router** — detect client format from URL path
2. **Auth** — validate client API key (if configured); build upstream auth headers
3. **Convert** — transform request body to upstream format; patch model name using `model_map`
4. **Proxy** — forward to upstream provider
5. **Convert back** — transform upstream response to client format
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
- Convert each block through the Conversion Engine
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

**Request Flow:**

```
┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐
│   Client    │   │   Client    │   │   Target    │   │   Upstream  │
│  (OpenAI    │──▶│   Format    │──▶│   Format    │──▶│  Provider   │
│   Chat)     │   │   Adapter   │   │   Adapter   │   │             │
└─────────────┘   └──────┬──────┘   └─────────────┘   └─────────────┘
                         │                                           
                         ▼                                           
                  ┌─────────────┐                                    
                  │  Canonical  │                                    
                  │     IR      │                                    
                  │  (request)  │                                    
                  └─────────────┘                                    
```

**Response Flow (reverse):**

```
┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐
│   Upstream  │   │   Target    │   │   Client    │   │   Client    │
│  Provider   │──▶│   Format    │──▶│   Format    │──▶│  (OpenAI    │
│             │   │   Adapter   │   │   Adapter   │   │   Chat)     │
└─────────────┘   └──────┬──────┘   └─────────────┘   └─────────────┘
                         │                                           
                         ▼                                           
                  ┌─────────────┐                                    
                  │  Canonical  │                                    
                  │     IR      │                                    
                  │ (response)  │                                    
                  └─────────────┘                                    
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
| **Canonical IR** (not direct format→format)     | O(N) adapter count instead of O(N²). Centralized testing for edge cases.                              |
| **Separate Conversion Engine crate**            | Pure library, no async runtime dependency. Testable in isolation without network.                     |
| **Typed IR structs** (not `serde_json::Value`)  | Compile-time safety, IDE autocomplete, zero-cost validation.                                          |
| **StreamState for accumulation**                | Tool call arguments span multiple SSE chunks; state must persist across `convert_stream_event` calls. |
| **Dual StreamState** (`state_in` + `state_out`) | Input and output formats may both need independent accumulation during streaming.                     |
| **Panic boundaries** (`catch_unwind`)           | Malformed upstream data must not crash the proxy server.                                              |
| **Namespace tool support**                      | Codex CLI dispatches tools via `ToolName{namespace, name}` for MCP server routing.                    |
| **Model wildcard `*`**                          | Simplifies config when all client model names map to one upstream model.                              |
| **Buffer-based SSE parsing**                    | reqwest yields arbitrary byte chunks, not event-aligned blocks.                                       |


---

## 8. Testing Strategy


| Layer                  | Scope                        | What to Verify                                                    |
| ---------------------- | ---------------------------- | ----------------------------------------------------------------- |
| **IR unit tests**      | Individual IR types          | Roundtrip serialization, edge cases, field omission               |
| **Adapter unit tests** | Each format adapter          | Parse correctness, canonical roundtrip, field mapping             |
| **Stream tests**       | Each format's stream adapter | SSE block → canonical events → SSE lines                          |
| **Integration tests**  | Full conversion pipeline     | End-to-end request/response/stream conversion across format pairs |
| **Server tests**       | Router + handlers            | Route detection, auth rejection, missing route handling           |
| **Proxy tests**        | Forwarder logic              | URL building, SSE block extraction, buffer drain                  |


Run all tests:

```bash
cargo test --workspace
```

---

## 9. Extending the System

### 9.1 Adding a New LLM API Format

1. **Add variant** to the `Format` enum in the Conversion Engine
2. **Create adapter module** under `formats/{new_format}/`:
  - Define **wire-format types** matching the provider's JSON schema
  - Implement `**FormatAdapter`** — parse/serialize requests and responses
  - Implement `**StreamAdapter**` — parse/emit SSE events
3. **Register** in the conversion dispatch match arms
4. **Add server route** (if the format has distinct HTTP paths) in the Router
5. **Add auth headers** in the Auth component
6. **Add tests** for all new adapter methods

### 9.2 Adding a New IR Field

1. Add the field to the relevant IR type
2. Update **all format adapters** to map the field to/from their wire format
3. Add **roundtrip tests** for each adapter

---

## 10. Component Interface Summary

### Conversion Engine Public API

```rust
// Top-level conversion
pub fn convert_request(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_response(input: &[u8], from: Format, to: Format) -> Result<Vec<u8>, ConvertError>;
pub fn convert_stream_event(
    event: &SseEvent,
    from: Format,
    to: Format,
    state_in: &mut StreamState,
    state_out: &mut StreamState,
) -> Result<Vec<String>, ConvertError>;

// Format enumeration
pub enum Format {
    OpenAIChat,
    Claude,
    OpenAIResponses,
    Gemini,
}

// Error type
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
// Start the server
pub async fn run(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>>;

// Configuration types
pub struct ServerConfig {
    pub server: ServerSettings,
    pub providers: Vec<ProviderConfig>,
    pub routes: Vec<RouteConfig>,
    pub model_metadata: HashMap<String, ModelMetadata>,
}

pub struct ProviderConfig {
    pub name: String,
    pub format: Format,
    pub base_url: String,
    pub api_key: String,
    pub model_map: HashMap<String, String>,  // client_model → upstream_model
}

pub struct RouteConfig {
    pub client_format: Format,
    pub provider: String,
}
```

