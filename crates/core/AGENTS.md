# Core Crate

`any-converter-core` — Pure conversion library for LLM API format interconversion.

> **Scope**: Data structures, serialization logic, and stateful SSE stream transformation. **No IO. No network. No async.**

## Architecture

```
src/
├── lib.rs        — Public exports
├── convert.rs    — Top-level dispatch: Format enum + convert_request/response/stream
├── error.rs      — ConvertError enum (thiserror)
├── sse.rs        — SSE parsing/emitting utilities (spec-compliant, zero-copy where possible)
├── ir/           — Streaming intermediate types (StopReason, Usage, StreamState, CanonicalStreamEvent)
│   ├── mod.rs
│   ├── response.rs  — StopReason, Usage
│   └── stream.rs    — CanonicalStreamEvent, StreamState, StreamPhase, AccumulatedToolCall
├── converters/   — Pairwise format converters (12 modules)
│   ├── mod.rs    — FormatConverter trait + get_converter dispatch
│   ├── shared.rs — Common utilities (ID generation, timestamps)
│   ├── claude_to_chat.rs, claude_to_resp.rs, claude_to_gemini.rs
│   ├── chat_to_claude.rs, chat_to_resp.rs, chat_to_gemini.rs
│   ├── resp_to_claude.rs, resp_to_chat.rs, resp_to_gemini.rs
│   └── gemini_to_claude.rs, gemini_to_chat.rs, gemini_to_resp.rs
└── formats/      — Per-format types and streaming adapters
    ├── mod.rs    — StreamAdapter trait
    ├── openai_chat/  (types.rs, stream.rs)
    ├── claude/       (types.rs, stream.rs)
    ├── openai_resp/  (types.rs, stream.rs)
    └── gemini/       (types.rs, stream.rs)
```

## Domain Constraints

### 1. Zero IO / Zero Async
- **MUST NOT** import `tokio`, `reqwest`, `std::fs`, or any IO-related crate.
- All functions are synchronous. If a caller needs async, they wrap it.

### 2. Pairwise Converters
- Each (source, target) format pair has a dedicated converter in `converters/`.
- Converters translate requests and responses directly between wire formats — no shared IR for non-streaming data.
- Streaming events use `CanonicalStreamEvent` as a lightweight intermediate: parse source SSE → canonical events → emit target SSE.
- Adding a new format requires N new converter modules (one per existing format in each direction).

### 3. FormatConverter Trait
- Every converter MUST implement `FormatConverter` from `converters/mod.rs`.
- `convert_request` and `convert_response` handle synchronous JSON-to-JSON conversion.
- `convert_stream_event` handles stateful SSE chunk conversion using `StreamState`.

### 4. StreamAdapter Trait
- Each format implements `StreamAdapter` in `formats/<format>/stream.rs`.
- `parse_sse_event` converts raw SSE into `Vec<CanonicalStreamEvent>`.
- `emit_sse_event` converts `CanonicalStreamEvent` into SSE lines.
- Pairwise converters compose parse + emit from different formats.

### 5. Error Handling
- Use `ConvertError` (defined in `error.rs`) for all failure paths.
- Prefer specific variants (`MissingField`, `InvalidField`) over `ConvertError::Other`.
- `ConvertError` implements `From<serde_json::Error>` — use `?` freely.

### 6. SSE Utilities Are Format-Agnostic
- `sse.rs` handles raw SSE spec parsing only (`event:`, `data:`, blank-line dispatch).
- Format-specific event naming (e.g., Claude's `message_stop` vs OpenAI's `[DONE]`) lives in the respective `StreamAdapter`, NOT in `sse.rs`.

### 7. Testing
- Unit-test each converter's request/response conversion.
- Unit-test SSE block splitting and parsing edge cases (multi-line data, comments, empty blocks).
- Integration tests for full `convert_stream_event` paths belong in `convert.rs` tests.

### 8. Cross-Format Field Safety
- **Thinking/reasoning**: `chat_to_claude` and `resp_to_claude` auto-inject `thinking` config when history contains thinking blocks. `claude_to_resp` maps `thinking` to `reasoning.effort`. All `*_to_gemini` converters drop thinking config entirely.
- **Namespace tool_choice**: When Responses namespace tools are flattened, `tool_choice` name references must be qualified to match the flattened names.
- **Implicit allowlist**: Typed struct deserialization is the primary parameter filter — unknown fields are silently ignored by serde. No explicit whitelist layer is needed.
- **Private fields**: The server strips `_`-prefixed fields before conversion to prevent internal client markers from reaching upstream.

## Common Pitfalls

- **Mutating `StreamState`**: `StreamState` carries per-conversion accumulator state (e.g., partial tool-call args). It is `&mut` in adapters — do NOT clone it unnecessarily; mutations are intentional.
- **Serde untagged enums**: Prefer explicit `#[serde(rename = "...")]` on struct fields. Untagged enums make error messages opaque when upstream sends unexpected shapes.
- **Adding a new format**: Requires new converter modules in `converters/`, a new `StreamAdapter` in `formats/`, AND dispatch entries in `converters/mod.rs` and `convert.rs`.
- **Thinking blocks without config**: Never emit thinking content blocks in Claude requests without the corresponding `thinking: {type: "enabled", ...}` config — Anthropic API will reject with 400.

## Documentation Maintenance

Before concluding work on this crate, verify:

- [ ] **This AGENTS.md** — Did you add/modify crate constraints, architecture, or pitfalls?
- [ ] **Root AGENTS.md** — Did you introduce a new crate-level pattern that affects cross-crate routing?
- [ ] **`../docs/memory/known-gotchas.md`** — Did you discover a new edge case specific to this crate?
- [ ] **`../docs/architecture.md`** — Did you change this crate's public interface or data flow?
- [ ] **`../CHANGELOG.md`** — Is this a user-visible change?

**Rule:** If any box is checked, update the corresponding file before ending the session.
