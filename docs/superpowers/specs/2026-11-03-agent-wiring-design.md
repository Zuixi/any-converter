# Agent Wiring — Design Spec

Date: 2026-11-03
Status: Approved (design review complete)
Branch: `feature/agent-wiring`

## Goal

One-command connect/disconnect for Claude Code, Codex, Gemini CLI, and Pi onto the
any-converter gateway: full catalog sync, byte-faithful restore, per-agent gateway
keys, driven from CLI and WebUI/Desktop through a single server-owned REST API.

## Confirmed Decisions (from brainstorming)

1. **Full catalog sync (A)** for all four agents.
2. **magpie-grade restore fidelity (A)**: byte backup + semantic extraction +
   format-preserving editors.
3. **Server owns all writes (A)**: `/v1/agents/*` is the only write path; CLI is a
   thin HTTP client; offline rescue = manual `.before-any-converter` copy (documented).
4. **Lazy catalog sync (C)**: startup + manual + request-path digest check that
   spawns async sync (never blocks the request).
5. **Per-agent gateway keys (B)**: `acw_<agent>_…` keys stored in the existing
   FileSecretStore; attribution without relying on User-Agent; individually revocable.

## §1 Architecture

```
CLI (thin HTTP client) / WebUI (Next.js proxy) / Desktop (in-process)
        │  all via /v1/agents/* REST (the only write entry)
        ▼
server: agents.rs (handlers) + agent_registry (global write mutex, key refs only)
        ▼
new crate agent-wiring (plain std::fs; tempdir tests; no trait Fs)
  spec/{claude,codex,gemini,pi}.rs   edit/{json,toml,env}.rs
  backup/{snapshot,stash,applied}.rs sync/catalog.rs         wiring.rs (orchestration)
        ▼
reused: agent_import (path discovery)  FileSecretStore (key storage)
        agent_id.rs (UA attribution fallback)
```

- `agent-wiring` is a pure library crate: no tokio/axum deps; all file ops via
  std::fs; tests use `tempfile::TempDir` (real semantics: fsync/symlink).
- State: `agent_state.json` (status, key ref, catalog digest) — atomic writes.
- Secrets live ONLY in FileSecretStore; state file stores `agent_key_id` references.
- Keys never cross the crate boundary as store handles: server resolves the ref and
  passes the key value into `WiringCtx`. Key lifecycle (create/revoke/rotate) is a
  server-layer concern; agent-wiring only consumes the value.
- WebUI proxy injects the global key server-side (never reaches the browser).
- CLI failure when server is down must print an action hint (`any-converter serve`)
  and, for disconnect, the manual `.before-any-converter` rescue path.

## §2 Data Model & Module Boundaries

### Dependency rules (hard)

```
spec/*  → edit/ + backup/ public types only (never each other, never wiring/sync)
wiring.rs → AgentSpec + backup + edit (never a concrete agent's internals)
edit/   → nothing (pure bytes→bytes)
backup/ → edit only (read-back validation)
sync/   → renders catalog blocks as bytes (no disk writes)
```

Adding a 5th agent = one new spec file + one registration line. Open/closed.

### AgentSpec (per-agent knowledge, one file)

```rust
pub struct AgentSpec { id, detect(), config_files(), connect(WiringCtx), disconnect(WiringCtx), read_state() }
pub struct WiringCtx { gateway_url, agent_key: &str, catalog: &ModelCatalog, default_model }
```

Cohesion rule: everything about Claude Code lives in `spec/claude.rs`.

| Module | Single responsibility | Never does |
|---|---|---|
| edit/json | JSONC→JSON, comment/orden-preserving set/get/del | know agents/backups |
| edit/toml | toml_edit table/key ops | idem |
| edit/env | dotenv preserve (comments/order/quotes) | idem |
| backup/snapshot | `.before-any-converter` take/restore/cleanup; idempotent take | understand our-keys |
| backup/stash | pre-connect replaced values, per-agent namespace | write agent files |
| backup/applied | our last-written KV + drift compare | decide restores |
| sync/catalog | ModelCatalog → per-spec block render + digest | write to disk |
| wiring | connect/disconnect/sync state machine | agent-specific logic |

