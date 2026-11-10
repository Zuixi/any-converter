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
# Import a temporary legacy config with literal keys
cargo run -- serve --config legacy-bootstrap.toml --home /path/to/any-converter-home
rm legacy-bootstrap.toml

# Later starts use the canonical home
cargo run -- serve --home /path/to/any-converter-home
```

#### Option B: Inline Arguments

```bash
cargo run -- serve \
  --host 127.0.0.1 \
  --port 8080 \
  --format openai-chat \
  --base-url https://api.openai.com \
  --home /path/to/any-converter-home
```

Set `ANY_CONVERTER_UPSTREAM_KEY` in the launching shell instead of passing
`--upstream-key`. Its value is imported into the OS keychain during this
one-time bootstrap and is not needed on later starts.

### Using the Release Binary

```bash
# After `cargo build --release`
./target/release/any-converter --help
./target/release/any-converter serve
```

## Configuration

`--config` is a one-time import, not an alternate authority. Provider and
client keys move to the OS keychain, canonical TOML keeps only
`keychain:any-converter:*` references, and later starts use `serve` alone.
See [config.example.toml](../config.example.toml) for the canonical shape; it
is not a bootstrap file because its keychain entries must already exist.

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

## UI (one build, browser and desktop)

There is one frontend: the Vite SPA under `apps/desktop`. The Tauri desktop
app loads it in its webview, and the Rust gateway embeds the same built
`apps/desktop/dist` and serves it to any browser. It provides:

- **Conversion Playground** (`/playground`) — interactive request/response conversion.
- **Request Log Explorer** (`/logs`) — browse request logs, including structured message/tool/usage trace summaries when request logging is enabled.
- **Usage Dashboard** (`/usage`) — token volume and latency charts.
- **Proxy Status** (`/status`) — health, disk usage, and recent errors.
- **Config Editor** (`/config`) — view/edit `config.toml`.

### Architecture at a glance

```
┌─────────────┐      embedded SPA + /v1/admin/*      ┌──────────────────────┐
│  Browser    │ ◄──────────────────────────────────► │  any-converter (Rust │
│             │   page auth via /v1/session          │  gateway, port 1228) │
└─────────────┘                                      └──────────────────────┘

┌─────────────┐      same SPA build in webview       ┌──────────────────────┐
│  Desktop    │ ◄──────────────────────────────────► │  Tauri shell (attaches│
│  app        │      data over Tauri IPC             │  to the gateway)     │
└─────────────┘                                      └──────────────────────┘
```

All data pages read `/v1/admin/*` on the gateway (SQLite is the single
source for logs and usage). The browser authenticates the page with a
page-key session: `any-converter serve` prints a one-time URL
(`http://127.0.0.1:1228/?k=…`) that exchanges the key for a session
cookie over `GET/POST /v1/session`.

### 1. Build the SPA

```bash
pnpm install
pnpm --filter @any-converter/desktop build
```

This type-checks and bundles the SPA into `apps/desktop/dist`.

### 2. Embed it in the Rust binary

`crates/server/build.rs` embeds `apps/desktop/dist` at compile time whenever
the directory exists (`has_ui_dist`), so build the SPA **before** the Rust
binary:

```bash
cargo build --release -p any-converter
./target/release/any-converter serve
```

Startup prints the one-time browser URL — open it and exchange the page key.
A binary built without the SPA answers `GET /` with 503 build instructions.

### Development override: `ANY_CONVERTER_WEB_DIR`

When the binary has no embedded build (or you want to iterate without
recompiling), point the server at a directory containing a built
`index.html`:

```bash
pnpm --filter @any-converter/desktop build
ANY_CONVERTER_WEB_DIR="$PWD/apps/desktop/dist" cargo run -p any-converter -- serve
```

`ANY_CONVERTER_WEB_DIR` takes precedence over nothing and loses to the
embedded copy: the embedded build wins whenever it exists at compile time,
the environment variable only applies to binaries built without one.

### Monorepo structure

```
any-converter/
├── apps/desktop/      # Tauri app + the one Vite SPA
├── packages/
│   ├── ui/            # shadcn/ui based atomic components
│   ├── core/          # Business components and hooks
│   ├── views/         # Page-level view components
│   ├── shared/        # Types, constants, utilities
│   └── tsconfig/      # Shared TypeScript configs
```

## Desktop Client Build

See also [desktop_design.md](./desktop_design.md). Build from the **repo root** with pnpm workspace install:

```bash
# Install frontend workspace dependencies (required after every pull that changes package.json / pnpm-lock.yaml)
pnpm install

# Dev (frontend + Tauri)
pnpm --filter @any-converter/desktop tauri dev

# Production Desktop package
pnpm --filter @any-converter/desktop tauri build
```

Notes:

- `apps/desktop/src-tauri/tauri.conf.json` `beforeBuildCommand` is `pnpm install && pnpm build`, so packaging re-syncs deps before `tsc`.
- If you see `TS2307: Cannot find module 'react-router-dom'`, the local `node_modules` is stale — run `pnpm install` from the repo root, then rebuild.

## CI/CD Reference

A typical CI pipeline should run:

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

### CI and automated tests

GitHub Actions (`.github/workflows/ci.yml`) runs on every pull request and on pushes to `main`, in two jobs:

- **rust**: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- **js**: `pnpm install --frozen-lockfile`, then `pnpm typecheck` (turbo runs `typecheck` across every workspace package) and `pnpm --filter @any-converter/desktop build` (the Vite build with its own `tsc --noEmit`).

Local parity is the same two commands:

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace
pnpm install --frozen-lockfile && pnpm typecheck && pnpm --filter @any-converter/desktop build
```
