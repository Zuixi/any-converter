# CLI Crate

`any-converter` — Command-line tool for LLM API format conversion and proxy serving.

> **Scope**: Argument parsing, stdin/stdout orchestration, config assembly, and thin delegation to `core` and `server` crates. No business logic.

## Architecture

```
src/
├── main.rs    — CLI entry: clap subcommands, dispatch to core or server
├── stats.rs   — `stats`: the usage report, one shot or `--watch`
├── format.rs  — pure formatters (compact/money/pct/bar/hit_rate)
└── tui/
    ├── mod.rs  — the `--tui` loop: raw mode, poll, redraw, restore
    ├── app.rs  — state and key reducer; `refresh` is the only IO
    └── view.rs — render only; never queries
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
| `stats` | `server::storage::{usage_summary, ledger_page}` (read-only) | `logging.dir` or `--log-dir` | report, JSON, `--watch`, or `--tui` |
| `accounts` | `server::credentials` | subcommands | stored sign-ins |

### 3. TUI Escapes Everything

`stats --tui` is the only path that touches the terminal: raw mode, alternate
screen, a panic hook that restores both. Two invariants:

- **Non-TTY falls back to the plain report.** Piping the command must never
  emit escape codes — CI logs and `| head` depend on it.
- **`view.rs` stays a pure render function.** All querying happens in
  `app::refresh`, on the loop's own thread. Rendering that reads the database
  is how a TUI starts stuttering.

### 4. Stdin / Stdout Handling
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

### 6. Logging Setup
- Uses the `log` crate (facade) with a custom `MultiLogger` implementation in `logging.rs`.
- Supports multi-output: console (stdout/stderr), file (daily rotation, JSON/pretty format).
- Console and file outputs are independently configurable via `LoggingConfig` / `ConsoleConfig` / `LogFileConfig`.
- Default level: `info`. Override via `LoggingConfig.level` in TOML config.
- Logger is set globally via `log::set_boxed_logger` — no guards needed.

## Common Pitfalls

- **Argument conflicts**: `--config` conflicts with `--port`, `--provider`, `--format`, `--base_url`, `--upstream_key`. Enforced by clap `conflicts_with_all` — do NOT bypass.
- **Format enum duplication**: `CliFormat` is a clap `ValueEnum` that mirrors `core::convert::Format`. Any new format requires updating BOTH enums and the `to_format()` mapping.
- **Large stdin payloads**: Currently reads entire stdin into `Vec<u8>` or `String`. For multi-GB streams this will OOM. The current scope (JSON API payloads) makes this acceptable; if streaming large files becomes a requirement, refactor to chunked reading.

## Documentation Maintenance

Before concluding work on this crate, verify:

- [ ] **This AGENTS.md** — Did you add/modify crate constraints, architecture, or pitfalls?
- [ ] **Root AGENTS.md** — Did you introduce a new crate-level pattern that affects cross-crate routing?
- [ ] **`../docs/memory/known-gotchas.md`** — Did you discover a new edge case specific to this crate?
- [ ] **`../docs/architecture.md`** — Did you change this crate's public interface or data flow?
- [ ] **`../CHANGELOG.md`** — Is this a user-visible change?

**Rule:** If any box is checked, update the corresponding file before ending the session.
