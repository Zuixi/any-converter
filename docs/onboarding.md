# Onboarding Guide

> **What will I learn?** How to orient yourself in this codebase in 30 minutes.
>
> **Prerequisites:** Basic Rust knowledge (`cargo`, `rustc`).
>
> **Time:** 30 minutes total (5 min → 10 min → 15 min).

---

## Step 1: 30-Second Overview (L0)

**any-converter** is a Rust-based LLM API format conversion tool. It converts requests, responses, and streaming SSE events between major LLM provider APIs:

- OpenAI Chat Completions
- Claude Messages
- OpenAI Responses
- Google Gemini

It operates in **three modes**:

| Mode | Crate | Use Case |
|------|-------|----------|
| **Library** | `crates/core` | Embed conversion in your Rust app |
| **CLI** | `crates/cli` | Offline JSON/SSE conversion |
| **HTTP Proxy** | `crates/server` | Transparent API gateway |

### Core Design Pattern: Pairwise Converters

Each (source, target) format pair has a dedicated converter that translates directly:

```
Client Format JSON ──▶ Pairwise Converter ──▶ Target Format JSON
   (OpenAI)           (direct mapping)          (Claude)
   (Claude)                                     (Gemini)
   (Gemini)           For streaming:            (OpenAI)
   ...                parse → canonical → emit    ...
```

**Benefits:** No data loss from intermediate normalization. Each converter optimized for its specific pair. Streaming uses lightweight canonical events as intermediate.

---

## Step 2: Build & Run (5 minutes)

### Prerequisites

| Tool | Minimum Version | Check Command |
|------|----------------|---------------|
| Rust | 1.85 (2024 edition) | `rustc --version` |

### Quick Start

```bash
# Build everything
cargo build

# Run all tests (156 tests)
cargo test

# Build release binary
cargo build --release

# Quick CLI demo
echo '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}' \
  | any-converter convert --from openai-chat --to claude
```

For full build details, see [`docs/build.md`](./build.md).

---

## Step 3: Understand the Architecture (10 minutes)

Read [`docs/architecture.md`](./architecture.md) **Sections 1 and 2 only** for the big picture.

```
┌─────────────────────────────────────────────────────────────┐
│                        CLI Binary                            │
│                   (argument parsing)                         │
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

| Component | Responsibility | External Dependencies |
|-----------|---------------|----------------------|
| **Conversion Engine** | Parse, transform, serialize LLM API payloads | `serde`, `thiserror` |
| **HTTP Proxy Server** | Accept HTTP requests, convert, forward, convert back | `axum`, `reqwest`, `tokio`, `core` |
| **CLI** | Parse arguments, dispatch to Engine or Server | `clap`, `tokio`, `core`, `server` |

---

## Step 4: Pick Your Component (15 minutes)

| I want to... | Read this | Key files |
|--------------|-----------|-----------|
| Add a new LLM API format | [`crates/core/AGENTS.md`](../crates/core/AGENTS.md) | `crates/core/src/formats/{new}/` |
| Fix streaming or SSE bugs | [`crates/core/AGENTS.md`](../crates/core/AGENTS.md) | `crates/core/src/sse.rs`, `stream.rs` |
| Add server routing/proxy features | [`crates/server/AGENTS.md`](../crates/server/AGENTS.md) | `crates/server/src/handlers.rs`, `proxy.rs` |
| Add CLI commands or config | [`crates/cli/AGENTS.md`](../crates/cli/AGENTS.md) | `crates/cli/src/main.rs` |
| Work on auth or model mapping | [`crates/server/AGENTS.md`](../crates/server/AGENTS.md) | `crates/server/src/auth.rs`, `config.rs` |

---

## Step 5: Before You Code

1. **Read the component's `AGENTS.md`** — It contains domain constraints and common pitfalls.
2. **Read [`docs/memory/known-gotchas.md`](./memory/known-gotchas.md)** — Critical edge cases that will save you debugging time.
3. **Run `cargo test`** before making changes — Establish a baseline.
4. **Run `cargo test`** after making changes — Verify nothing broke.
5. **Review the Maintenance Checklist** in your crate's `AGENTS.md` before finishing.

---

## Reference Map

| Document | What it covers | Read when... |
|----------|---------------|--------------|
| [`docs/architecture.md`](./architecture.md) | Full system architecture (all sections) | You need deep understanding |
| [`docs/build.md`](./build.md) | Build commands, testing, CI/CD | Setting up dev environment |
| [`docs/memory/AGENTS.md`](./memory/AGENTS.md) | Memory system index | Before any code change |
| [`docs/memory/known-gotchas.md`](./memory/known-gotchas.md) | 16 critical edge cases | Before touching relevant code |
| [`docs/memory/project_context.md`](./memory/project_context.md) | 35-point living project state | Returning after a break |
| [`CHANGELOG.md`](../CHANGELOG.md) | Version history with root-cause analysis | Understanding recent changes |
| [`docs/references/api.md`](./references/api.md) | External LLM API documentation links | Implementing format adapters |
| [`docs/references/ascii.md`](./references/ascii.md) | ASCII diagram style rules | Writing diagrams in docs |

---

> **Where to go next?** Pick your component from Step 4 and read its `AGENTS.md`.
