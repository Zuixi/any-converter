# TSConfig Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Shared TypeScript compiler presets used across the monorepo.

## Read This When

- You are changing `base.json`, `nextjs.json`, or `react-library.json`
- You are updating compiler defaults shared by multiple frontend packages

## Boundary Rules

1. Keep this package limited to compiler configuration.
2. Any change here has workspace-wide blast radius; validate affected packages after edits.
