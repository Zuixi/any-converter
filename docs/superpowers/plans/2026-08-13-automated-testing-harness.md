# Automated Testing Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Recovery note (2026-08-14):** This plan/spec was restored from conversation transcript after a local workspace wipe. Implementation commits from the prior SDD run were **not** recovered (never pushed). Treat all task checkboxes as unchecked and re-execute from Task 1 on a fresh branch from `origin/main`. Project Cursor hook `.cursor/hooks/block-dangerous-shell.py` now denies agent `rm -rf` / `git clean -fdx` / similar commands — keep that constraint when re-running.

**Goal:** Make “功能是否正常” answerable via deterministic CI, contract replay, a one-command local smoke, and opt-in live canaries — without requiring real API keys on every PR.

**Architecture:** Keep L1/L2 tests beside their crates; add cross-domain fixtures under `tests/contracts/`; drive them with a server integration runner that spawns in-process mock upstreams (existing `axum` + `127.0.0.1:0` pattern). Smoke uses `crates/server/examples/mock_upstream.rs` + `scripts/e2e-smoke.sh`. Live canaries are `#[ignore]` and a separate GitHub workflow.

**Tech Stack:** Rust (`cargo test`, axum, tower::ServiceExt, tokio), GitHub Actions, bash smoke script, optional `pnpm test` for existing frontend unit tests.

**Spec:** [`../specs/2026-08-13-automated-testing-harness-design.md`](../specs/2026-08-13-automated-testing-harness-design.md)

## Global Constraints

- Default CI: **no outbound network**, **no real API keys**.
- Do not relocate existing crate tests into `tests/`; only shared contracts live there.
- Fixtures must be sanitized (no `sk-`, no `Bearer sk-`, no home paths).
- Follow TDD: failing test → implement → pass → commit per task.
- When behavior changes, update related tests/docs/config before finishing (project rule).
- Rust edition / MSRV follow workspace (`edition = "2024"`, `rust-version = "1.85"`).
- Prefer extending `passthrough_integration.rs` / `proxy_integration.rs` helpers over inventing a second mock style.

## File Structure

| Path | Responsibility |
|------|----------------|
| `.github/workflows/ci.yml` | PR gate: fmt, clippy, cargo test, pnpm test, smoke |
| `.github/workflows/live-canary.yml` | Manual/nightly live provider canaries |
| `tests/contracts/**` | Sanitized contract fixtures + README |
| `tests/AGENTS.md` | Route agents to contracts |
| `crates/server/tests/contract_replay.rs` | Load fixtures, mock upstream, assert |
| `crates/server/tests/failover_integration.rs` | G1 failover coverage |
| `crates/server/tests/live_canary.rs` | Ignored live tests |
| `crates/server/examples/mock_upstream.rs` | Standalone mock for smoke script |
| `scripts/e2e-smoke.sh` | One-command local/CI smoke |
| `docs/build.md` | How to run layers |
| `CHANGELOG.md` | Developer-visible harness note |
| `crates/server/AGENTS.md` | Contract/live pitfalls |

---

### Task 1: CI workflow (P0)

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `CHANGELOG.md` (Unreleased — Added bullet for CI)
- Modify: `docs/build.md` (short “CI & tests” subsection)

**Interfaces:**
- Consumes: existing `cargo` / `pnpm` workspace scripts
- Produces: GitHub Actions workflow that fails the PR on rust/js test failure

- [ ] **Step 1: Add the workflow file**

```yaml
# .github/workflows/ci.yml
name: CI

on:
  pull_request:
  push:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: -D warnings

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: fmt
        run: cargo fmt --all -- --check
      - name: clippy
        run: cargo clippy --all-targets --all-features -- -D warnings
      - name: test
        run: cargo test --all-targets

  js:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
        with:
          version: 9
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
      - run: pnpm install --frozen-lockfile
      - name: unit tests
        run: pnpm --filter @any-converter/core test
```

- [ ] **Step 2: Document in `docs/build.md`**

Add a subsection:

