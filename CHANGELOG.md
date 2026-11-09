# Any Converter CHANGELOGS

## Unreleased — Resident gateway

### Changed

- **The server is now the configuration and lifecycle authority.** The canonical home is `%APPDATA%\any-converter` on Windows and `$XDG_CONFIG_HOME/any-converter` (or `$HOME/.config/any-converter`) elsewhere; `ANY_CONVERTER_HOME` overrides it. `config.toml` is loaded and written through a kernel-locked, atomic, revision-checked `ConfigStore`, not reconstructed from Desktop SQLite.
- **Provider, inference/client, and management credentials use separate OS keychain namespaces.** TOML stores `keychain:any-converter:*` references. Missing or unreadable secrets fail closed; management and inference credentials are no longer interchangeable.
- **Desktop is a resident supervisor and management console.** It migrates legacy SQLite configuration once, starts the managed gateway under the server instance lock, reports `starting/running/stopping/failed` from observed readiness and task completion, and updates providers, routes, settings, and secrets only through typed management APIs.
- **CLI `serve` uses the canonical home.** A one-time `--config` import moves legacy provider/client keys to the keychain and then starts the managed gateway; later starts use the canonical config. `--home` selects an explicit installation root.
- Inline CLI bootstrap accepts `ANY_CONVERTER_UPSTREAM_KEY`, keeping the provider key out of shell history and process arguments during the one-time import.
- **Web config editing proxies the management API.** It no longer reads or writes `config.toml` directly.
- **Desktop hardening:** non-IPC management proxying is gone, requests are limited to an allowlist and the management key, Tauri has a restrictive CSP/single-instance/tray close-to-hide behavior, and the unused shell capability was removed.

### Added

- Accepted architecture: [`docs/design/desktop-resident-gateway.md`](docs/design/desktop-resident-gateway.md).
- Management endpoints for typed config patches (`ETag`/`If-Match`), provider and inference secrets, management-key rotation, and server status.

### Fixed

- One-time CLI imports and management secret mutations now hold the same cross-process config lock while writing the keychain and config, restore previous keychain values if an import fails, and serialize management-key rotation. This prevents orphaned keys, stale successful-looking writes, and concurrent import races.
- Initialization and legacy migration no longer overwrite an existing canonical secret with a different value. Equal values retry idempotently after a crash; conflicts fail closed, preserving the prior credential.
- Non-default `ANY_CONVERTER_HOME` installations use isolated keychain service namespaces. The default installation remains compatible with the existing `any-converter` service.
- The resident gateway instance lock now uses a kernel-backed cross-process lock without age-based takeover. The credential locks keep stale-crash recovery, but a long-running or slow-starting gateway can no longer be incorrectly taken over after 30 seconds.
- CLI management commands (`agents` and `accounts quotas`) now authenticate with the dedicated management key from `ANY_CONVERTER_MANAGEMENT_KEY` or the local OS keychain. Inference/agent keys can no longer be silently presented as management credentials.
- A completed Desktop migration marker is terminal: restarting the app no longer re-imports legacy SQLite settings over canonical management-API edits, and logging setup no longer falls back to SQLite configuration. An interrupted `in_progress` marker remains resumable; unsupported or inconsistent markers fail closed.
- Desktop lifecycle operations are serialized, so restart requested during `starting` waits for the observed transition instead of failing with `already running`. The management proxy also ignores all environment-proxy variables and decodes dynamic path segments exactly once before applying its allowlist.
- Desktop reports the actual listener address after bind, uses a kernel-backed config lock without age-based takeover, and blocks automatic gateway startup when legacy migration fails.
- The Desktop Agents page derives its default-model picker from the management config view instead of trying to proxy the inference-only `/v1/models` endpoint. The picker now works without widening the management allowlist.
- Streaming proxies preserve separately framed SSE events when an upstream uses CRLF line endings. The previous incremental splitter treated the whole response as one event, collapsing Claude streams into a malformed terminal event.

## v0.1.2 (2026-11-04)

### Fixed

