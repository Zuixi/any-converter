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
│   ├── components/layout/       # AppShell, Sidebar, shared form/table helpers
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
4. Tauri commands should be thin IPC entrypoints around focused backend modules such as `db.rs`, `server_manager.rs`, and `secrets.rs`.
5. If Desktop changes embedded server behavior or config shape, sync with the corresponding Rust crate docs under [`../../crates/AGENTS.md`](../../crates/AGENTS.md).
6. After pulling dependency changes, install from the **repo root** (`pnpm install`). `tauri build` also runs `pnpm install` via `beforeBuildCommand` / `pretauri` so packaging does not use a stale `node_modules`.
