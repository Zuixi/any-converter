# Build & Development Guide

## Prerequisites

| Tool    | Minimum Version     | Check Command     |
| ------- | ------------------- | ----------------- |
| Rust    | 1.85 (2024 edition) | `rustc --version` |
| Cargo   | 1.85                | `cargo --version` |
| Node.js | 20                  | `node --version`  |
| pnpm    | 9                   | `pnpm --version`  |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update
```

Install pnpm via npm:

```bash
npm install -g pnpm
```

## Quick Start

```bash
# Clone the repository
git clone <repo-url>
cd any-converter

# Install JavaScript dependencies
pnpm install

# Build the entire workspace
cargo build

# Run all tests
cargo test

# Build release binary
cargo build --release
```

## Workspace Structure

```
crates/
├── core/    # Format conversion library (pure Rust, no IO)
├── server/  # HTTP proxy server (axum + reqwest)
└── cli/     # CLI binary (clap + tokio)
```

## Build Commands

### Debug Build

```bash
# Build all crates
cargo build

# Build specific crate
cargo build -p any-converter-core
cargo build -p any-converter-server
cargo build -p any-converter          # CLI binary
```

### Release Build

```bash
# Optimized release build
cargo build --release

# The CLI binary will be at:
# target/release/any-converter
```

## Testing

### Run All Tests

```bash
# Run tests across all crates (including doc tests)
cargo test --workspace

# Run with output visible
cargo test --workspace -- --nocapture
```

### Run Tests for Specific Crate

```bash
cargo test -p any-converter-core
cargo test -p any-converter-server
```

### Run Specific Test

```bash
# Run a single test by name
cargo test test_system_message_extraction

# Run tests matching a pattern
cargo test openai_chat
```

### Test Categories

| Test file              | What it covers                                                                    |
| ---------------------- | --------------------------------------------------------------------------------- |
| `converter_matrix`     | Full cross-format request/response matrix with precise assertions                 |
| `roundtrip`            | Request and response A->B->A consistency                                          |
| `stream_matrix`        | SSE stream conversion with event type and ordering checks                         |
| `parameter_mapping`    | Temperature, top_p, max_tokens, stop sequences, system prompt                     |
| `response_deep`        | Finish reason, usage tokens, tool calls, thinking/reasoning                       |
| `property_tests`       | Property-based: identity, JSON validity, model preservation, StopReason roundtrip |
| `fuzz_tests`           | Random bytes/JSON/SSE robustness, malformed tool args, Unicode                    |
| `concurrent_tests`     | Parallel conversions, StreamState isolation, stress (200+ tasks)                  |
| `error_paths`          | Invalid input, edge cases, error variants                                         |
| `stress_test` (server) | Concurrent HTTP endpoints, auth rejection under load                              |

```bash
# Run property-based tests
cargo test --test property_tests

# Run fuzz-style robustness tests
cargo test --test fuzz_tests

# Run concurrency stress tests
cargo test --test concurrent_tests
cargo test -p any-converter-server --test stress_test
```

### Test Coverage

```bash
# Install cargo-tarpaulin (requires LLVM)
cargo install cargo-tarpaulin

# Generate HTML coverage report
cargo tarpaulin --out Html

# Or use cargo-llvm-cov
cargo install cargo-llvm-cov
cargo llvm-cov --html
```

> **Requirement**: Test coverage must be ≥ 80% before merging.

## Code Quality

### Format

```bash
# Format all code
cargo fmt

# Check formatting without modifying files
cargo fmt -- --check
```

### Lint (Clippy)

```bash
# Run clippy on all targets
cargo clippy --all-targets --all-features

# Treat warnings as errors (CI)
cargo clippy --all-targets --all-features -- -D warnings
```

### Type Check

```bash
# Fast type check without building
cargo check --all-targets
```

### Documentation

```bash
# Build docs
cargo doc --no-deps

# Build and open docs in browser
cargo doc --no-deps --open
```

### Full Pre-Commit Checklist

The repository ships shared Git hooks in `.githooks/`. Enable them once per clone:

```bash
git config core.hooksPath .githooks
```

The `pre-commit` hook then runs automatically on every commit:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --lib --all-features -- -D warnings
cargo test --workspace
```

> The hook lints the library and binary crates only because the test and bench
> code currently uses `unwrap()` and `panic!` patterns that the workspace denies.
> Run `--all-targets` manually when you want to lint tests and benches as well.