- **Accounts/Agents pages no longer fail with `TypeError: Failed to fetch`** (root cause: the webview's cross-origin `fetch` into `http://127.0.0.1:<port>` triggers a CORS preflight the server never answers). Management calls now travel over Tauri IPC (`server_request` in `server_proxy.rs`): the Rust side — the same process that hosts the server — dials loopback directly. Typed failures render localized next-step hints (`not_running` says "start the server") instead of a raw TypeError.
- **`server.host` defaults to `127.0.0.1`** (loopback only); the previous `0.0.0.0` default exposed the unauthenticated management API to the LAN.
- **Plugin empty-state hint** now says the CLI is a separate release download (with the Windows asset name) instead of assuming it is already on PATH.

## v0.1.1 (2026-11-04)

### Fixed

- **Desktop Agents page**: the agent-wiring release shipped a WebUI `/agents` page but the Desktop app — a separate Vite SPA, not the WebUI — had no Agents entry at all; installed desktop builds showed no tab. Desktop now has a native Agents page (connect/reapply/disconnect/sync/rotate-key, drift banner, 10s polling) talking to the embedded server over loopback (PR #5).

### Documentation

- Design-spec errata: §4.4's "Desktop reuses the WebUI page" premise never held; process lesson recorded in the spec.

## Unreleased — Desktop management IPC

### Fixed

- **Accounts/Agents pages no longer fetch the embedded server cross-origin from the webview.** The browser origin (`http://localhost:1420` in dev, `tauri://localhost` or `http://tauri.localhost` in production) crossing into `http://127.0.0.1:1228` requires a CORS preflight the server never answered, so both pages failed instantly with `TypeError: Failed to fetch`. Management calls now travel over Tauri IPC (`server_request` command in `server_proxy.rs`), where the Rust side — the same process that hosts the server — dials loopback directly. Failures come back typed: `not_running` renders the localized “server not running” hint instead of a raw `TypeError`, and non-2xx bodies keep the server's own `error.message — error.hint` text. Path traversal and non-GET/POST methods are rejected before dialing.
- **`server.host` now defaults to `127.0.0.1` (loopback only).** The previous default `0.0.0.0` exposed an unauthenticated management API to the LAN by default. Existing installs keep their chosen host; the retired `server.host.lan_default_applied` marker row is removed from Settings so it stops surfacing as a phantom option.

### Added

- **`server_proxy.rs` end-to-end tests**: a real embedded server is spawned per test, verifying wildcard-host rewriting (`0.0.0.0` → dialable loopback), health/model forwarding, the `not_running` typed error when nothing answers, and path validation — the same paths the webview used to attempt.
- **`src/lib/management.ts`**: the single typed façade for the server's management API. Every management path, payload shape and polling helper lives in exactly one file; pages import typed methods and never assemble URLs. Adding a management operation = adding one method there.
- **`scripts/guard-no-loopback-fetch.sh`** (pre-commit + CI): rejects any `fetch` of `127.0.0.1` / `localhost` / `:1228` under `apps/desktop/src/` — the bug class that produced the original `TypeError` now fails the build instead of shipping. Pooled `reqwest` client on the Rust side replaces per-call construction (pages poll at 1–10s intervals).

## Unreleased — Build & Release pipeline

### Added

- **Release workflow (`.github/workflows/release.yml`)**: pushing a `v*` tag now builds unsigned installers and CLI binaries — macOS `.dmg` (arm64) + CLI (arm64 & x86_64), Linux `.AppImage`/`.deb` + CLI (x86_64), Windows installer + CLI (x64) — after checking the tag matches `tauri.conf.json`'s version and smoke-testing the server binary (`/health`, `/v1/models`, and an unknown-model request must fail with a structured error). All assets ship with a `SHA256SUMS` file on a **draft** GitHub release for manual review before publishing. `workflow_dispatch` runs the builds without publishing.
- **CI hardening**: GTK/WebKitGTK apt installs now time out after 15 minutes (a stuck mirror can hold the queue for hours), CI runs with read-only permissions, and every push to `main` builds the release CLI binary so the release pipeline stays green between tags.

## Unreleased — Usage Statistics

### Added

- **`any-converter stats --tui`**: a live terminal screen for the usage page — period, granularity, split and metric switch on single keys (`p` `g` `s` `m`), the ledger scrolls, `space` freezes it, and it refreshes once a second. Built for the case the Web UI cannot serve: no browser, or a shell where opening one is slower than pressing `q`. A non-TTY (a pipe, CI) silently falls back to the one-shot report, so no escape codes ever reach a log.
- **`GET /v1/usage`**: the same usage summary the Web UI serves, over the gateway, so a script or a remote widget can ask what it spent without reading the database file. Requires the client key when `server.api_key` is set — it exposes spend, not just liveness — and answers a zeroed summary, not an error, when the database does not exist yet.
- **`any-converter stats`**: a terminal report of the usage page with `--period` / `--granularity` / `--split` / `--metric` / `--json` / `--watch`.
- **Per-provider, per-model and per-agent usage statistics.** The Usage page now breaks token usage down by provider, by model (with its provider), and by agent (the normalized client id), for today / 7 days / 30 days / all time, bucketed by hour, day, week or month. A `GET /api/usage?preset=7d&granularity=day` answers the summary; `last:24h` and `2026-01-01..2026-01-31` select a rolling window or a picked day range.
- **Cache hit rate.** `cache_read / (input + cache_read + cache_write − cache_unknown)`, shown as an em dash when no vendor in the range counted a cache — a vendor that reports no cache tokens is never rendered as a 0% hit rate. Anthropic's 1-hour cache writes are counted separately (`cache_write_1h`).
- **Ledger view.** A second tab lists individual calls with time, agent, provider, requested vs served model, tokens, cache traffic, cost, status, latency and TTFT. Filters cover agent, provider, model, account, failures, a text query and a single day; paging is cursor-based, and the filtered set exports to CSV.
- **Pre-aggregated hourly rollup.** Summary queries read `rollup_hourly` instead of scanning the detail table (measured 1.40 s → 0.008 s for a 30-day provider×model ranking over 1M rows). `rebuild_rollup()` recomputes it from the detail rows if it ever drifts.
- **Data retention.** `logging.retention_days` (default 30, `0` keeps everything) prunes request logs and their bodies from the background sweep. Previously the SQLite database had no bound at all.
- **Optional JSONL mirrors.** `logging.jsonl_mirror` (default `false`) writes `usage.YYYY-MM-DD.jsonl` beside the database for `tail`/`jq`/backup. Nothing reads it for queries.

### Changed

- **SQLite is the single source of truth.** The narrow metadata and the wide body payload now live in separate tables (`request_logs` / `request_bodies`) so aggregation never drags multi-megabyte rows through a scan. Schema changes are versioned with `pragma user_version` and applied in place.
- **Inserts are batched.** Records commit in one transaction per 256 records or 200 ms, replacing one WAL fsync per request.
- **`/api/logs` and `/api/usage` no longer fall back to JSONL files.** A silent fallback could hide a broken database; an empty one now answers zeroed data.

### Fixed

- **The database could grow without bound.** `pragma auto_vacuum` was never enabled, so `pragma incremental_vacuum` in the background sweep was a no-op and deleted rows kept their pages: a 1000-row/290 MB database stayed at 290 MB after deleting 90% of it. New databases are created with `auto_vacuum = incremental` and existing ones are converted once (a single `VACUUM` at open, tens of seconds on a multi-GB file); the sweep now releases pages every tick in bounded steps.
- **Bodies and metadata shared one retention window.** `request_bodies` (three captured bodies plus the trace, up to ~3 MiB per row) lived as long as the 30-day metadata, so keeping a month of statistics also kept a month of conversation payload. Bodies now expire after 7 days (`logging.body_retention_days`), metadata after 30 (`logging.retention_days`), and the hourly rollup is configurable separately (`logging.rollup_retention_days`, 0 = forever). Measured end to end: 40 days × 40 calls/day × 64 KiB bodies went from 106.2 MB to 19.1 MB after one retention pass.
- **`max_capture_bytes` default dropped from 10 MiB to 1 MiB**, capping a single stored row at roughly 3 MiB instead of 30 MiB.
- **Hourly usage attributed a whole hour to one arbitrary provider.** The old aggregate used `min(provider)` and `min(client_model)` as placeholders, so every provider's tokens in an hour were summed onto one of them. Provider and model are now real grouping keys.
- **The write-only `usage_logs` table is gone.** It duplicated a strict subset of `request_logs` on every request and was never read.
- **Desktop no longer declares its own `request_logs` schema.** Two owners of one table could pin an old shape on a shared database file.

## Unreleased — Agent Wiring

### Added

- **One-command agent wiring (`agents`)**: Claude Code, Codex, Gemini CLI, and Pi can now be connected to the gateway without hand-editing their configs. `any-converter agents connect <id>` writes the gateway URL, a per-agent key, and the full model catalog into the agent's own files; `disconnect` restores them **byte for byte** (a `.before-any-converter` backup sits beside each touched file as the offline rescue copy). Same flows in the Web UI's new `/agents` page, with drift detection and one-click reapply when another tool rewrites a connected agent's config.
- **Per-agent gateway keys (`acw_<agent>_…`)**: every connected agent gets its own key, so request attribution no longer depends on User-Agent and keys rotate independently (`agents rotate-key`). Agent keys authenticate inference endpoints only — the agents management API requires the global server key.
- **`/v1/agents` management API**: list (with lazy drift checks), connect, disconnect, sync, key rotation, and key metadata (never plaintext) under `/v1/agents/*`.
- **`agent-wiring` crate**: format-preserving editors (JSON/JSONC, TOML, dotenv — comments, order, CRLF, BOM survive), atomic writes, and the backup trio (snapshot/stash/applied) behind the agents feature; reusable for future agents.
- **Model catalog from `[[model_routes]]`**: concrete (non-wildcard) route patterns now feed the agents' model catalog, so the README-recommended routing setup yields a non-empty catalog; wildcard patterns from either `model_routes` or `model_map` are excluded (a pattern is not a model an agent can pick).
- **Chinese/English "Agents" navigation** entry and page copy.

### Changed

- **Default gateway port is now `1228`** (was `8080`). The server, CLI defaults, Web UI proxy fallback, and Desktop defaults all agree. **Upgrade note**: deployments that relied on the old implicit `8080` default (no explicit `port` in config) move to `1228` on upgrade — set `port = 8080` explicitly to keep it, and update any client `base_url` accordingly.
- `resolve_provider` now also accepts catalog ids of the form `<provider>/<model>` (what agent wiring writes into configs): specific routes match the bare model name, and a catch-all `"*"` can no longer shadow a named route for prefixed ids.

## Unreleased — subscription accounts and plugins

### Added

- **Sign in to six subscriptions from the window**: `gemini` and `antigravity`
  (Google OAuth with PKCE and a loopback callback), `codex` (ChatGPT OAuth on
  Codex CLI's fixed `127.0.0.1:1455`, going paste-only when that port is
  taken), `chatgpt-api` (Sign in with ChatGPT, which registers this app as it
  goes and hands back the client id with the code), `copilot` (GitHub device
  flow) and `claude` (Claude Code's own `claude auth login`, run here). A
  sign-in is a task: `POST /v1/accounts/signin` returns at once, the window
  polls, pastes a callback address or a code, or cancels — and a cancelled
  flow releases its listener and temp state.
- **Per-account model discovery and a model column on the Accounts page**:
  `GET /v1/accounts/{id}/models` lists what a signed-in account can use —
  Code Assist for Gemini/Antigravity, `/v1/models` for ChatGPT API, a
  Copilot session plus `/models`, a plugin's `models` hook, and literal
  `model_map`/`model_routes` entries from an exactly bound provider. The
  sign-in completion card and every account row now show loading, discovered,
  configured, empty, unsupported and retryable-error states instead of
  stopping at “signed in”. Discovery results and configured aliases are
  deduplicated by model id; wildcard patterns are not presented as models.
- **Plugin accounts appear in the account list**: `GET /v1/accounts` now
  merges the built-in secret store with `plugin-auth.json`, marks each row
  `source: "builtin" | "plugin"`, and never exposes key or token material.
  The window labels plugin-managed rows and leaves their removal/order to the
  plugin.
- **Providers served by an OpenCode plugin**: a `plugin` entry in a provider
  makes the plugin own the transport — its loader supplies the base URL and
  signs the request, its model list picks the wire API per model (a Claude
  model and a GPT model can sit side by side), and a request leaves through
  the plugin's own `fetch`. `any-converter plugin install/list` manage the
  packages; `GET /v1/accounts/plugins` tells the window what each serves.
- **Account pools with allowance-aware ordering**: several accounts per
  provider, tried in the user's saved order (`accounts order`, the page's
  "move up", kept in `account-order.json`), with any account past 95% of its
  fullest window moved last rather than rearranged. Routing never waits on a
  quota read: a cached window decides, a stale one refreshes in the
  background, and an unknown one counts as having room.
- **`GET /v1/accounts/quotas`**, and the Accounts page draws each allowance
  window as a bar with its reset time.
- **Codex `auth.json` write-back**: an adopted account's rotated tokens are
  written back where Codex reads them, keeping every other field, under a
  lock and a last-good backup — and never over a file it could not read. A
  keyring store is refused with guidance rather than guessed at.
- **Importing accounts from other tools**: `accounts discover` /
  `accounts import-agent` read Codex's and Claude Code's own stores (read
  only), and `accounts import <path>` reads claude-swap (cswap), Sub2API and
  CLIProxyAPI exports, honouring cswap's newer-wins rule.
- **Per-account proxies**: a sign-in, a refresh and a revocation leave
  through the same exit the account's requests use.

### Changed

- Provider authentication goes through a `TokenSource`, so an OAuth account
  and a static key are the same shape to the request path.
- The upstream API is a per-provider (and per-plugin-model) property rather
  than an implication of the client format.
- `config.rs` and `handlers.rs` are split by concern; `handlers/chat/` keeps
  the request path in `plan` / `forward` / `plugin` / `failover` stages.

### Fixed

- **A successful sign-in could still look like no models exist.** The plugin
  model cache is invalidated after an OAuth callback or background sign-in,
  so an empty list fetched before the credential existed cannot hide the
  models it just unlocked. Discovery failures retain literal configured
  models, and a provider-name hint no longer claims an account is bound —
  `"bound": false` tells the window that routing still needs an exact
  `account`/`accounts` entry.
- **ChatGPT API accounts with `siwc` metadata could not be resolved.** The
  credential reader accepted only `siwc: None`; it now accepts both `None`
  and `Some(...)`, so those accounts can refresh and discover models.

## Unreleased — Web UI

### Added

- **Normalized client ids in request logs**: the raw `User-Agent` is now mapped to a stable agent id (`claude-code`, `codex`, `gemini-cli`, `opencode`, `cursor`, `cline`, `roo-code`, `aider`, `pi`, or the UA's product name), so every record of one agent shares one `client_id`.
- **`POST /v1/messages/count_tokens` endpoint**: Claude Code's token counting now returns `{"input_tokens": N}` instead of a 404 — requests are forwarded verbatim to Claude-native upstreams (with model mapping applied), and answered with a local UTF-8 byte-length estimate when the provider is not Claude or the upstream fails.
- **English/Chinese UI language support**: Web and Desktop frontends now share a lightweight i18n provider with a persisted language toggle and automatic first-run language detection.
- **Guided Desktop setup forms**: Desktop Providers and Routes now include provider presets, field labels, examples, strategy explanations, disabled invalid submits, and empty-state next steps; Playground now makes clear it converts payload format only and includes per-format examples.
- **Structured request trace summaries**: Request logs can now include `trace.client`, `trace.upstream`, and `trace.response` summaries with message previews, tool definitions, tool calls, and tool results across OpenAI Responses, OpenAI Chat, Claude Messages, Gemini, and captured SSE output.
- **SQLite log storage mirror**: When `logging.dir` is configured, usage logs and full request logs are now mirrored into `{logging.dir}/any-converter.sqlite3` while continuing to write JSONL files as the fallback/debug format.
- **SQLite-backed Web UI logs and usage**: `/api/logs` and `/api/usage` now read `any-converter.sqlite3` first and fall back to JSONL files when SQLite is missing, empty, or unreadable.
- **Remote test helper script**: `scripts/om-any-converter-tunnel.sh` starts a local any-converter server and opens an SSH reverse tunnel to `om` for pi/codex testing.
- **Provider transport overrides**: Providers can now override upstream endpoint paths (`[providers.endpoints] path` / `stream_path`) and authentication behavior (`[providers.auth] scheme` / `headers`) while retaining format-based defaults.
- **Next.js web interface** (`apps/web/`): Local web UI for `any-converter` with five pages:
  - **Conversion Playground** (`/playground`): Interactive request/response conversion across OpenAI Chat, OpenAI Responses, Claude, and Gemini formats.
  - **Request Log Explorer** (`/logs`): Browse and inspect captured request/response lifecycle records from `requests.YYYY-MM-DD.jsonl`.
  - **Usage Dashboard** (`/usage`): Token volume, request count, and latency charts from `usage.YYYY-MM-DD.jsonl`.
  - **Proxy Status** (`/status`): Live health polling, log disk usage, and recent errors from the running server.
  - **Config Editor** (`/config`): View and edit `config.toml`; secrets are masked and the UI prompts to restart the server after saving.
- **Monorepo layout** (`packages/*`): Shared packages for UI atoms (`packages/ui`), business components (`packages/core`), page views (`packages/views`), types/utilities (`packages/shared`), TypeScript configs (`packages/tsconfig`), and the Rust napi-rs bridge (`packages/bridge`).
- **`crates/web-bridge`**: Native Node.js addon exposing `convert_request_string` and `convert_response_string` from `any-converter-core`.
- **Turborepo + pnpm workspace**: Root-level scripts for installing, building, and formatting the web stack alongside the Rust workspace.
- **CI workflow** (`.github/workflows/ci.yml`): GitHub Actions now gates pull requests on Rust `fmt` / `clippy -D warnings` / `cargo test --workspace` plus a workspace-wide `pnpm typecheck`.

### Changed

- **Desktop embedded server binds `0.0.0.0` by default** so LAN clients can reach it; existing localhost-only defaults are migrated once, and Dashboard shows a LAN access hint.
- **Desktop sidebar UX**: Sidebar is fixed (main panel scrolls independently), supports collapse/expand with persisted state, and each nav item shows a stroke icon (Providers uses the interlocking-rings mark; Usage uses the bar-chart mark; Playground uses the frame-plus mark).
- **Desktop frontend routing refactor**: Desktop UI now uses `react-router-dom` `HashRouter` with path-based pages (`#/dashboard`, `#/providers`, …), splits shell/pages into `components/layout` and `pages/*`, and routes all Tauri IPC through typed `src/lib/api.ts` wrappers instead of a single `main.tsx` `useState` page switcher.
- The Web UI request log detail view now displays structured trace summaries for downstream messages, model tool calls, and tool results when present in request logs.
- `Cargo.toml` workspace members now include `crates/web-bridge`.
- `docs/build.md` updated with web UI prerequisites, build steps, and environment variables.
- `round_robin` model route strategy now rotates the first provider attempted while retaining configured failover order.
- Server handler responsibilities were split into focused `model`, `namespace`, and `route_strategy` modules.

### Removed

- **Legacy `[[routes]]` routing**: format-based `[[routes]]` entries no longer route requests and their provider references are no longer validated. Configs still parse (so old `config.toml` files load), but the server prints one startup warning and ignores them — migrate to `[[model_routes]]` (e.g. `pattern = "*"`, `provider = "openai"`). `resolve_provider` now resolves via `[[model_routes]]` globs only.

### Fixed

- **Desktop log durability and Windows release startup**: Request audit JSONL files rotate at 10 MiB, request/usage mirrors share an SQLite connection pool, disk quota cleanup preserves SQLite/WAL files, Desktop persists WARN/ERROR runtime logs, and the Windows subsystem attribute now applies to the release executable instead of the library crate.
- **Logs Assistant Markdown rendering**: JSON content-part arrays are unwrapped before rendering so embedded Markdown is displayed as formatted conversation text instead of raw JSON.
- **Desktop Logs session grouping and overflow**: Requests without an explicit session ID are conservatively grouped only when their client message history has one uniquely longest strict prefix; request/correlation IDs no longer masquerade as sessions, and the conversation layout now stays within the default/minimum Desktop window width.
- **Logs `missing field truncated`**: `SanitizedBody.truncated` is omitted when `false` on write; deserialization now defaults it so SQLite/JSONL log reads succeed.
- **Server panic on CJK SSE previews**: Streaming log previews no longer slice `&str` at a raw byte index (`proxy.rs` / handler body truncation); use UTF-8-safe `utf8_prefix` so multibyte characters like `配` cannot panic the tokio worker.
- **Desktop Logs/Usage while server is running**: Desktop now opens the request-log SQLite database read-only (no schema/`journal_mode` rewrite), avoiding `database is locked` failures that previously surfaced as a generic `Unknown error` in Logs.
- **Shared hooks show Tauri string errors**: `useLogs` / `useUsage` / `useStatus` / `useConfig` now normalize string rejects the same way as Playground, so real IPC/SQLite messages are visible.
- **Desktop window minimum size**: Enforce `1024×700` via `set_min_size` at startup (in addition to `tauri.conf.json`) so the window cannot be resized below a usable layout.
- **Claude → Responses streaming without `message_delta`**: When Claude-compatible providers (e.g. MiniMax) end a tool_use stream with `message_stop` and omit `message_delta`, the converter now synthesizes a terminal `Done` event so clients receive required `response.completed` / `response.function_call_arguments.done` / `response.output_item.done` instead of failing with "stream ended before a terminal response event".
- Conversion Playground: swapping formats now reloads the matching input example; response-mode examples include required schema fields so convert no longer fails; conversion errors from Tauri/string throws are shown instead of a generic `Unknown error`; input/output JSON is pretty-printed (with a Beautify JSON action).
- Desktop `tauri build` now runs `pnpm install` before the frontend build (`beforeBuildCommand` / `pretauri`), preventing stale `node_modules` after pulling new deps such as `react-router-dom` from failing with `TS2307: Cannot find module`.
- Desktop Logs and Usage now read request records from the embedded server log database at `{app_data_dir}/logs/any-converter.sqlite3` instead of the separate Desktop app-state database.
- Desktop app (`apps/desktop/`) now shares the Web UI design system: its Tailwind config defines the shadcn semantic tokens, CSS variables are loaded, and conflicting global element styles were removed, so the shared Playground/Logs/Usage views render correctly instead of unstyled.
- Desktop Dashboard/Providers/Routes/Settings pages now compose `@any-converter/ui` components; IPC failures surface as error banners instead of silent empty states, providers can be deleted from the UI, and route provider selection uses checkboxes instead of raw comma-separated IDs.
- Claude-compatible streaming now accepts and ignores `signature_delta` chunks, preventing Kimi/Claude thinking signature events from breaking SSE conversion.
- OpenAI Responses → Claude request conversion now accepts typeless message items such as `{role, content}`, including `system`/`developer` messages that must become Claude `system` instructions, matching pi and common Responses client payloads.

## Unreleased — Protocol conversion fidelity improvements

### Added

- **Client header passthrough** (`crates/server/src/proxy.rs`): Client headers such as `anthropic-*`, `openai-organization`, `x-stainless-*`, `x-requested-with`, and `user-agent` are now forwarded to upstream providers by default. Hop-by-hop headers (`connection`, `transfer-encoding`, etc.), request-control headers (`host`, `content-length`, `content-type`, `authorization`), and provider auth headers are always managed by the proxy. Configure forwarding behavior with `[server.pass_through_headers]` (`mode`, `allow`, `deny`).
- **Upstream headers debug log** (`crates/server/src/handlers.rs`): `[logging] upstream_headers_log = true` logs the headers sent to upstream providers with sensitive values redacted.
- **Client ID in request logs** (`crates/server/src/request_log.rs`): Each request log record now includes an optional `client_id` extracted from `user-agent`, `x-requested-with`, or `x-stainless-*` headers. The Desktop Settings page displays the most recent client identifier.
- **Full request/response logging** (`crates/server/src/request_log.rs`): Optional audit logger enabled by `[logging.request_log] enabled = true`. Writes one JSON Lines record per request to `requests.YYYY-MM-DD.jsonl`, including request body, upstream request body, response body or SSE lines, latency, token usage, and truncation status. Sensitive headers and body keys are redacted.
- **Streaming request logging**: Captures every converted SSE line emitted to the client and records time-to-first-byte (TTFB) latency.
- **Disk quota manager migration**: `disk_manager.rs` moved from `crates/cli` to `crates/server/src/disk_quota.rs` so the server owns log-directory size enforcement.
- **Shared reasoning/thinking mapper** (`crates/core/src/converters/reasoning.rs`): Centralized `reasoning_effort` ↔ `thinking.budget_tokens` conversion for OpenAI Chat/Responses ↔ Claude, eliminating duplicated logic.
- **OpenAI Chat reasoning fields**: `OpenAIChatRequest` now accepts `reasoning_effort` and `reasoning`, enabling explicit reasoning preferences when converting Chat → Claude/Responses.
- **Streaming reasoning propagation**: OpenAI Chat SSE `delta.reasoning_content` is parsed as `CanonicalStreamEvent::ThinkingDelta` and emitted as Claude `thinking_delta`; Claude thinking deltas are emitted as OpenAI Chat `reasoning_content` chunks.
- **Base64 data URL image support**: `chat_to_claude` and `resp_to_claude` now detect `data:image/...;base64,...` URLs and emit Claude `source: {type: "base64", ...}` instead of forwarding the data URL as a remote URL.
- **Stream cache-token propagation**: OpenAI Chat/Responses SSE usage with `cached_tokens` is preserved through `CanonicalStreamEvent` and emitted as Claude `cache_read_input_tokens` / `cache_creation_input_tokens`.
- **ID normalization helpers** (`converters/shared.rs`): `normalize_id_to_chat`, `normalize_id_to_claude`, and `normalize_id_to_resp` consistently strip and re-add `chatcmpl-` / `msg_` / `resp_` prefixes across response and stream conversions.

### Fixed

- **System array joining**: Claude multi-block system arrays are now joined with `"\n\n"` (was `""`) when converting to OpenAI Chat/Responses, matching provider conventions.
- **Cache token accounting**: When converting OpenAI Chat/Responses usage to Claude, cached tokens are now subtracted from `input_tokens` because OpenAI's reported input tokens include cache reads while Claude's `input_tokens` does not.

## Unreleased — Test coverage hardening

### Added

- **Property-based tests** (`property_tests.rs`): Identity conversion, JSON validity, model preservation, StopReason roundtrip, Usage consistency — all driven by `proptest`.
- **Fuzz-style robustness tests** (`fuzz_tests.rs`): Random bytes/JSON/SSE inputs never panic; malformed tool arguments fall back gracefully; Unicode content preserved across all format pairs.
- **Concurrent stress tests** (`concurrent_tests.rs`): 200+ parallel conversions verify thread safety and StreamState isolation for both request/response and streaming paths.
- **Server stress tests** (`stress_test.rs`): 200 concurrent health checks, mixed endpoint requests, and auth rejection under load via tower oneshot.

### Changed

- **Stronger assertions in existing tests**: `converter_matrix.rs` now uses `pretty_assertions` and precise field extraction instead of substring searches. `parameter_mapping.rs` validates exact temperature/top_p/max_tokens/stop_sequence values per format pair (was threshold-based `passed > 0`). `response_deep.rs` asserts finish_reason, usage tokens, and tool names per pair (was `count >= 2/12`). `roundtrip.rs` adds response roundtrip tests and tool name set equality. `stream_matrix.rs` validates format-specific event types and ordering.
- **Test helpers** (`common/mod.rs`): Added ~15 precise field extraction functions (`extract_temperature`, `extract_tool_names`, `extract_response_text`, etc.) and format-aware `minimal_request_json`/`minimal_response_json` builders for proptest generators.

## 0.3.1 — Field safety, thinking/reasoning cross-format mapping, client compatibility

### Fixed

- **Thinking safety**: `chat/resp -> claude` converters now auto-inject `thinking` config when request history contains thinking blocks, preventing 400 errors from Anthropic API.
- **Namespace tool_choice**: When Responses API namespace tools are flattened to qualified names (`mcp__srv__shell`), `tool_choice` references are now corrected to match.
- **Model "default" leak**: Empty model field no longer sends `"default"` to upstream; uses empty string + warning log instead.

### Added

- **Thinking/reasoning cross-format mapping**: Claude `thinking` config maps to Responses `reasoning.effort` and vice versa; thinking is dropped for Gemini targets.
- **`ThinkingConfig` type**: Added to Claude wire types for proper serialization of thinking configuration.
- **Private field filtering**: Request body fields starting with `_` (internal client markers) are stripped before forwarding to upstream providers.
- **Model map longest-prefix matching**: When `model_map` has multiple wildcard patterns, the longest matching pattern wins (e.g. `claude-opus-*` beats `claude-*`).
- **`clean_system_billing_headers`**: Utility to strip `x-anthropic-billing-header:` lines from system prompts.

## 0.3.0 — Model-based intelligent routing, provider failover, and usage logging

### Added

- **Model-based routing** (`[[model_routes]]`): Route requests to upstream providers by model name using glob patterns (`claude-*`, `gpt-*`, `*`). First match wins. Replaces the need for format-only routing.
- **Multi-provider failover**: Configure multiple providers per model route (`providers = ["primary", "backup"]`). On upstream failure (429/5xx), automatically retries with next provider.
- **Route strategy**: `priority` (default, try in order) or `round_robin` per model route.
- **Usage logging**: Token usage automatically extracted from upstream responses and written as JSON Lines to `usage.YYYY-MM-DD.jsonl` files when `logging.dir` is configured. Supports OpenAI, Claude, and Gemini usage formats.
- **Config validation**: Startup checks verify all provider references in `model_routes` and `routes` exist. Missing providers cause an immediate error with actionable messages.
- **`/v1/models` enhancement**: Model list now populated from both `model_routes` (non-wildcard patterns) and `model_map` keys.

### Changed

- `process_request` now extracts model name **before** provider selection, enabling model-based routing.
- Provider resolution order: `model_routes` (first glob match) → legacy `routes` (format match) → 404.
- `config.example.toml` rewritten to showcase model routing with DeepSeek and failover examples.

### Backward Compatible

- Existing `[[routes]]` format-based config continues to work unchanged as a fallback.
- No changes to request/response conversion logic.

## 0.2.0 — Architecture: Pairwise format converters replace canonical IR

### Changed

- **Conversion architecture**: Replaced hub-and-spoke canonical IR conversion with direct pairwise converters — each (source, target) format pair now has a dedicated converter that translates requests, responses, and streaming events directly without data loss
- **`FormatConverter` trait**: New trait in `crates/core/src/converters/mod.rs` defines `convert_request`, `convert_response`, and `convert_stream_event` for each pair
- **Streaming reuse**: Pairwise converters compose existing `StreamAdapter::parse_sse_event` and `StreamAdapter::emit_sse_event` for streaming conversion

### Added

- 12 pairwise converter modules: `claude_to_chat`, `claude_to_resp`, `claude_to_gemini`, `chat_to_claude`, `chat_to_resp`, `chat_to_gemini`, `resp_to_claude`, `resp_to_chat`, `resp_to_gemini`, `gemini_to_claude`, `gemini_to_chat`, `gemini_to_resp`
- `crates/core/src/converters/shared.rs` — common utilities for converters
- Identity conversion passthrough for same-format requests

### Removed

- `FormatAdapter` trait and all `adapter.rs` / `adapter_tests.rs` files
- Canonical IR types no longer used: `CanonicalRequest`, `CanonicalResponse`, `ContentBlock`, `Turn`, `Role`, `ImageSource`, `SystemContent`, `SystemBlock`, `ToolDef`, `ToolChoice`, `GenerationParams`, `ResponseFormat`
- IR modules: `ir/request.rs`, `ir/message.rs`, `ir/tool.rs`, `ir/params.rs`
- Format-specific `helpers.rs` and `tools.rs` files (functionality moved into converters)

### Retained

- IR streaming types: `CanonicalStreamEvent`, `StreamState`, `StreamPhase`, `StopReason`, `Usage`, `AccumulatedToolCall` — used as the intermediate representation for streaming event conversion

## 0.1.7 — Refactor: Migrate logging from tracing to log crate

### Changed

- **Logging facade**: Replaced `tracing` + `tracing-subscriber` + `tracing-appender` with the `log` crate facade and a custom `MultiLogger` implementation
- **Multi-output support**: Console output (stdout/stderr) and file outputs are independently configurable with per-output level filtering, format (pretty/json), and optional target filtering
- **Config enhancement**: Added `[logging.console]` section with `enabled`, `output`, `level`, `format` fields; extended `[[logging.files]]` with `format` and `rotation` fields
- **Daily file rotation**: File outputs support automatic daily rotation via date-suffixed filenames (e.g. `general.2026-06-11.jsonl`)
- **Backward compatible**: All new config fields have sensible defaults; existing TOML configs work without modification

### Removed

- Dependencies: `tracing`, `tracing-subscriber`, `tracing-appender`

### Added

- Dependencies: `log` (with `std` feature), `chrono` (for timestamps and rotation)

## 0.1.6 — Fix: Namespace tool name dropped in function_call history + resilient error handling

### Fixed

- **Critical**: `parse_function_call_fields` dropped the `namespace` field from `function_call` items in request history — when Codex sends `{"type":"function_call", "namespace":"mcp__playwright", "name":"browser_navigate"}`, the converter was forwarding only `"browser_navigate"` to Claude instead of the qualified name `"mcp__playwright__browser_navigate"`, causing Claude to reject unrecognized tool names and triggering Codex retry loops
- **Critical**: `convert_sse_block` in proxy.rs silently swallowed stream conversion errors (returned empty `Vec`), causing critical SSE events like `response.output_item.done` and `response.completed` to be silently dropped — now emits an `error` SSE event for diagnostics
- **Critical**: Panic handler in `convert_sse_block` now also emits an `error` SSE event instead of returning empty output
- Added test `test_namespace_function_call_in_history_qualifies_name` validating the full roundtrip: namespace function_call in Responses request → qualified ToolUse name in canonical IR → correct Claude tool matching

### Root cause analysis

Codex CLI sends `function_call` history items with separate `namespace` and `name` fields for MCP tools. The `helpers::parse_function_call_fields` function was only reading `name` and ignoring `namespace`, so when the tool call was forwarded to Claude, the tool name didn't match any defined tool (Claude had `mcp__playwright__browser_navigate` but received `browser_navigate`). Claude would then return an error or unexpected response, causing Codex to retry indefinitely.

## 0.1.5 — Fix: Codex CLI MCP tool call dead loop (missing function_call item `id`)

### Fixed

- **Critical**: Codex CLI enters infinite loop with MCP tools — function_call items in streaming SSE responses were missing the `id` field (e.g. `fc_{call_id}`), causing Codex to silently discard tool calls and re-send the same request endlessly
- **Root cause**: Codex CLI checks `if (!item.id) return [state, NO_EVENTS]` when processing `response.output_item.added` — without `id`, tool calls are never executed, model keeps re-issuing the same tool call
- Added `id` field (`fc_{call_id}` format) to all function_call items in `response.output_item.added`, `response.output_item.done`, and `response.completed` output arrays
- Added `status` field (`"in_progress"` / `"completed"`) to function_call items matching the real OpenAI Responses API contract
- Added `item_id` field to `response.function_call_arguments.delta` and `response.function_call_arguments.done` events for proper delta routing across parallel tool calls
- Added `response.in_progress` SSE event after `response.created` (matching cc-switch / real API behavior)
- Added `created_at` timestamp to response objects in `response.created` and `response.completed` events

## 0.1.4 — Fix: Codex CLI model metadata warning + Feature: Structured logging system

### Fixed

- **Codex CLI warning**: "Model metadata for `GPT-5.4` not found" caused by case-sensitive slug matching — model names in `config.toml` normalized to lowercase (`gpt-5.4`) to match Codex bundled catalog
- **Root cause**: Codex CLI's `find_model_by_longest_prefix()` uses `model.starts_with(&candidate.slug)` which is case-sensitive; bundled `models.json` uses lowercase slugs; Codex does NOT fetch `/models` from custom base URLs with API key auth (only ChatGPT backend or command auth)
- **Production code audit**: eliminated `unwrap()` from `gemini/tools.rs` (response_schema) and `cli/main.rs` (read_input)

### Added

- **Codex-compatible `/v1/models` endpoint**: detects `client_version` query param (Codex signature) and returns `{ "models": [...] }` format with all required `ModelInfo` fields (`slug`, `display_name`, `shell_type`, `visibility`, `truncation_policy`, etc.)
- **Structured logging system** (`[logging]` config section) with:
  - Multi-layer tracing-subscriber: console (human-readable) + JSON file layers (non-blocking via `tracing-appender`)
  - Per-module/level log file splitting: `general.jsonl`, `error.jsonl`, `conversion.jsonl` + custom files via `[[logging.files]]`
  - Conversion before/after logging (`target: "conversion"`): captures original request, converted request, upstream response, and converted response with `request_id` correlation
  - Daily log rotation via `tracing_appender::rolling::daily`
  - Disk quota management: background task (every 5 min) enforces `max_disk_mb` by removing oldest log files
  - `WorkerGuard`-based crash protection: flushes pending writes on process exit
- **`ModelMetadata` extension**: `display_name` and `description` optional fields for Codex-compatible model info
- **`LoggingConfig`** / **`LogFileConfig`** structs in server config with sensible defaults

### Changed

- CLI `main.rs` now initializes multi-layer tracing instead of plain `fmt().init()`
- `process_request()` now generates `request_id` (UUID) for all log events, enabling request correlation
- Replaced `print!` in `Stream` command with `io::stdout().write_all()` for consistency with `println!` ban

## 0.1.3 — Refactor: Split OpenAI Responses adapter into focused modules

### Changed

- **Refactored** `openai_resp/adapter.rs` from 1078 lines to 544 lines by extracting concerns into sub-modules
- **Extracted** `tools.rs` (189 lines) — tool definition parsing, namespace flattening, tool_choice, response_format serialization
- **Extracted** `helpers.rs` (57 lines) — shared function_call parsing/serialization and status-to-stop_reason mapping (used by both adapter and stream)
- **Extracted** `adapter_tests.rs` (314 lines) — all 12 adapter unit tests via `#[path]` attribute
- **Eliminated** `_impl` indirection — FormatAdapter trait methods now contain logic directly
- **Deduplicated** function_call parsing (was in both `parse_input_item` and `parse_output_item`) into `helpers::parse_function_call_fields`
- **Deduplicated** function_call serialization (was in both `turns_to_input` and `content_blocks_to_output`) into `helpers::emit_function_call_json`
- **Unified** status↔StopReason mapping that was triplicated across adapter.rs and stream.rs into `helpers::status_to_stop_reason` / `helpers::stop_reason_to_status`

## 0.1.2 — Fix: MCP tool discovery/execution and model metadata for Codex CLI

### Fixed

- **Critical**: Codex CLI MCP tools (type `namespace`) were silently dropped during request conversion — they are now flattened into individual `function` tools with qualified names (`namespace__tool_name`) for upstream model compatibility
- **Critical**: Streaming SSE responses now correctly split qualified tool names back into separate `namespace` + `name` fields in `function_call` events, matching Codex CLI's `ToolName{namespace, name}` lookup format
- **Critical**: Non-streaming responses also patch `function_call` output items with proper `namespace` field
- `response.completed` and `response.output_item.done` / `response.output_item.added` SSE events all correctly patched

### Added

- `[model_metadata]` configuration section in `config.toml` — allows serving rich model metadata (`context_window`, `max_context_window`, `supports_parallel_tool_calls`) via `/v1/models` endpoint, eliminating Codex CLI's "Model metadata not found" warning
- `extract_namespace_map()` extracts namespace→tool mapping from Responses API requests before conversion
- `patch_response_namespaces()` restores `namespace` + short `name` in non-streaming responses
- `patch_sse_namespaces()` restores `namespace` + short `name` in streaming SSE events
- `slug` field added to `/v1/models` response for Codex CLI compatibility
- Unit tests for namespace tool flattening, name conflict disambiguation, and roundtrip

## 0.1.1 — Fix: Streaming tool calls not triggering execution in Codex CLI

### Fixed

- **Critical**: OpenAI Responses streaming emitter now emits `response.output_item.done` for `function_call` items — this is required by Codex CLI to trigger tool execution
- **Critical**: `response.function_call_arguments.done` event now emitted when tool call arguments streaming completes
- **Critical**: `response.completed` payload now includes all `function_call` items in the `output` array (previously only included text messages)
- **Critical**: Claude adapter now merges consecutive same-role turns before serializing to Claude messages — fixes 400 errors when parallel tool calls (multiple `function_call` items) produce multiple adjacent assistant turns that violate Claude's strict alternating role requirement
- Added `AccumulatedToolCall` to `StreamState` to track tool call data (id, name, arguments) during streaming
- Root cause (streaming): Codex CLI relies on `response.output_item.done` with a complete `FunctionCall` item to trigger tool execution
- Root cause (request): Parallel tool calls in Responses format produce separate `function_call` input items, each parsed as an independent assistant Turn; without merging, this creates consecutive assistant messages rejected by Claude-format APIs

### Added

- Cross-format integration tests: OpenAI Chat → Responses and Claude → Responses streaming tool call conversion
- Unit tests for tool call accumulation in streaming state

## 0.1.0 — MVP

### Added

- **Core conversion library** (`any-converter-core`) with typed Canonical IR and bidirectional conversion between 4 LLM API formats
- **Format adapters** for OpenAI Chat Completions, Claude Messages, OpenAI Responses, and Google Gemini
- **Streaming SSE adapters** for all 4 formats (parse + emit)
- **HTTP proxy server** (`any-converter-server`) with path-based format detection, client API key auth, upstream auth injection, model mapping, and SSE stream proxying
- **CLI tool** with `convert`, `stream`, and `serve` subcommands
- **156 unit tests** across core (124) and server (32) crates
- Example configuration file `config.example.toml`
