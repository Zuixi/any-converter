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
├── ir/           — Canonical intermediate representation
│   ├── mod.rs
│   ├── request.rs, response.rs, stream.rs
│   ├── message.rs, tool.rs, params.rs
└── formats/      — Per-format adapters
    ├── mod.rs    — FormatAdapter + StreamAdapter traits
    ├── openai_chat/
    ├── claude/
    ├── openai_resp/
    └── gemini/
```

## Domain Constraints

### 1. Zero IO / Zero Async
- **MUST NOT** import `tokio`, `reqwest`, `std::fs`, or any IO-related crate.
- All functions are synchronous. If a caller needs async, they wrap it.

### 2. IR Is the Source of Truth
- All format adapters convert **to** canonical IR and **from** canonical IR.
- Never convert directly between two wire formats (A → B). Always go through IR (A → IR → B).
- Changing the IR (`ir/`) requires updating **all four** format adapters.

### 3. Adapter Trait Compliance
- Every new format MUST implement both `FormatAdapter` and `StreamAdapter` from `formats/mod.rs`.
- `FormatAdapter` covers request/response serialization.
- `StreamAdapter` covers stateful SSE chunk conversion (`StreamState` is mutable and per-conversion).

### 4. Error Handling
- Use `ConvertError` (defined in `error.rs`) for all failure paths.
- Prefer specific variants (`MissingField`, `InvalidField`) over `ConvertError::Other`.
- `ConvertError` implements `From<serde_json::Error>` — use `?` freely.

### 5. SSE Utilities Are Format-Agnostic
- `sse.rs` handles raw SSE spec parsing only (`event:`, `data:`, blank-line dispatch).
- Format-specific event naming (e.g., Claude's `message_stop` vs OpenAI's `[DONE]`) lives in the respective `StreamAdapter`, NOT in `sse.rs`.

### 6. Testing
- Unit-test every adapter's parse + serialize roundtrip.
- Unit-test SSE block splitting and parsing edge cases (multi-line data, comments, empty blocks).
- Integration tests for full `convert_stream_event` paths belong in `convert.rs` tests.

## Common Pitfalls

- **Mutating `StreamState`**: `StreamState` carries per-conversion accumulator state (e.g., partial tool-call args). It is `&mut` in adapters — do NOT clone it unnecessarily; mutations are intentional.
- **Serde untagged enums**: Prefer explicit `#[serde(rename = "...")]` on struct fields. Untagged enums make error messages opaque when upstream sends unexpected shapes.
- **Adding a new format**: Requires changes in `convert.rs` (dispatch match arms), `formats/` (new module), AND `lib.rs` exports. Missing any arm = compile-time coverage, but easy to overlook.
