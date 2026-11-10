# Unified Gateway UI + Daemon Plan

> **Product position:** B — any-converter is a headless-capable local gateway.
> The browser is the primary control surface; the desktop app is one client of
> the same gateway. There is one UI build, one data API, one management API,
> and one gateway process per home.
>
> **Reference:** `../magpie` — `internal/gui/web.go`, `apppage.go`,
> `daemon/{daemon,launchd,systemd}.go`, `autostart/*`, `backend.go`.
>
> **Status:** accepted for implementation. Supersedes the separate-process
> Next.js assumptions in `docs/build.md` and the SQLite-as-data-API
> assumptions in `desktop-resident-gateway.md`.

## 1. Goals

1. `any-converter serve` serves the same React SPA that the desktop window
   loads, on the same HTTP listener as the inference and management API.
2. The browser is a first-class management client. It authenticates with a
   page session, never with the management key.
3. The desktop app attaches to an already-running gateway instead of starting
   a second one. It remains able to run the gateway in-process when no daemon
   is running.
4. A user-session daemon starts the gateway at login on macOS and Linux.
   It is opt-in, per-user, and never a system service.
5. All UI data (`logs`, `usage`, `heatmap`, ledger, CSV, status, conversion)
   comes from the Rust server. No second SQLite reader, no second SQL rollup,
   no second conversion bridge.
6. A release build can embed the prebuilt SPA into the CLI binary. A plain
   `cargo build` still works without Node.

## 2. Non-goals

- TLS termination and LAN/multi-user hosting. Cookie auth is loopback-only;
  TLS can be added in front without changing the contract.
- Windows service installation (documented as unsupported in this phase).
- A GUI (wry) single binary. The desktop shell remains Tauri; the gateway
  binary is headless.
- Public plugin/marketplace surfaces.

## 3. Target topology

```
                 ┌────────────────────────────────────────────┐
  browser ──────►│  any-converter serve (one process, one home)│
  (session      │  ├─ /                 SPA (embedded or dir) │
   cookie)      │  ├─ /assets/*         hashed static assets   │
                 │  ├─ /v1/admin/*       management + data API  │
  CLI / agents ─►│  ├─ /v1/session       page session          │
  (bearer/      │  ├─ /v1/*             inference gateway     │
   inference)   │  └─ /health                                │
                 └──────────────┬─────────────────────────────┘
                                │ loopback HTTP + bearer
                 ┌──────────────┴─────────────┐
                 │ Tauri desktop (one client) │
                 │ server_request -> Rust HTTP│
                 │ loads the same SPA dist    │
                 └────────────────────────────┘
```

Process rules:

- One managed gateway per `Home`, enforced by `instance.lock`.
- `instance.json` describes the live instance: pid, started time, config
  revision, ownership token, host, port, server version.
- Desktop startup: attach if `instance.json` + `/health` agree; otherwise
  start the in-process gateway.
- Desktop never stops an attached gateway. Quitting the desktop leaves the
  daemon running.
- `daemon stop` unloads the service, so a later desktop start has no race.

## 4. Authentication model

Three principals, three credentials, no reuse:

| Principal | Credential | Routes |
|---|---|---|
| Inference client | inference key (`Authorization: Bearer`) | `/v1/*` inference routes |
| Operator/script | management key (`Authorization: Bearer`) | `/v1/admin/*`, `/v1/accounts*`, `/v1/agents*`, `/v1/usage` |
| Browser page | page key -> `any-converter-session` cookie | management routes + UI |

### 4.1 Page key

Resolution order at startup:

1. `ANY_CONVERTER_WEB_KEY`, validated (`[A-Za-z0-9._~-]{16,}`).
2. `<home>/web-key`, a `0600` file created by `daemon install` or by the user.
3. A random 128-bit hex key (32 hex characters, from a UUIDv4) for this run
   (not persisted).

The resolved key is printed once at `INFO` after bind:

```
UI: http://127.0.0.1:1228/?k=<key>
```

Wildcard binds print `127.0.0.1`. The service log is the operator's to read;
the key is never written to `instance.json`, request/access logs, or HTML.

### 4.2 Session exchange

- `GET /?k=<key>`: constant-time compare; on success set the cookie and
  `303` redirect to the same URL with `k` removed (other query parameters
  survive); on failure `401`.
- `POST /v1/session` `{"key":"..."}`: same compare; sets the cookie and
  returns `204`. Used by the SPA's sign-in form for bookmarked URLs.
- `GET /v1/session`: `204` when the cookie is valid, `401` otherwise.
- `DELETE /v1/session`: clears the cookie, `204`.

