# Apps Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** This file routes user-facing application work. The app domain is the Desktop app; after choosing it, drill down.

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Tauri Desktop shell, embedded server control, local SQLite app state | [`desktop/AGENTS.md`](./desktop/AGENTS.md) |
| App-level concerns | Read this file, then [`../packages/AGENTS.md`](../packages/AGENTS.md) and [`desktop/AGENTS.md`](./desktop/AGENTS.md) |

## App Overview

| App | Path | Purpose |
|-----|------|---------|
| Desktop | [`desktop/`](./desktop/) | Tauri app for the local control plane and gateway management |

The Desktop SPA is the only frontend build: the Tauri app loads it in its
webview, and the Rust gateway embeds the same `apps/desktop/dist` build and
serves it to any browser.

## Shared Constraints

1. App entrypoints should compose shared packages rather than duplicating UI or API client logic.
2. Shared React components, hooks, and types belong under [`../packages/AGENTS.md`](../packages/AGENTS.md), not directly in app-specific code unless they are truly app-specific.
3. If app work changes user-facing setup or URLs, update `README` and [`../docs/build.md`](../docs/build.md).
