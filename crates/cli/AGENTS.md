# CLI Crate

`any-converter` — Command-line tool for LLM API format conversion and proxy serving.

> **Scope**: Argument parsing, stdin/stdout orchestration, config assembly, and thin delegation to `core` and `server` crates. No business logic.

## Architecture

```
src/
└── main.rs    — CLI entry: clap subcommands, dispatch to core or server
```

## Domain Constraints

### 1. Thin Wrapper Only
- This crate is a **user-facing shell** around `core` and `server`.
- All conversion logic lives in `core`; all HTTP logic lives in `server`.
- Do NOT add format-specific logic here — if you need it, it belongs in `core`.

### 2. Subcommands

| Subcommand | Delegates To | Input | Output |
|------------|-------------|-------|--------|
| `convert` | `core::convert_request` / `convert_response` | File or stdin | stdout (JSON) |
| `stream` | `core::convert_stream_event` | stdin (SSE blocks) | stdout (SSE lines) |
| `serve` | `server::run` | TOML config file or inline flags | HTTP server |

### 3. Stdin / Stdout Handling
- Default to stdin when no input file is provided or `--stdin` is set.
- Always ensure output ends with a newline (`\n`) if it doesn't already.
- Read stdin fully into memory before processing (acceptable for JSON/SSE payloads).

### 4. Config Assembly (`serve` subcommand)
- Two modes:
  1. **File mode**: `--config path.toml` → deserialize via `ServerConfig::from_toml`.
  2. **Inline mode**: `--port`, `--format`, `--base-url`, `--upstream-key`, `--provider`, `--host`, `--api-key` → build a single-provider, single-route config programmatically.
- `--format` is **required** in inline mode. Fail fast with a clear error message.

### 5. Error Messages for Humans
- Use `?` propagation; `Box<dyn std::error::Error>` ensures any error prints its `Display` message.
- Avoid panics. All errors should result in a readable stderr message and non-zero exit code.

### 6. Tracing Setup
- Initialize `tracing_subscriber` with `EnvFilter` at startup.
- Default level: `info`. Override via `RUST_LOG` env var.

## Common Pitfalls

- **Argument conflicts**: `--config` conflicts with `--port`, `--provider`, `--format`, `--base_url`, `--upstream_key`. Enforced by clap `conflicts_with_all` — do NOT bypass.
- **Format enum duplication**: `CliFormat` is a clap `ValueEnum` that mirrors `core::convert::Format`. Any new format requires updating BOTH enums and the `to_format()` mapping.
- **Large stdin payloads**: Currently reads entire stdin into `Vec<u8>` or `String`. For multi-GB streams this will OOM. The current scope (JSON API payloads) makes this acceptable; if streaming large files becomes a requirement, refactor to chunked reading.