A `pre-push` hook also runs `cargo audit` when `cargo-audit` is installed.

Claude Code users get the same guardrails via `.claude/hooks/`:

- `PostToolUse` formats touched `.rs` files and runs `cargo check` on affected crates.
- `Stop` reminds you to update related files when a session completes.

The manual equivalent of the full checklist is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --all-targets
cargo test --workspace
cargo doc --no-deps
```

## Running the Application

### CLI: `convert` — Format Conversion

Convert a request JSON between formats:

```bash
# Convert request: OpenAI Chat → Claude
cargo run -- convert --from openai-chat --to claude < request.json

# Convert response: Claude → OpenAI Chat
cargo run -- convert --from claude --to openai-chat --response < response.json

# Using stdin explicitly
echo '{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}' | \
  cargo run -- convert --from openai-chat --to claude --stdin
```

### CLI: `stream` — SSE Stream Conversion

Convert SSE stream events from stdin to stdout:

```bash
# Convert SSE stream: OpenAI Chat → Claude
cat stream-events.txt | cargo run -- stream --from openai-chat --to claude
```

### CLI: `serve` — HTTP Proxy Server

#### Option A: With Config File

```bash
# Use config.toml
cargo run -- serve --config config.toml

# Or use the example config
cargo run -- serve --config config.example.toml
```

#### Option B: Inline Arguments

```bash
cargo run -- serve \
  --host 127.0.0.1 \
  --port 8080 \
  --format openai-chat \
  --base-url https://api.openai.com \
  --upstream-key sk-your-key
```

### Using the Release Binary

```bash
# After `cargo build --release`
./target/release/any-converter --help
./target/release/any-converter serve --config config.toml
```

## Configuration

Copy the example config and customize:

```bash
cp config.example.toml config.toml
# Edit config.toml with your provider settings
```

See [config.example.toml](../config.example.toml) for full documentation.

## Development Workflow

### 1. Make Changes

Edit code in the relevant crate under `crates/`.

### 2. Run Tests

```bash
cargo test -p <crate-name>
```

### 3. Check Code Quality

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

### 4. Verify Coverage (if adding tests)

```bash
cargo tarpaulin --out Stdout
```

### 5. Build Release

```bash
cargo build --release
```

## Linux Build

项目使用 `rustls-tls`（纯 Rust TLS），在 Linux 上**无需安装任何系统依赖**即可编译。

```bash
# 克隆仓库后直接构建
git clone <repo-url> && cd any-converter
cargo build --release

# 产物: target/release/any-converter
```

> macOS 本机构建同样适用，`rustls-tls` 在所有平台上行为一致。

## Troubleshooting

### `rustc` version too old

```bash
rustup update
rustc --version  # Should be ≥ 1.85
```

### Build fails with linking errors

```bash
# Clean and rebuild
cargo clean
cargo build
```

### Linux 上提示缺少 OpenSSL

项目已使用 `rustls-tls` 替代 `native-tls`，无需系统 OpenSSL。如果仍遇到 OpenSSL 链接错误，请确认 `Cargo.toml` 中 reqwest 使用 `rustls-tls` feature。

### Tests fail on async code

Ensure `tokio` is available in dev-dependencies:

```bash
cargo test -p any-converter-server
```

### Clippy warnings you disagree with

Add to `.claude/settings.json` or run with allowed lints:

```bash
cargo clippy --all-targets --all-features
```

## Web UI

The web interface is a Next.js app located in `apps/web`. It runs **as a separate process** next to the Rust proxy server and provides:

- **Conversion Playground** (`/playground`) — interactive request/response conversion.
- **Request Log Explorer** (`/logs`) — browse request logs, including structured message/tool/usage trace summaries when request logging is enabled.
- **Usage Dashboard** (`/usage`) — token volume and latency charts.
- **Proxy Status** (`/status`) — health, disk usage, and recent errors.
- **Config Editor** (`/config`) — view/edit `config.toml`.

### Architecture at a glance

```
┌─────────────┐      reads/logs      ┌──────────────────────┐
│  Browser    │ ◄──────────────────► │  Next.js web (apps/web)
└─────────────┘                      └──────────┬───────────┘
                                                │ env vars
                                                ▼
                                       ┌──────────────────────┐
                                       │ any-converter-server │
                                       │ (Rust, port 8080)    │
                                       └──────────────────────┘
