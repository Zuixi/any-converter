# Web App

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Next.js application that exposes the browser UI and server-side API routes for config, status, logs, usage, and conversion playground flows.

## Read This When

- You are changing `apps/web/src/app/*`
- You are changing Next.js route handlers under `apps/web/src/app/api/*`
- You are adjusting app-specific config such as `next.config.js`

## Local Structure

```
apps/web/
├── src/app/           # App Router pages and API routes
├── src/components/    # Web-specific shell/navigation components
├── package.json       # Next.js scripts
└── next.config.js     # Next.js runtime config
```

## Downstream Dependencies

- Shared UI, hooks, and views live in [`../../packages/AGENTS.md`](../../packages/AGENTS.md)
- Browser playground acceleration goes through [`../../crates/web-bridge/AGENTS.md`](../../crates/web-bridge/AGENTS.md) via `@any-converter/bridge`
- Log and usage data ultimately come from the Rust server and its storage contracts

## Boundary Rules

1. Keep app-specific page composition here.
2. Move reusable hooks, typed clients, and presentational components down into `packages/` when they are shared or shareable.
3. Web API routes should remain thin adapters around filesystem/config access and shared types; avoid burying product logic in route handlers.
