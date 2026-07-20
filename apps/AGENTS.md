# Apps Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** This file routes user-facing application work. After choosing `web` or `desktop`, drill down again.

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Next.js Web UI, API routes, browser-facing pages | [`web/AGENTS.md`](./web/AGENTS.md) |
| Tauri Desktop shell, embedded server control, local SQLite app state | [`desktop/AGENTS.md`](./desktop/AGENTS.md) |
| App-level concerns spanning both Web and Desktop | Read this file, then [`../packages/AGENTS.md`](../packages/AGENTS.md) and the relevant app `AGENTS.md` |

## App Overview

| App | Path | Purpose |
|-----|------|---------|
| Web | [`web/`](./web/) | Browser UI for playground, logs, usage, status, and config |
| Desktop | [`desktop/`](./desktop/) | Tauri app for local control plane and embedded server management |

## Shared Constraints

1. App entrypoints should compose shared packages rather than duplicating UI or API client logic.
2. Shared React components, hooks, and types belong under [`../packages/AGENTS.md`](../packages/AGENTS.md), not directly in app-specific code unless they are truly app-specific.
3. If app work changes user-facing setup or URLs, update `README` and [`../docs/build.md`](../docs/build.md).