```markdown
## CI and automated tests

- PR CI runs `cargo fmt`, `clippy -D warnings`, `cargo test --all-targets`, and package unit tests.
- Local parity: `cargo test --all-targets` then `pnpm --filter @any-converter/core test`.
- Contract fixtures live under `tests/contracts/` (see that README).
- One-command smoke: `scripts/e2e-smoke.sh` (after Task 7).
- Live canaries: `ANY_CONVERTER_LIVE=1 cargo test -p any-converter-server --test live_canary -- --ignored` (after Task 8).
```

- [ ] **Step 3: Changelog bullet**

Under `## Unreleased` Added:

```markdown
- **CI workflow**: Pull requests run `cargo fmt` / `clippy -D warnings` / `cargo test --all-targets` and `@any-converter/core` unit tests.
```

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml docs/build.md CHANGELOG.md
git commit -m "$(cat <<'EOF'
ci: add PR gate for rust and core package tests

EOF
)"
```

---

### Task 2: Failover integration test (G1)

**Files:**
- Create: `crates/server/tests/failover_integration.rs`
- Modify: none unless helpers need extraction

**Interfaces:**
- Consumes: `create_router_with_state`, `AppState`, `ProviderConfig`, `ModelRouteConfig` (or equivalent in `config.rs`)
- Produces: `failover_retries_second_provider_after_429` test

- [ ] **Step 1: Write the failing test**

```rust
// crates/server/tests/failover_integration.rs
#![allow(clippy::unwrap_used)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Router, routing::post};
use reqwest::Client;
use serde_json::json;
use tower::ServiceExt;

fn provider(name: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
        base_url: base_url.into(),
        api_key: "k".into(),
        passthrough: false,
        model_map: Default::default(),
        endpoints: Default::default(),
        auth: Default::default(),
    }
}

#[tokio::test]
async fn failover_retries_second_provider_after_429() {
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));

    let upstream_a = Router::new().route(
        "/v1/chat/completions",
        post({
            let hits_a = hits_a.clone();
            move || {
                let hits_a = hits_a.clone();
                async move {
                    hits_a.fetch_add(1, Ordering::SeqCst);
                    StatusCode::TOO_MANY_REQUESTS
                }
            }
        }),
    );
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener_a, upstream_a).await.unwrap() });

    let upstream_b = Router::new().route(
        "/v1/chat/completions",
        post({
            let hits_b = hits_b.clone();
            move || {
                let hits_b = hits_b.clone();
                async move {
                    hits_b.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "id": "ok",
                        "object": "chat.completion",
                        "model": "gpt-4",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "from-b"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }
        }),
    );
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener_b, upstream_b).await.unwrap() });

    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
        },
        providers: vec![
            provider("a", &format!("http://{addr_a}")),
            provider("b", &format!("http://{addr_b}")),
        ],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: "a".into(),
            // If the real type uses `providers: Vec<String>`, set vec!["a","b"] instead.
            // Adjust field names to match config.rs before compiling.
            providers: vec!["a".into(), "b".into()],
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    };

    let state = Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        usage_logger: None,
        request_logger: None,
    });
    let app = create_router_with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(val["choices"][0]["message"]["content"], "from-b");
}
```

> Implementer note: open `crates/server/src/config.rs` and align `ModelRouteConfig` field names (`provider` vs `providers`, strategy enum) exactly — do not invent fields. If the pool is expressed as comma-separated `provider = "a,b"`, parse that the same way production config does.

- [ ] **Step 2: Run test to verify it compiles/fails for the right reason**

```bash
cargo test -p any-converter-server --test failover_integration -- --nocapture
```

Expected: either compile error until fields match, or test FAIL if failover is broken; fix struct fields first until the test runs.

- [ ] **Step 3: Implement minimal fix only if the test fails for behavioral reasons**

If failover already works, the test should PASS with no production code change. If it fails, fix `handlers.rs` retry loop only enough to satisfy 429 → next provider.

- [ ] **Step 4: Re-run to confirm PASS**

```bash
cargo test -p any-converter-server --test failover_integration
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/failover_integration.rs crates/server/src/handlers.rs
git commit -m "$(cat <<'EOF'
test(server): cover model_route failover after upstream 429