### State files

`agent_state.json` (atomic write):
```json
{ "version": 1, "agents": { "claude-code": {
  "status": "connected|disconnected|drifted",
  "agent_key_id": "ak_claude-code_01",
  "connected_at": "...", "default_model": "...",
  "catalog_digest": "sha256:...", "stash_ref": "claude-code" } } }
```

`agent_stash.json` (separate file; recovery data lifecycle ≠ display state):
`{ "version": 1, "<agent>": { "<key>": "<old value>" } }`

Separation rationale: stash loss = cannot perfectly restore (needs durable backup);
applied loss = only loses drift detection. Different failure costs → different files.

### Corrections from strict review (binding)

1. **Drift detection is OFF the request hot path.** Requests do in-memory key
   lookup + digest compare only; mismatch spawns an async sync task. Drift checks
   run lazily when `GET /v1/agents` is called (UI polling cadence).
2. **snapshot take() is idempotent** — an existing backup is never overwritten
   (prevents losing the pristine pre-connect state on double connect).
3. **Disconnect restore order (three steps, wiring-enforced, specs cannot bypass):**
   1. semantic extraction of our-keys;
   2. iff extracted result (semantic DeepEqual) == backup (semantic DeepEqual)
      → byte-restore the backup (perfect path, user formatting returns);
   3. else keep the extracted result (user's mid-connect edits survive).
4. **All agent writes serialize** through one `tokio::sync::Mutex` in
   agent_registry (in-process concurrency: double-click, poll-vs-disconnect).

### Performance budget

| Path | Added cost |
|---|---|
| proxy request hot path | in-memory key lookup + digest compare, < 1µs |
| lazy sync trigger | spawn async task; request never blocks |
| GET /v1/agents (UI poll) | ≤ ~12 small file stats/reads, < 5ms local SSD (mtime cache reserved, not in v1) |
| connect/disconnect | < 100ms interactive |
| resident memory | < 1MB state+keys+catalog |

## §3 Agent Specs (post-red-team corrections included)

Shared rules (enforced by wiring):
- Before writing any key, consult stash: has-old-value → restore mode; none →
  add mode (disconnect deletes the key rather than blanking it).
- Each spec exposes `const OUR_KEYS: &[&str]` — single source of truth for
  extraction, applied snapshot, and drift comparison.
- Default model choice: if the agent's current model exists in the catalog keep
  it; else first available; user `--model` overrides.

### 3.1 Claude Code — `~/.claude/settings.json` (JSONC)

Write (format-preserving): `env.ANTHROPIC_BASE_URL` = gateway, `env.ANTHROPIC_AUTH_TOKEN`
= gateway key, four tier model fields (`ANTHROPIC_MODEL`, `…_HAIKU/SONNET/OPUS_MODEL`)
mapped from catalog (tier missing in catalog → key untouched).

- Dual-key rule: a pre-existing `ANTHROPIC_API_KEY` is stashed and removed on
  connect (restored on disconnect). `apiKeyHelper` present → warn, do not override.
- Catalog sync = recompute the 4 tier fields (write only when a value changes).
- Known limitation (documented, not handled): project-level `settings.local.json`
  overrides user-level settings.

### 3.2 Codex — `~/.codex/config.toml` + `~/.codex/auth.json` + `~/.codex/any-converter-models.json`

**Correction (red-team finding 1):** the provider table must use
`requires_openai_auth = true` and the gateway key goes into `auth.json`'s
`OPENAI_API_KEY` field (OAuth tokens are never touched; both magpie and cc-switch
do this — with `false` and no env_key Codex sends no Authorization header and
every request 401s).

Write: `model_provider = "any-converter"`, `model = <default>`,
`[model_providers.any-converter]` (name, base_url `<gw>/v1`, wire_api "responses",
requires_openai_auth true), standalone catalog file
`any-converter-models.json` + `model_catalog_json` key pointing at it.

- All three files enter `config_files()` for backup.
- Multi-home: `codex_homes()` discovery; connect defaults to the main home,
  optional `codexHome` selects another; each home wires independently.
- Extraction: restore stashed `model_provider`/`model` (delete if absent), remove
  the provider table, catalog file, and `model_catalog_json` key; restore stashed
  `OPENAI_API_KEY` in auth.json (delete ours if none).

### 3.3 Gemini CLI — `~/.gemini/.env` + `~/.gemini/settings.json`

**Correction (red-team finding 2):** if `security.auth.selectedType` in
settings.json is an OAuth mode, connect rewrites it to `"GEMINI_API_KEY"`
(stashed + restored); otherwise untouched. settings.json enters the backup list.

.env write: `GOOGLE_GEMINI_BASE_URL`, `GEMINI_API_KEY` (gateway key), `GEMINI_MODEL`.
Sync: ensure GEMINI_MODEL exists in catalog, else downgrade to first model + notify.
Dual-key: legacy `GOOGLE_API_KEY` handled like 3.1's `ANTHROPIC_API_KEY`.

### 3.4 Pi — `~/.pi/agent/settings.json` + `~/.pi/agent/models.json`

Respect `PI_CODING_AGENT_DIR` with `~` expansion (magpie semantics).

models.json: `providers.any-converter` block with full catalog (name, baseUrl
`<gw>/v1`, apiKey, models map with name/reasoning/context/thinkingLevelMap per
pi-ai schema). settings.json: `defaultProvider = "any-converter"`, `defaultModel`,
and append our model refs to `enabledModels` when that list exists (magpie lesson:
a default outside the enabled list never starts).

Extraction: restore/remove the three settings keys; remove the provider block;
strip `any-converter/` prefixed entries from `enabledModels` (delete the key when
the list becomes empty — never write an empty array).

## §4 REST API & Three Clients

Endpoints (mounted on the existing router):

| Method Path | Purpose |
|---|---|
| GET /v1/agents | detect + status (connected/disconnected/drifted/absent) + model + digest; runs lazy drift checks |
| POST /v1/agents/{id}/connect | body `{model?, codexHome?}` |
| POST /v1/agents/{id}/disconnect | three-step restore |
| POST /v1/agents/{id}/sync | manual catalog sync |
| POST /v1/agents/{id}/key/rotate | rotate the per-agent key (rewrites agent config) |
| GET /v1/agents/{id}/keys | key metadata only (id, created, last used) — never plaintext |

Auth: `validate_client_key` extended to a key SET = {global server key} ∪
{agent keys}; single-key deployments behave identically. Write endpoints accept
the GLOBAL key only (agent keys are inference-only — blocks prompt-injection
self-tampering from inside an agent process).

Errors (uniform JSON shared by CLI/WebUI):
`{ "error": { "code": "agent_absent|server_no_providers|config_unwritable|stash_missing|model_not_found", "message", "hint", "agent" } }`

CLI: `any-converter agents list|connect|disconnect|sync|rotate-key|status` — clap
subcommand beside `Accounts`; server-down errors carry the action hint +
manual-backup rescue text for disconnect; `connect` prints written files + keys.

WebUI: `/agents` page (card per agent: status badge, model, actions), connect
modal with model picker (from `/v1/models`) and codex-home selector, inline
drifted → "Reapply" (= connect rerun), server-side key injection in the proxy
route, 10s visible-poll.

Desktop: zero new Tauri commands (embedded server has everything); tray/window
entry opens the WebUI `/agents` page in the webview. Desktop SQLite is NOT
involved in agent state (single source of truth: server's agent_state.json).

## §5 Error Handling, Testing, Delivery

### Failure behavior (design decisions)

| Scenario | Behavior |
|---|---|
| backup fails at connect | abort connect, write nothing |
| mid-connect write failure | in-memory transforms first, then per-file atomic commit (temp+fsync+rename) |
| stash missing at disconnect | degrade: extraction-only, return `stash_missing` warning |
| backup file corrupt at disconnect | degrade; corrupt copy moved to `.before-any-converter.bad-<ts>` (never overwritten) |
| sync write failure | keep old block, warn, never half-written |
| crash mid-write | temp+rename ⇒ old-or-new, never partial |
| agent config missing at connect | create minimal skeleton; before-* marks absent; disconnect deletes back to absent |

Errors: one `WiringError` enum crossing the crate boundary (no internal module
paths leak); server maps to JSON codes. No anyhow across crate boundaries.

### Testing pyramid

- L1 (agent-wiring, largest): edit/ 15–25 snapshot cases per format (comments,
  order, CRLF, BOM, JSONC trailing commas, duplicate keys, corrupt-input-refuse);
  backup idempotency + three-step-restore branches + bad-file preservation;
  per-spec detect path matrix (env vars) and "user content zero-loss" extraction
  assertions. All TempDir real FS.
- L2 (crates/server/tests, follows agent_import_e2e.rs conventions):
  agents_api_e2e (connect → snapshot agent files → disconnect → byte-identical);
  auth matrix (global ✓ inference, agent key ✓ inference, agent key ✗ management,
  no key ✗); concurrent connect serialization; lazy sync updates catalog async
  without blocking the request.
- L3 (optional smoke script): real installed agents, full flow, manual trigger.

### Delivery (4 independently mergeable PRs)

1. agent-wiring crate (edit/backup/spec/wiring + full L1) — no deps
2. server integration (multi-key auth, /v1/agents/*, registry, lazy sync) — PR1
3. CLI agents commands — PR2
4. WebUI /agents + Desktop entry — PR2 (parallel with PR3)

Acceptance: full-catalog connect on 4 agents; byte-identical restore after
disconnect (L2 asserted); request path never blocks; per-agent keys end-to-end;
three clients usable.

## Constraints for Implementation

- Hard limit: **800 LOC per file** (split by module boundary when hit).
- High cohesion / low coupling per §2 rules; violations are review blockers.
- Every phase ends with monitor review: scope diff, LOC check, acceptance
  commands re-run by monitor, `cargo test -p` targeted suites green.

## Errata (post-merge, 2026-11-04)

**§4.4 "Desktop: zero new Tauri commands; tray/window entry opens the WebUI
`/agents` page in the webview; Desktop reuses the WebUI page" was wrong.**

Desktop (`apps/desktop`) is its own Vite SPA — its own sidebar, HashRouter,
pages under `src/pages/`, and Tauri IPC through `src/lib/api.ts` — it does
not load the Next.js WebUI at all. The assumption held nowhere in this
codebase, and the agent-wiring PR shipped with a WebUI `/agents` page but
no Desktop entry: installed desktop builds (including the first Windows
release) showed no Agents tab. Caught by user testing, fixed in PR #5
(`feat(desktop): Agents page`): a native `AgentsPage` speaking to the
embedded server's `/v1/agents` over loopback with the settings-injected
API key (AccountsPage conventions), plus sidebar/route/i18n entries.

Two lessons recorded for future specs:

1. When implementation discovers a design premise is false (here: within
   days of writing §4.4), the premise must go back to the decision table
   and be re-confirmed — not silently resolved as "no change needed".
   The codebase fact (two independent frontends) was even noted during
   research but never reconciled with the spec text.
2. "Client X reuses client Y's UI" claims need a one-line verification
   against how the client actually loads UI (webview URL? embedded SPA?
   native windows?) before they enter a spec.

`codexHome` API field remains reserved-but-unwired as stated in §4.1.