Cookie: `any-converter-session-<port>=<key>; HttpOnly; SameSite=Strict;
Path=/; Priority=High`. `Max-Age=30 days` for a persisted key; session
cookie for a per-run key. `Secure` is omitted because loopback is HTTP; add
it behind TLS later.

The cookie name carries the port because browsers scope cookies by host and
ignore the port: two gateways on `127.0.0.1:1228` and `127.0.0.1:1229`
would otherwise overwrite each other's session. The port is taken from the
request `Host` header (the address the browser used), with `80` when the
header has none; this matches magpie's `magpie_web_<port>`. The `k` link,
`/v1/session`, and the management cookie check all derive the same name.

### 4.3 Management middleware

`require_management` accepts:

1. `Authorization: Bearer <management key>` — desktop IPC, CLI, scripts.
2. A valid session cookie — only when no `Authorization` header is present
   (a supplied but invalid bearer fails closed) and:
   - the `Host` header is loopback (`127.0.0.1`, `localhost`, `[::1]`, with
     an optional port). Cookie auth is loopback-only by construction; a LAN
     bind has no browser session and uses the management bearer.
   - `Sec-Fetch-Site`, when present, is `same-origin` or `none` (a
     cross-site page never gets a cookie-authenticated response);
   - for `POST`/`PUT`/`PATCH`/`DELETE`, `Origin`, when present, matches the
     request host.

`Sec-Fetch-Site` mirrors magpie's `webOwnPage`: `SameSite=Strict` already
keeps the cookie out of cross-site writes in modern browsers, but browsers
too old to honor it, or a future relaxation of the cookie, must not turn a
sibling `127.0.0.1` port into a management client.

No CORS headers are emitted. Management routes keep failing closed when the
authority is disabled. Agent and inference keys never authorize management.

## 5. HTTP contract additions

All data routes are management-guarded and return the same shapes the shared
`ApiClient` already consumes.

| Method | Path | Response |
|---|---|---|
| GET | `/v1/admin/logs?limit=N` | `{"records": RequestLogRecord[]}` (limit clamp 1..=1000, default 500) |
| GET | `/v1/admin/usage` | `{"summary": UsageSummary}` |
| GET | `/v1/admin/usage?heatmap=1` | `{"heatmap": Heatmap}` |
| GET | `/v1/admin/usage?ledger=1` | `{"page": LedgerPage}` |
| GET | `/v1/admin/usage?format=csv&ledger=1` | `text/csv` |
| GET | `/v1/admin/status` | `StatusData` + `version`, `pid`, `address`, `uptime_seconds` |
| POST | `/v1/admin/convert` | `ConvertApiResponse` |
| GET | `/` | SPA `index.html` |
| GET | `/assets/*` | hashed asset |

Usage query parameters mirror `apps/web/src/app/api/usage/route.ts`:
`preset`, `from`, `to`, `granularity`, `provider`, `model`, `agent`,
`account`, `split_by`, `day`, `failed`, `retried`, `query`, `cursor`,
`limit`, `ledger`, `heatmap`, `format`.

Error envelope: `{"error": "<message>", "code": "<stable-code>"}` with `400`
for bad ranges/parameters and `500` for storage failures. Missing database or
table is an empty summary, not an error (fresh home).

Static responses add:

- `Cache-Control: public, max-age=31536000, immutable` for `/assets/*`.
- `Cache-Control: no-cache` for `index.html`.
- `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`.
- CSP: `default-src 'self'; connect-src 'self'; img-src 'self' data:;
  style-src 'self' 'unsafe-inline'; script-src 'self'`.

## 6. UI build and embedding

`apps/desktop` remains the SPA source (Vite + React + HashRouter). It is
loaded by:

- Tauri from `apps/desktop/dist` (existing `frontendDist`);
- the Rust server from an embedded copy or a directory.

Embedding is automatic:

- `crates/server/build.rs` checks `apps/desktop/dist/index.html` at compile
  time and emits `cfg(has_ui_dist)`.
- With the cfg, `rust-embed` embeds `../../apps/desktop/dist/`.
- Without it, the server serves `ANY_CONVERTER_WEB_DIR` when set.
- Otherwise `GET /` returns `503` with build instructions.

This keeps `cargo test` and a plain `cargo build` independent of Node, while
`pnpm --filter @any-converter/desktop build && cargo build --release` produces
one self-contained gateway binary.

Build order for release/CI:

```bash
pnpm install --frozen-lockfile
pnpm --filter @any-converter/desktop build
cargo build --release -p any-converter
```

## 7. Daemon contract

CLI:

```
any-converter daemon install [--dry-run]
any-converter daemon uninstall
any-converter daemon start
any-converter daemon stop
any-converter daemon restart
any-converter daemon status
```