EOF
)"
```

---

### Task 3: Stream terminal-event regression (G2)

**Files:**
- Modify: `crates/core/tests/stream_matrix.rs` **or** add focused test in `crates/core/src/formats/claude/stream.rs` tests module
- Prefer: unit test next to `parse_message_stop` if that is where synthesis lives

**Interfaces:**
- Consumes: Claude `StreamAdapter::parse_sse_event` / `convert_stream_event`
- Produces: test proving Responses clients get terminal events when Claude omits `message_delta`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn claude_message_stop_without_message_delta_synthesizes_done_for_tool_use() {
    use any_converter_core::convert::{Format, convert_stream_event};
    use any_converter_core::ir::stream::StreamState;

    let mut state = StreamState::default();
    // Seed state as mid tool_use (match real StreamState fields in ir/stream.rs).
    // Minimal SSE from MiniMax-style provider: content_block_stop then message_stop, no message_delta.
    let block = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let out = convert_stream_event(Format::Claude, Format::OpenAIResp, block, &mut state)
        .expect("convert");
    let joined = out.join("");
    assert!(
        joined.contains("response.completed")
            || joined.contains("response.function_call_arguments.done")
            || joined.contains("response.output_item.done"),
        "expected Responses terminal event, got: {joined}"
    );
}
```

> Align the “seed state” setup with how `stream_matrix.rs` already builds tool_use mid-stream state. Copy that helper rather than inventing new fields.

- [ ] **Step 2: Run to verify failure mode**

```bash
cargo test -p any-converter-core message_stop_without_message_delta -- --nocapture
```

If already fixed in tree (CHANGELOG claims it is), expect PASS; keep the test as a regression lock.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/stream_matrix.rs crates/core/src/formats/claude/stream.rs
git commit -m "$(cat <<'EOF'
test(core): lock Claude message_stop without message_delta terminal events

