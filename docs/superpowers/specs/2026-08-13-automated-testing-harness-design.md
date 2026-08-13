# Automated Testing Harness Design

> **Status:** Draft for review (doc restored 2026-08-14 after local wipe; implementation not restored)  
> **Date:** 2026-08-13  
> **Parent docs:** [`../../architecture.md`](../../architecture.md), [`../../design/2026-08-05-transparent-proxy.md`](../../design/2026-08-05-transparent-proxy.md), [`../../../tests/AGENTS.md`](../../../tests/AGENTS.md)  
> **Plan:** [`../plans/2026-08-13-automated-testing-harness.md`](../plans/2026-08-13-automated-testing-harness.md)

## 1. Problem

`any-converter` already has substantial unit and mock-upstream integration tests under `crates/core/tests/` and `crates/server/tests/`, plus fixture JSON/SSE under `crates/core/tests/fixtures/`. What is missing is a **confidence ladder** that answers:

1. Does a PR break conversion, proxy, or passthrough behavior? (CI gate)
2. Do Claude Code / Codex / Pi real wire shapes still work after changes? (contract replay)
3. Can a developer prove the local gateway works end-to-end in one command? (smoke)
4. Do real upstream providers still accept converted traffic? (optional live canary)

Without those layers, contributors cannot reliably answer “功能是否正常” beyond unit-level green.

## 2. Goals

1. **Default CI is deterministic**: no network, no real API keys, PR-blocking.
2. **Preserve existing crate-local tests**; extend them rather than relocating into a monolith.
3. **Add a contract / record-replay layer** fed by sanitized captures from real agent tools.
4. **Add a one-command local smoke** that starts the gateway against a mock upstream and exercises Claude Code / Codex / Pi client shapes.
5. **Add opt-in live canaries** gated by env vars, never required for merge.
6. **Document how to record, sanitize, and land new fixtures** so future agents follow the same path.

## 3. Non-Goals

- MITM / local CA testing of tools that cannot set a base URL.
- Full browser E2E of Web/Desktop UI (Playwright against Tauri) in Phase 1.
- Virtual keys, spend budgets, guardrails (product scope, not test harness).
- Replacing pairwise converters with a new IR-based test oracle.
- Paying CI minutes for live provider calls on every PR.

## 4. Design Principles

1. **Test pyramid for an LLM gateway** (industry practice from LiteLLM / Helicone-style proxies):
   - Core converters + stream state machines are the bulk.
   - Server tests use in-process mock upstreams (already the project pattern: spawn `axum` on `127.0.0.1:0`).
   - Contract replay uses recorded bytes, not hand-waved “ideal” JSON alone.
   - Live canaries are rare and explicit.
2. **Tap, never flake**: logging / SQLite failures must not fail smoke that asserts forwarding (matches production tap policy).
3. **Sanitize before commit**: fixtures must never contain API keys, OAuth tokens, absolute home paths, or account emails.
4. **Normalize before golden compare**: strip/replace `id`, `created`, `message_id`, timestamps, and request ids.
5. **Own tests beside owners**: crate tests stay in crate; only cross-domain assets go under `tests/`.

## 5. Architecture

```
PR CI (no network)
  cargo fmt / clippy / test --all-targets
  pnpm test (packages that already have tests)
        │
        ▼
L1 Core matrix / stream / property / fuzz   (existing + gap fills)
L2 Server mock-upstream integration         (existing + gap fills)
L3 Contract replay harness                  (NEW: tests/contracts + runner)
L4 Local smoke script                       (NEW: scripts/e2e-smoke.sh)
        │
nightly / manual (secrets required)
        ▼
L5 Live canary                              (NEW: #[ignore] + workflow_dispatch)
```

### 5.1 Layer responsibilities

| Layer | Location | Runs in PR CI | Needs secrets |
|-------|----------|---------------|---------------|
| L1 Core | `crates/core/tests/` | Yes | No |
| L2 Server | `crates/server/tests/` | Yes | No |
| L3 Contracts | `tests/contracts/` + `crates/server/tests/contract_replay.rs` | Yes | No |
| L4 Smoke | `scripts/e2e-smoke.sh` | Optional job (no secrets) | No |
| L5 Live | `crates/server/tests/live_canary.rs` | No (manual/nightly) | Yes |

### 5.2 Contract fixture layout

```
tests/contracts/
  README.md                 # how to record + sanitize
  AGENTS.md                 # agent routing for this tree
  claude-code/
    messages_simple/
      meta.toml             # client_format, upstream_format, path, mode
      client.request.json
      upstream.response.json          # non-stream
      # OR
      upstream.response.sse           # stream
      expected.client.response.json   # optional; omit when identity/passthrough
      expected.client.response.sse
  codex/
    responses_tools_stream/
      ...
  pi/
    ...
  passthrough/
    arbitrary_path_upload/
      ...
```

`meta.toml` schema:

```toml
name = "messages_simple"
tool = "claude-code"          # claude-code | codex | pi | other
client_format = "claude"      # openai_chat | openai_resp | claude | gemini | unknown
upstream_format = "claude"
path = "/v1/messages"
method = "POST"
mode = "convert"              # convert | identity | passthrough
stream = false
normalize = ["id", "created", "message.id", "response.id"]
```

### 5.3 Contract runner behavior

For each fixture directory:

1. Spawn mock upstream that expects `path` and returns the recorded upstream body (JSON or SSE with correct `Content-Type`).
2. Build `ServerConfig` with one provider pointing at the mock, formats from `meta.toml`, and `passthrough` when `mode = "passthrough"`.
3. Send `client.request.json` (plus headers from optional `client.headers.json`) to the gateway router.
4. Assert HTTP status 200 (unless `meta.toml` sets `expect_status`).
5. Compare client-visible body to `expected.client.response.*` after normalization.
6. When `mode = "passthrough"` or `identity`, also assert upstream received path/query/body bytes matching the client request (modulo auth header replacement rules).

### 5.4 Smoke script behavior

`scripts/e2e-smoke.sh`:

1. Build `any-converter` CLI if missing.
2. Start a tiny Python/Rust mock upstream **or** reuse a prebuilt `any-converter` test binary helper — prefer spawning the same recording pattern as `passthrough_integration.rs` via a small `any-converter smoke-upstream` **only if** adding a CLI subcommand stays tiny; otherwise embed an `axum` binary under `crates/server/examples/mock_upstream.rs`.
3. Write a temp `config.toml` with passthrough + convert routes.
4. `any-converter serve --config $tmp &`
5. Run three curls mirroring `any-converter connect` smoke snippets (Claude Messages, Responses, Chat).
6. Hit `/api/logs` (or SQLite path) and assert ≥1 record with expected `client_tool` when UA is set.
7. Tear down processes; non-zero exit on any failure.

### 5.5 Live canary behavior

`live_canary.rs` tests are `#[ignore]` unless `ANY_CONVERTER_LIVE=1`.

Each canary:

- Requires the relevant `*_API_KEY` env var; skips cleanly if missing when live mode is on but that key is absent.
- Sends one short non-stream completion through the gateway.
- Asserts 2xx and that a parseable response in the **client** format is returned.

Never log raw API keys. Never commit `.env`.

## 6. Gap Fill Requirements (L1/L2)

These are required for the harness to cover known production risks already documented in CHANGELOG / AGENTS.md:

| ID | Requirement |
|----|-------------|
| G1 | Failover: first provider 429/5xx → second provider succeeds |
| G2 | Claude tool_use stream ending with `message_stop` and **no** `message_delta` still emits Responses terminal events |
| G3 | Passthrough logging failure does not break forwarding (already partially covered; keep as regression) |
| G4 | Known API routes still convert when a passthrough provider exists |
| G5 | `client_tool` UA mapping table stays locked (`claude-cli/` → `claude-code`, etc.) |
| G6 | Core: Responses typeless `{role, content}` message items convert to Claude |

## 7. CI Requirements

Add `.github/workflows/ci.yml`:

- Trigger: `pull_request`, `push` to `main`
- Jobs:
  1. `rust`: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`
  2. `js`: `pnpm install`, `pnpm test` (or workspace filter for packages with tests)
  3. `smoke` (optional same workflow, `continue-on-error: false` once stable): run `scripts/e2e-smoke.sh`
- Separate workflow `live-canary.yml`: `workflow_dispatch` + optional `schedule`; secrets for provider keys; runs `cargo test -p any-converter-server --test live_canary -- --ignored`

## 8. Success Criteria

1. Fresh clone + `cargo test --all-targets` + contract fixtures → green without network.
2. `scripts/e2e-smoke.sh` exits 0 on a developer machine without real keys.
3. At least **one** sanitized contract fixture each for `claude-code`, `codex`, and `passthrough` lands in Phase 1.
4. PR CI blocks on rust fmt/clippy/test failures.
5. Live canaries never run on PR.

## 9. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Fixtures leak secrets | Sanitizer checklist in `tests/contracts/README.md`; CI grep for `sk-` / `Bearer ` patterns in fixtures |
| SSE golden brittle | Normalize ids; assert event-type sequences as primary, body golden as secondary |
| Smoke flaky ports | Bind `127.0.0.1:0`, pass port via env file |
| Large fixture repo bloat | Cap each fixture body at 256 KiB; truncate with documented note |
| Scope creep into UI E2E | Explicitly Phase 2+ |

## 10. Phased Delivery

| Phase | Deliverable | Mergeable alone? |
|-------|-------------|------------------|
| P0 | CI workflow + docs routing | Yes |
| P1 | L2 gap tests G1–G6 | Yes |
| P2 | Contract directory + runner + 3 seed fixtures | Yes |
| P3 | `scripts/e2e-smoke.sh` + mock upstream example | Yes |
| P4 | Live canary ignored tests + dispatch workflow | Yes |

## 11. Open Questions (resolved for this draft)

| Question | Decision |
|----------|----------|
| Where do contracts live? | `tests/contracts/` (cross-domain), runner in `crates/server/tests/` |
| Mock upstream for smoke? | `crates/server/examples/mock_upstream.rs` (no new CLI surface unless needed) |
| Commit real captures in Phase 1? | Yes, but hand-authored from existing fixtures first; replace with sanitized real captures when available |
| UI E2E? | Out of scope until P4+ follow-up |

## 12. Documentation Updates Required When Implementing

- `docs/AGENTS.md` — route `docs/superpowers/`
- `tests/AGENTS.md` — point at `tests/contracts/`
- `CHANGELOG.md` — note CI + contract harness as developer-visible
- `docs/build.md` — how to run smoke / live canaries
- `crates/server/AGENTS.md` — contract runner + live canary pitfalls

## Spec Self-Review

- [x] No TBD / “implement later” placeholders in requirements
- [x] Layers, paths, meta schema, success criteria are concrete
- [x] Non-goals and phases prevent scope creep
- [x] Aligns with transparent-proxy tap/passthrough semantics