All six accept `--home <dir>` (global on the `daemon` subcommand). It selects
the same canonical home as `serve --home`; without it, `Home::discover()`.

- First run: a bare `serve` on a home with no `config.toml` bootstraps
  `[server] host=127.0.0.1 port=1228` and `[logging] dir=<home>/logs
  max_disk_mb=500` through the control plane. The daemon therefore starts on
  a clean machine, and the browser configures providers through the
  management API. A bare `serve` never writes a provider or a key.
- Scope: the invoking user's session. Never `sudo`, never a system-wide unit.
- macOS: `~/Library/LaunchAgents/com.any-converter.gateway.plist`
  - `RunAtLoad`, `KeepAlive {SuccessfulExit=false}`, `ThrottleInterval=5`,
    `ProcessType=Background`, stdout/stderr
    `~/Library/Logs/any-converter/gateway.log`.
  - `launchctl bootstrap gui/<uid> <plist>`, `bootout`, `kickstart -k`,
    `print`.
- Linux: `~/.config/systemd/user/any-converter-gateway.service`
  - `Restart=on-failure`, `RestartSec=5`, `WantedBy=default.target`.
  - `systemctl --user daemon-reload`, `enable --now`, `disable --now`,
    `show`.
- `install` refuses if a healthy gateway is already running (desktop or a
  foreground `serve`) and tells the user to stop it first.
- `install` creates `<home>/web-key` (`0600`) when absent, so the page URL is
  stable across restarts.
- `status` reports installed/enabled/running/pid/state/file/logs and the UI
  URL. The URL includes `?k=` because it is the operator's terminal.
- `uninstall` unloads, removes the unit, and leaves the key file and logs.
- `--dry-run` prints the service file and does not touch the system. It is
  the testable path on machines where installing would be intrusive.

## 8. Desktop attach contract

`instance.json` v2:

```json
{
  "pid": 1234,
  "started_at_unix": 1760000000,
  "config_revision": "sha256:...",
  "ownership_token": "...",
  "host": "127.0.0.1",
  "port": 1228,
  "version": "0.1.2"
}
```

- Fields are written after bind, atomically, `0600`; old readers tolerate
  missing fields.
- `ServerManager::attach_if_running(home)` reads metadata, probes
  `GET /health` with a short timeout, and enters `attached` when the probe
  succeeds.
- `ServerRunState` gains `attached`. `server_proxy` accepts `running` and
  `attached`.
- `stop`/`restart` on an attached manager return `ManagedExternally`.
- Desktop startup: attach first, start only when no live instance.
- Dashboard hides lifecycle buttons in attached mode and explains that the
  daemon owns the gateway.
- Browser `getServerStatus` reports `running` and hides lifecycle controls
  because the host API is unavailable.

## 9. Frontend capability model

The SPA is identical in both hosts; only the transport differs.

- `lib/platform.ts`: `isDesktopApp()` checks `__TAURI_INTERNALS__`.
- `lib/transport.ts`: `requestServer()` is the one dispatch point:
  - desktop -> Tauri `server_request` (management bearer);
  - browser -> same-origin `fetch` with `credentials: "include"`.
- `lib/management.ts`: the existing typed façade keeps its signatures; its
  `call()` delegates to `requestServer`. Pages do not change.
- `create-desktop-api-client.ts` becomes the single `ApiClient` adapter for
  both hosts, reading `/v1/admin/{convert,logs,usage,status}` through
  `requestServer`; the desktop's direct SQLite data commands leave the data
  path in Phase 5. `lib/api.ts` keeps only host lifecycle
  (`get_server_status`, start/stop/restart) and reports `running` from
  `/v1/admin/status` in the browser.
- `packages/core` `fetchApiClient` stays on the old `/api/*` routes until
  Phase 6 deletes its only consumer (`apps/web`); it is not the browser
  adapter.
- Browser-only `SessionGate`: checks `GET /v1/session`; on `401` renders a key
  form that posts `/v1/session`; on success renders the app.
- Host lifecycle controls render only when `isDesktopApp()` and the state is
  not `attached`; the browser and attached states show an explanation
  instead. Settings hides host/port in the browser because the browser
  cannot restart the gateway.
- The desktop `server_request` allowlist gains the four data routes (query
  string forwarded opaquely). `server_proxy::forward` returns a 2xx non-JSON
  body as a JSON string so ledger CSV works through IPC.

This removes the need to move every page into `packages/views` while still
using one build and one set of pages.

## 10. Tasks

### Phase 0 — Contract