EOF
)"
```

---

### Task 4: Known-route convert with passthrough provider present (G4)

**Files:**
- Modify: `crates/server/tests/passthrough_integration.rs`

**Interfaces:**
- Consumes: existing `test_state`, `passthrough_provider`, `spawn_recording_upstream`
- Produces: `known_api_path_still_converts_when_passthrough_provider_exists`

- [ ] **Step 1: Write the failing test at bottom of `passthrough_integration.rs`**

```rust
#[tokio::test]
async fn known_api_path_still_converts_when_passthrough_provider_exists() {
    // Upstream speaks OpenAI Chat; client speaks Claude on /v1/messages.
    // A passthrough provider also exists but must NOT steal /v1/messages.
    let openai_hits = Arc::new(AtomicUsize::new(0));
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post({
            let openai_hits = openai_hits.clone();
            move || {
                let openai_hits = openai_hits.clone();
                async move {
                    openai_hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "id": "chatcmpl-x",
                        "object": "chat.completion",
                        "model": "gpt-4",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });
    let openai_base = format!("http://{addr}");

    let (pass_base, _captured) = spawn_recording_upstream().await;

    let mut openai = ProviderConfig {
        name: "openai".into(),
        format: Format::OpenAIChat,
        base_url: openai_base,
        api_key: "k".into(),
        passthrough: false,
        model_map: Default::default(),
        endpoints: Default::default(),
        auth: Default::default(),
    };
    let pass = passthrough_provider(&pass_base, "pk");
    // Wire routes: Claude client -> openai provider (adjust to real RouteConfig / model_routes API).
    let state = test_state(vec![openai, pass], None);
    // IMPORTANT: after constructing state, set routes/model_routes so Claude -> openai resolves.
    // Prefer editing test_state helper or building AppState explicitly like proxy_integration.rs.

    let app = create_router_with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/messages")
                .method("POST")
                .header("Content-Type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    r#"{"model":"gpt-4","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(openai_hits.load(Ordering::SeqCst), 1);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(val["type"], "message");
    assert_eq!(val["role"], "assistant");
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p any-converter-server --test passthrough_integration known_api_path_still_converts -- --nocapture
```

- [ ] **Step 3: Fix router only if known paths incorrectly fall into passthrough**

- [ ] **Step 4: Commit**

```bash
git add crates/server/tests/passthrough_integration.rs
git commit -m "$(cat <<'EOF'
test(server): keep known API routes converting when passthrough exists

EOF
)"
```

---

### Task 5: Contract fixture tree + README (P2 scaffolding)

**Files:**
- Create: `tests/contracts/README.md`
- Create: `tests/contracts/AGENTS.md`
- Create: three seed fixture directories (hand-authored from existing core fixtures first)
- Modify: `tests/AGENTS.md`

**Interfaces:**
- Consumes: shapes from `crates/core/tests/fixtures/**`
- Produces: on-disk contract layout matching the spec `meta.toml` schema

- [ ] **Step 1: Write `tests/contracts/AGENTS.md`**

```markdown
# Contract Fixtures

> **Parent:** [`../AGENTS.md`](../AGENTS.md)

Sanitized record-replay fixtures for gateway contract tests.
Runner: `crates/server/tests/contract_replay.rs`.
Do not store secrets. Prefer copying from `crates/core/tests/fixtures/` then adapting.
```

- [ ] **Step 2: Write `tests/contracts/README.md`** with record/sanitize checklist

Must include:

1. Capture via request log or `curl -v` through local gateway  
2. Redact `authorization`, `x-api-key`, emails, absolute paths  
3. Fill `meta.toml`  
4. Run `cargo test -p any-converter-server --test contract_replay`  
5. CI forbids raw `sk-ant-` / `sk-proj-` strings under `tests/contracts/`

- [ ] **Step 3: Seed fixtures**

Create at least:

1. `tests/contracts/claude-code/messages_simple/` — Claude request + Claude upstream response (identity), copied from `crates/core/tests/fixtures/claude/request_simple.json` + `response_simple.json`
2. `tests/contracts/codex/responses_simple/` — Responses simple request/response from `openai_resp` fixtures
3. `tests/contracts/passthrough/arbitrary_path/` — small JSON body + `meta.toml` with `mode = "passthrough"`, `path = "/v1beta/files/upload"`

Example `meta.toml` for (1):

```toml
name = "messages_simple"
tool = "claude-code"
client_format = "claude"
upstream_format = "claude"
path = "/v1/messages"
method = "POST"
mode = "identity"
stream = false
normalize = ["id", "message.id"]
```

For identity fixtures, `expected.client.response.json` can equal `upstream.response.json` after normalization.

- [ ] **Step 4: Update `tests/AGENTS.md`**

Add routing row to `tests/contracts/`.

- [ ] **Step 5: Commit**

```bash
git add tests/contracts tests/AGENTS.md
git commit -m "$(cat <<'EOF'
test: scaffold contract fixture tree with seed captures

EOF
)"
```

---

### Task 6: Contract replay runner

**Files:**
- Create: `crates/server/tests/contract_replay.rs`
- Create: `crates/server/tests/contract_support/mod.rs` (optional if file grows)

**Interfaces:**
- Consumes: `tests/contracts/**/meta.toml` + body files
- Produces: one `#[tokio::test]` per discovered fixture **or** a single test that walks the directory tree

Recommended approach (simpler CI): one test `all_contract_fixtures_pass` that walks `CARGO_MANIFEST_DIR/../../tests/contracts`.

- [ ] **Step 1: Write failing runner skeleton**

```rust
// crates/server/tests/contract_replay.rs
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Meta {
    name: String,
    tool: String,
    client_format: String,
    upstream_format: String,
    path: String,
    method: String,
    mode: String,
    stream: bool,
    #[serde(default)]
    normalize: Vec<String>,
    #[serde(default = "default_status")]
    expect_status: u16,
}

fn default_status() -> u16 {
    200
}

fn contracts_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contracts")
}

fn discover_cases(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_name() == "meta.toml" {
            out.push(entry.path().parent().unwrap().to_path_buf());
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn all_contract_fixtures_pass() {
    let root = contracts_root();
    let cases = discover_cases(&root);
    assert!(
        !cases.is_empty(),
        "expected fixtures under {}",
        root.display()
    );
    for dir in cases {
        run_case(&dir).await;
    }
}

async fn run_case(dir: &Path) {
    let meta: Meta = toml::from_str(&std::fs::read_to_string(dir.join("meta.toml")).unwrap())
        .unwrap_or_else(|e| panic!("meta.toml in {}: {e}", dir.display()));
    // 1) read client.request.json
    // 2) spawn mock upstream returning upstream.response.json or .sse
    // 3) build ServerConfig from meta.mode / formats
    // 4) oneshot gateway request
    // 5) normalize + assert against expected.client.response.*
    let _ = meta;
    panic!("not implemented: {}", dir.display());
}
```

If `walkdir` / `toml` are not workspace deps, either:

- add them as **dev-dependencies** of `any-converter-server`, or
- implement discovery with `std::fs` recursion and parse `meta.toml` via a tiny hand parser / `toml` already used by server config (`ServerConfig::from_toml` stack). Prefer reusing the crate’s existing `toml` dependency.

- [ ] **Step 2: Run — expect FAIL `not implemented`**

```bash
cargo test -p any-converter-server --test contract_replay -- --nocapture
```

- [ ] **Step 3: Implement `run_case` for non-stream JSON first**

Minimal behavior:

1. Mock upstream on `meta.path` returns file bytes + `content-type: application/json`
2. Provider format from `meta.upstream_format`
3. Route client format from `meta.client_format` to that provider (identity/convert)
4. For `mode = "passthrough"`, set `passthrough = true` and hit an unmatched path
5. Compare JSON with normalize keys set to `"<normalized>"`

Normalization helper:

```rust
fn normalize_json(mut v: serde_json::Value, keys: &[String]) -> serde_json::Value {
    // Walk object/array; if key path matches or key name is in keys, replace with "<normalized>"
    fn walk(v: &mut serde_json::Value, keys: &[String]) {
        match v {
            serde_json::Value::Object(map) => {
                let names: Vec<String> = map.keys().cloned().collect();
                for k in names {
                    if keys.iter().any(|n| n == &k || n.ends_with(&format!(".{k}"))) {
                        map.insert(k.clone(), serde_json::json!("<normalized>"));
                    } else if let Some(child) = map.get_mut(&k) {
                        walk(child, keys);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, keys);
                }
            }
            _ => {}
        }
    }
    walk(&mut v, keys);
    v
}
```

- [ ] **Step 4: Extend runner for `stream = true` SSE**

Assert: split on `\n\n`, compare event type lines sequence; optionally compare full normalized SSE text.

- [ ] **Step 5: Add fixture secret grep to CI**

In `.github/workflows/ci.yml` rust job:

```yaml
- name: forbid secrets in contract fixtures
  run: |
    if rg -n "sk-ant-|sk-proj-|sk-[a-zA-Z0-9]{20,}" tests/contracts; then
      echo "Possible secret in fixtures" >&2
      exit 1
    fi
```

- [ ] **Step 6: Full pass**

```bash
cargo test -p any-converter-server --test contract_replay
```

Expected: PASS for all seed fixtures

- [ ] **Step 7: Update `crates/server/AGENTS.md`** with a “Contract replay” bullet under pitfalls/testing

- [ ] **Step 8: Commit**

```bash
git add crates/server/tests/contract_replay.rs crates/server/Cargo.toml .github/workflows/ci.yml crates/server/AGENTS.md tests/contracts
git commit -m "$(cat <<'EOF'
test(server): add contract replay runner for sanitized fixtures

EOF
)"
```

---

### Task 7: Mock upstream example + e2e smoke script (P3)

**Files:**
- Create: `crates/server/examples/mock_upstream.rs`
- Create: `scripts/e2e-smoke.sh`
- Modify: `.github/workflows/ci.yml` (add smoke job)
- Modify: `docs/build.md`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: CLI `any-converter serve`
- Produces: exit code 0 smoke proving convert + passthrough paths

- [ ] **Step 1: Write `mock_upstream.rs`**

```rust
// crates/server/examples/mock_upstream.rs
// Listen on PORT env or 9090. Record POST /v1/chat/completions and /v1/messages
// and unmatched paths; return fixed JSON/SSE bodies good enough for smoke curls.

use axum::{Router, routing::post, Json};
use serde_json::json;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9090);
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "chatcmpl-smoke",
                    "object": "chat.completion",
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "smoke-ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            }),
        )
        .route(
            "/v1/messages",
            post(|| async {
                Json(json!({
                    "id": "msg_smoke",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4",
                    "content": [{"type": "text", "text": "smoke-ok"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
            }),
        )
        .fallback(axum::routing::any(|| async { "passthrough-ok" }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    eprintln!("mock upstream on http://127.0.0.1:{port}");
    axum::serve(listener, app).await.expect("serve");
}
```

Ensure `any-converter-server` `Cargo.toml` allows examples (default). Add deps already present (`axum`, `tokio`, `serde_json`).

- [ ] **Step 2: Run example manually once**

```bash
cargo run -p any-converter-server --example mock_upstream
```

Expected: listens and prints port line.

- [ ] **Step 3: Write `scripts/e2e-smoke.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

UP_PORT="${UP_PORT:-19090}"
GW_PORT="${GW_PORT:-18080}"
TMP="$(mktemp -d)"
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$TMP"' EXIT

cargo build -p any-converter-server --example mock_upstream
cargo build -p any-converter

PORT="$UP_PORT" cargo run -p any-converter-server --example mock_upstream >/dev/null &
sleep 0.5

cat >"$TMP/config.toml" <<EOF
[server]
host = "127.0.0.1"
port = $GW_PORT
api_key = "smoke-key"

[[providers]]
name = "mock"
format = "openai_chat"
base_url = "http://127.0.0.1:$UP_PORT"
api_key = "upstream"
passthrough = true

[[model_routes]]
pattern = "*"
provider = "mock"

[logging]
dir = "$TMP/logs"

[logging.request_log]
enabled = true
EOF

cargo run -p any-converter -- serve --config "$TMP/config.toml" >/dev/null &
sleep 0.8

curl -sf "http://127.0.0.1:$GW_PORT/health" >/dev/null

curl -sf "http://127.0.0.1:$GW_PORT/v1/chat/completions" \
  -H "Authorization: Bearer smoke-key" \
  -H "Content-Type: application/json" \
  -H "User-Agent: codex_cli_rs/0.0.0" \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}' \
  | grep -q smoke-ok

curl -sf "http://127.0.0.1:$GW_PORT/v1beta/files/upload?x=1" \
  -H "Authorization: Bearer smoke-key" \
  -H "Content-Type: application/json" \
  -d '{"file":"x"}' \
  | grep -q passthrough-ok

echo "e2e-smoke OK"
```

- [ ] **Step 4: Make executable and run**

```bash
chmod +x scripts/e2e-smoke.sh
scripts/e2e-smoke.sh
```

Expected: `e2e-smoke OK`

- [ ] **Step 5: Wire CI smoke job**

```yaml
  smoke:
    runs-on: ubuntu-latest
    needs: rust
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: scripts/e2e-smoke.sh
```

- [ ] **Step 6: Docs + changelog + commit**

```bash
git add crates/server/examples/mock_upstream.rs scripts/e2e-smoke.sh .github/workflows/ci.yml docs/build.md CHANGELOG.md
git commit -m "$(cat <<'EOF'
test: add mock upstream example and e2e smoke script

EOF
)"
```

---

### Task 8: Live canary tests + dispatch workflow (P4)

**Files:**
- Create: `crates/server/tests/live_canary.rs`
- Create: `.github/workflows/live-canary.yml`
- Modify: `docs/build.md`, `CHANGELOG.md`, `crates/server/AGENTS.md`

**Interfaces:**
- Consumes: env `ANY_CONVERTER_LIVE=1`, `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GOOGLE_API_KEY`
- Produces: ignored tests never run in PR CI

- [ ] **Step 1: Write ignored canary**

```rust
// crates/server/tests/live_canary.rs
#![allow(clippy::unwrap_used)]

fn live_enabled() -> bool {
    std::env::var("ANY_CONVERTER_LIVE").ok().as_deref() == Some("1")
}

#[tokio::test]
#[ignore = "live canary; run with ANY_CONVERTER_LIVE=1 -- --ignored"]
async fn live_openai_chat_through_gateway() {
    if !live_enabled() {
        return;
    }
    let key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return,
    };
    // Spawn gateway with provider base_url https://api.openai.com using `key`,
    // POST /v1/chat/completions with a tiny prompt, assert 2xx and choices[0].message.content is string.
    let _ = key;
    todo!("implement with same AppState pattern as proxy_integration, real URL");
}
```

Replace `todo!` with a real implementation using `create_router_with_state` and `reqwest`/`oneshot`. Keep prompts tiny (`max_tokens`/`max_completion_tokens` minimal).

Add sibling ignored tests for Anthropic and Gemini when keys exist.

- [ ] **Step 2: Confirm default CI does not run them**

```bash
cargo test -p any-converter-server --test live_canary
```

Expected: `0 passed, 0 failed, N ignored` (or filtered)

- [ ] **Step 3: Add workflow**

```yaml
# .github/workflows/live-canary.yml
name: Live Canary
on:
  workflow_dispatch:
  schedule:
    - cron: "0 6 * * 1"

jobs:
  canary:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: run canaries
        env:
          ANY_CONVERTER_LIVE: "1"
          OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
          GOOGLE_API_KEY: ${{ secrets.GOOGLE_API_KEY }}
        run: cargo test -p any-converter-server --test live_canary -- --ignored --nocapture
```

- [ ] **Step 4: Docs + commit**

```bash
git add crates/server/tests/live_canary.rs .github/workflows/live-canary.yml docs/build.md CHANGELOG.md crates/server/AGENTS.md
git commit -m "$(cat <<'EOF'
test(server): add opt-in live provider canaries

EOF
)"
```

---

### Task 9: Docs routing for superpowers + final verification

**Files:**
- Create: `docs/superpowers/AGENTS.md` (if not already)
- Modify: `docs/AGENTS.md`
- Modify: root `AGENTS.md` only if workspace routing needs a one-line pointer

**Interfaces:**
- Produces: agents can find specs/plans via docs routing matrix

- [ ] **Step 1: Ensure `docs/superpowers/AGENTS.md` exists**

```markdown
# Superpowers Specs & Plans

> **Parent:** [`../AGENTS.md`](../AGENTS.md)

| Path | Purpose |
|------|---------|
| `specs/` | Approved design specs |
| `plans/` | Bite-sized implementation plans |

When executing a plan, use subagent-driven-development or executing-plans.
```

- [ ] **Step 2: Add row to `docs/AGENTS.md` Task Routing Matrix**

```markdown
| Superpowers specs or implementation plans | [`superpowers/AGENTS.md`](./superpowers/AGENTS.md) |
```

- [ ] **Step 3: Run full local verification**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
scripts/e2e-smoke.sh
```

Expected: all green

- [ ] **Step 4: Commit docs-only if needed**

```bash
git add docs/superpowers/AGENTS.md docs/AGENTS.md
git commit -m "$(cat <<'EOF'
docs: route superpowers specs and plans

EOF
)"
```

---

## Self-Review (plan vs spec)

| Spec section | Task coverage |
|--------------|---------------|
| §5 L1/L2 gap fills G1–G6 | Tasks 2–4 (G1/G2/G4); G3 already in passthrough tests — re-run only; G5 covered by existing UA tests — add assertion in Task 6 logs if missing; G6 add under Task 3 if not already in core |
| §5.2–5.3 Contracts | Tasks 5–6 |
| §5.4 Smoke | Task 7 |
| §5.5 Live | Task 8 |
| §7 CI | Tasks 1, 6 (secret grep), 7 (smoke job), 8 (dispatch) |
| §8 Success criteria | Task 9 verification |
| §12 Doc updates | Tasks 1, 5, 6, 7, 8, 9 |

**Placeholder scan:** Task 2/4 call out aligning `ModelRouteConfig` field names with `config.rs` at implement time (repo types may differ slightly). No TBD left for behavior.

**Type consistency:** `ProviderConfig.passthrough`, `AppState`, `create_router_with_state` match current server tests.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-13-automated-testing-harness.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — execute tasks in this session with checkpoints  

Which approach?