```

The web app does **not** start the Rust server for you. Start the server first, then start the web app and point it at the server. For `/logs` and `/usage`, the web API reads `{LOG_DIR}/any-converter.sqlite3` first and falls back to `requests.*.jsonl` / `usage.*.jsonl` when SQLite is missing, empty, or unreadable.

### 1. (Optional) Build the Rust bridge

For the fastest conversion playground experience, build the native `napi-rs` addon once:

```bash
pnpm build:bridge
```

This produces `packages/bridge/index.{platform}.node`. If you skip this step, the playground falls back to `cargo run -p any-converter -- convert ...`.

### 2. Start the Rust proxy server

From the repository root:

```bash
./target/release/any-converter serve --config config.toml
```

By default the server listens on `http://127.0.0.1:8080`. If you changed `host`/`port` in `config.toml`, use that URL in the next step.

### 3. Start the web app

In a second terminal, run:

```bash
# Example: server on default port, logs written to ./logs at the repo root
SERVER_URL=http://127.0.0.1:8080 \
LOG_DIR=../../logs \
CONFIG_PATH=../../config.toml \
  pnpm --filter @any-converter/web dev
```

Then open [http://localhost:3000](http://localhost:3000) (Next.js may use 3001 if 3000 is busy).

> **Important about paths:** when you run `pnpm --filter @any-converter/web dev`, the working directory is `apps/web`. Make `LOG_DIR` and `CONFIG_PATH` absolute, or relative to `apps/web`. If the server writes logs to `./logs` from the repo root, use `LOG_DIR=../../logs`.

#### Run the web app on a different port

Use the `PORT` environment variable, for example `9000`:

```bash
PORT=9000 \
SERVER_URL=http://127.0.0.1:8080 \
LOG_DIR=../../logs \
CONFIG_PATH=../../config.toml \
  pnpm --filter @any-converter/web dev
```

Or pass the port directly to Next.js:

```bash
pnpm --filter @any-converter/web dev -- -p 9000
```

### 4. Environment variables

| Variable          | Default                 | Purpose                                                                                                                                                                                                                                                                            |
| ----------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SERVER_URL`      | `http://127.0.0.1:8080` | URL of the running `any-converter-server`. Required for `/status` and live config reloading.                                                                                                                                                                                       |
| `LOG_DIR`         | —                       | Directory containing `any-converter.sqlite3`, `requests.*.jsonl`, and `usage.*.jsonl`. Required for `/logs` and `/usage`; SQLite is read first, JSONL is the fallback. Request logs may include `trace` summaries for messages, tool calls, tool results, and token usage context. |
| `CONFIG_PATH`     | `config.toml`           | Path to the TOML config file. Required for `/config`.                                                                                                                                                                                                                              |
| `LOG_MAX_DISK_MB` | —                       | Disk quota for the log directory. If set, the status page shows usage percentage. This should match `logging.max_disk_mb` in `config.toml`.                                                                                                                                        |
| `PORT`            | `3000`                  | Port for the Next.js dev server.                                                                                                                                                                                                                                                   |

### 5. Build for production

```bash
pnpm build
```

This runs Turborepo across the monorepo and outputs the Next.js production build in `apps/web/.next`. Start it with:

```bash
PORT=9000 \
SERVER_URL=http://127.0.0.1:8080 \
LOG_DIR=../../logs \
CONFIG_PATH=../../config.toml \
  pnpm --filter @any-converter/web start
```

### Monorepo structure

```
any-converter/
├── apps/web/          # Next.js app
├── packages/
│   ├── ui/            # shadcn/ui based atomic components
│   ├── core/          # Business components and hooks
│   ├── views/         # Page-level view components
│   ├── shared/        # Types, constants, utilities
│   ├── bridge/        # napi-rs native addon wrapper
│   └── tsconfig/      # Shared TypeScript configs
```

## Future: Desktop Client Build

When the Tauri Desktop Client is implemented (see [desktop_design.md](./desktop_design.md)):

```bash
# Install prerequisites
npm install -g pnpm

# Install frontend dependencies
pnpm install

# Run dev mode (frontend + Tauri)
pnpm tauri dev

# Build Desktop app
pnpm tauri build
```

## CI/CD Reference

A typical CI pipeline should run:

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```