- **T0.1** This document.
- **T0.2** Extend shared TypeScript types: `StatusData.version?`,
  `pid?`, `address?`, `uptime_seconds?`; `ServerStatus.state` adds
  `"attached"`.

### Phase 1 — Rust data API (no UI yet)

- **T1.1** `GET /v1/admin/logs`: reuse `SqliteStorage::recent_request_logs`,
  clamp limit, JSONL fallback only when SQLite is missing/empty (same
  behaviour as the deleted Next route).
- **T1.2** `GET /v1/admin/usage`: summary/heatmap/ledger/CSV. Reuse
  `Storage::usage_summary`, `heatmap`, `ledger_page`, `ledger_csv`; resolve
  `preset`/`from`/`to` in Rust with the same rules as the TypeScript route.
- **T1.3** `GET /v1/admin/status`: health, log-dir disk usage, up to 10 recent
  WARN/ERROR lines, version, pid, bound address, uptime.
- **T1.4** `POST /v1/admin/convert`: serde-validated `ConvertApiRequest`,
  calls `any_converter_core::convert`.
- **T1.5** Route guard inventory + handler tests.

Acceptance: all new routes require management auth; empty home yields valid
empty payloads; invalid query returns 400; CSV has the exact header; a
non-management key gets 403.

### Phase 2 — Page auth + UI serving

- **T2.1** `WebAuth` (key resolution, key file, constant-time compare,
  cookie set/clear, host/origin checks).
- **T2.2** `require_management` accepts a cookie session with the checks in
  §4.3.
- **T2.3** `GET/POST/DELETE /v1/session` and `GET /?k=`.
- **T2.4** UI asset provider (embedded cfg / `ANY_CONVERTER_WEB_DIR` / 503),
  SPA fallback limited to non-API GETs, cache and security headers.
- **T2.5** Startup log with the one-time URL.
- **T2.6** Tests: cookie accepted, bearer still accepted, bad bearer fails
  closed, wrong key 401, non-loopback cookie host 403, Origin mismatch 403,
  asset content type, unknown API still 404.

Acceptance: a browser can load `/`, exchange the printed key, and call every
management route. `curl` with the management bearer is unaffected.

### Phase 3 — Daemon

- **T3.0** Bare `serve` on a config-less home bootstraps `[server]` +
  `[logging]` so the installed service starts on a clean machine; the CLI
  reuses the existing `initialize_from_config` path.
- **T3.1** Service manager trait and macOS launchd implementation.
- **T3.2** Linux systemd user implementation.
- **T3.3** CLI commands, status formatting, `--dry-run`, current-gateway
  refusal.
- **T3.4** Unit tests for generated files and status parsing; a fake command
  runner for lifecycle tests.

Acceptance: `daemon install --dry-run` prints a valid plist/unit with the
absolute executable, `serve --home`, log paths, restart policy, and no
credentials. `daemon status` on a clean machine says not installed and exits
0. No command uses `sudo`.

### Phase 4 — Frontend convergence

- **T4.1** Platform detection and transport dispatch.
- **T4.2** One `ApiClient` adapter on `/v1/admin/*` for both hosts; ledger CSV
  through IPC; desktop proxy allowlist and non-JSON success bodies.
- **T4.3** Session gate, attached-state UI, hide host controls in browser.
- **T4.4** `pnpm typecheck` and `pnpm --filter @any-converter/desktop build`.

Acceptance: the built SPA works in the browser against the Rust server and in
Tauri; no page imports a Tauri module directly except the adapters and
`lib/api.ts` host lifecycle.

### Phase 5 — Desktop attach + data path removal

- **T5.1** Metadata host/port/version written after bind.
- **T5.2** `attach_if_running`, `attached` state.
- **T5.3** Remove direct SQLite Tauri commands (`list_request_logs`,
  `get_usage_summary`, `get_heatmap`, `list_ledger_page`, `convert_payload`)
  from the data path; route through `server_request`.
- **T5.4** Tests for attach discovery, stale metadata, attached lifecycle
  refusal, and the narrowed proxy allowlist.

Acceptance: start the daemon, open the desktop app, and the dashboard shows
`attached`; quitting the desktop leaves the daemon alive.

### Phase 6 — Remove the second stack

- **T6.1** Delete `apps/web`, `packages/bridge`, `crates/web-bridge`; update
  `pnpm-workspace.yaml`, `Cargo.toml`, root scripts, lockfile.
- **T6.2** Update `docs/build.md`, `docs/architecture.md`, `README*.md`,
  `CHANGELOG.md`, `AGENTS.md` routing.
- **T6.3** CI: build the SPA before the release Rust build; keep `pnpm
  typecheck`; add a release binary smoke check.

