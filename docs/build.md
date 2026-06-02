# Build & Development Guide

## Prerequisites

| Tool | Minimum Version | Check Command |
|------|----------------|---------------|
| Rust | 1.85 (2024 edition) | `rustc --version` |
| Cargo | 1.85 | `cargo --version` |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update
```

## Quick Start

```bash
# Clone the repository
git clone <repo-url>
cd any-converter

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

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
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
