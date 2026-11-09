# Desktop App

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Tauri application combining a React frontend with a Rust backend for provider management, model route management, embedded server control, local logs, and usage views.

## Read This When

- You are changing `apps/desktop/src/*`
- You are changing Tauri backend code under `apps/desktop/src-tauri/*`
- You are changing Desktop-specific packaging or capabilities

## Local Structure

```
apps/desktop/
├── src/
│   ├── main.tsx                 # React mount + I18nProvider
│   ├── App.tsx                  # ApiClientProvider + HashRouter routes
│   ├── components/layout/       # AppShell, fixed/collapsible Sidebar + nav icons, form/table helpers
│   ├── pages/                   # Dashboard, Providers, Routes, Playground, Logs, Usage, Settings
│   ├── hooks/                   # useAsyncState and other desktop hooks
│   ├── lib/
│   │   ├── api.ts               # Typed Tauri command wrappers (pages must use this)
│   │   ├── create-desktop-api-client.ts
│   │   └── constants.ts
│   ├── types/
│   └── styles.css
├── src-tauri/src/               # Tauri Rust commands, DB, secrets, server manager
├── src-tauri/tests/             # Desktop-specific Rust tests
├── src-tauri/icons/             # App icons
└── src-tauri/tauri.conf.json
```

## Downstream Dependencies

- Shared React views and hooks live in [`../../packages/AGENTS.md`](../../packages/AGENTS.md)
- Desktop Rust code reuses `any-converter-server` and `any-converter-core` behavior
- Local persistence is SQLite-backed and separate from the Web app's server-side log reader

## Boundary Rules

1. Desktop frontend should compose shared packages whenever possible.
2. Prefer `react-router-dom` `HashRouter` for in-app navigation; do not reintroduce `useState` page switching.
3. Frontend pages and hooks must call `src/lib/api.ts` instead of raw `invoke("command_name")` strings.
4. The webview must NEVER `fetch` the embedded server directly — a browser origin cannot cross into loopback (the CORS preflight the server never answers surfaces as `TypeError: Failed to fetch`). Management calls go through `src/lib/management.ts` (the `server_request` Tauri command); adding a management operation = adding one typed method there. `scripts/guard-no-loopback-fetch.sh` (pre-commit + CI) rejects any loopback `fetch` under `src/`.
5. Tauri commands should be thin IPC entrypoints around focused backend modules such as `db.rs`, `server_manager.rs`, and `secrets.rs`.
6. If Desktop changes embedded server behavior or config shape, sync with the corresponding Rust crate docs under [`../../crates/AGENTS.md`](../../crates/AGENTS.md).
7. After pulling dependency changes, install from the **repo root** (`pnpm install`). `tauri build` also runs `pnpm install` via `beforeBuildCommand` / `pretauri` so packaging does not use a stale `node_modules`.
8. Embedded server defaults to `server.host=127.0.0.1` (loopback only). A LAN bind is an explicit operator choice; the retired `server.host.lan_default_applied` marker is cleaned up by `seed_defaults`.
9. Window minimum size is `1024×700` (`tauri.conf.json` + `lib.rs` `set_min_size`). Keep both in sync.
10. Logs/Usage IPC must open `{app_data}/logs/any-converter.sqlite3` **read-only** while the embedded server may be writing; do not call `SqliteStorage::open_in_log_dir` from those read paths.
11. Desktop is not a second configuration authority. Canonical `config.toml` and API-key state belong to the server; Desktop SQLite is only the one-time legacy migration source and read-only log adapter.
12. `server_proxy.rs` is an allowlisted management client, not a generic HTTP proxy. It must use the management key from the server keychain and must never fall back to an arbitrary external server when the managed instance is stopped.
13. Provider/route/settings pages call the typed methods in `src/lib/management.ts`; do not reintroduce SQLite-backed configuration IPC commands.