Acceptance: `pnpm build` produces only the SPA; `cargo build --release`
embeds it; `rg "better-sqlite3|next"` finds no runtime dependency.

### Phase 7 — Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`
- `pnpm install --frozen-lockfile && pnpm typecheck`
- `pnpm --filter @any-converter/desktop build`
- Build the gateway, run it on a temp home, and curl: `/`, `/?k=`,
  `/v1/session`, `/v1/admin/status`, `/v1/admin/logs`, `/v1/admin/usage`,
  `/v1/admin/usage?ledger=1`, `/v1/admin/usage?format=csv`, and a 404 API.
- Manual desktop attach check.
- Review: rust-reviewer, typescript-reviewer, security-reviewer.

## 11. Test matrix

| Layer | Case | Expected |
|---|---|---|
| Rust unit | `web_key` accepts 16+ safe chars, rejects spaces/newlines | clear error |
| Rust unit | cookie parser ignores unrelated cookies | only exact name matches |
| Rust unit | origin check | same host ok, other host 403, absent ok |
| Rust unit | cookie name | `any-converter-session-<Host port>`, `80` when absent |
| Rust integration | two gateways, two ports | each cookie name is its own; no overwrite |
| Rust integration | cross-site `Sec-Fetch-Site` + cookie | 403, no management data |
| Rust unit | plist/unit generation | paths escaped, policies present, no key |
| Rust integration | management bearer | 200 |
| Rust integration | cookie session | 200 |
| Rust integration | invalid bearer + valid cookie | 401 (fail closed) |
| Rust integration | non-loopback Host + cookie | 403 |
| Rust integration | `GET /?k=wrong` | 401, no Set-Cookie |
| Rust integration | `GET /?k=right` | 303 + Set-Cookie |
| Rust integration | assets | 200, correct type, immutable cache |
| Rust integration | unknown `/v1/*` | 404 JSON, never SPA HTML |
| Rust integration | usage empty home | 200 empty summary |
| Rust integration | usage bad range | 400 envelope |
| Rust integration | ledger CSV | header + rows, `text/csv` |
| Rust integration | logs limit clamp | 1000 max |
| Rust integration | status | disk + errors + version |
| Rust integration | convert invalid body | 400 |
| Rust integration | attach metadata | host/port/version present after bind |
| Rust integration | attach probe | attached on 200, stopped on failure |
| TS typecheck | adapters implement `ApiClient` | pass |
| E2E (manual/Playwright) | browser: open key URL -> dashboard -> config save -> logs -> usage -> CSV | pass |
| E2E (manual) | desktop attaches to daemon, quit leaves it running | pass |

## 12. Compatibility and migration

- Existing management bearer callers are unchanged.
- Existing `/v1/usage` stays for CLI/stats compatibility.
- `apps/web` is removed only after the SPA serves every page it had. The
  browser now uses the desktop SPA, so parity is by construction.
- Desktop local SQLite commands are removed only after the new HTTP data
  routes pass tests; during the transition both exist but only HTTP is used.
- `config.toml` and keychain authority are unchanged.

## 13. Risks

| Risk | Mitigation |
|---|---|
| UI embed makes `cargo build` depend on Node | build.rs cfg only when `dist/index.html` exists |
| Cookie auth widens the management surface | loopback Host check + Origin check + SameSite=Strict + no CORS |
| Desktop and daemon fight over the lock | attach-first; daemon stop unloads; clear install refusal |
| Duplicate data semantics drift during migration | delete the Next reader and desktop SQLite commands in the same change; contract tests assert shapes |
| Per-run key changes on restart | daemon persists `<home>/web-key`; SPA sign-in form recovers |
| Long-running daemon with stale binary | `daemon restart` after upgrades; status shows executable path |
| macOS keychain ACL scoped to the creating app | keys written through the browser are created by the daemon binary; keys created by the desktop may prompt on the daemon's first unattended read — re-save them through the browser after `daemon install` |
| `serve` on a fresh home previously refused to start | bare `serve` now bootstraps `[server]` + `[logging]`; inline flags still require `--format`/`--base-url` |

## 14. Definition of done

- One `any-converter serve` process serves API + SPA on one port.
- Browser management works with only the printed page key.
- Desktop attaches to that process and never starts a second one when it is
  alive.
- `daemon install --dry-run` and `daemon status` are correct on macOS/Linux;
  real install is opt-in and user-scoped.
- Every UI data view is backed by Rust HTTP.
- Next.js, better-sqlite3, and the napi conversion bridge are gone from the
  runtime.
- Full Rust and TypeScript checks pass; the release binary embeds the SPA.
