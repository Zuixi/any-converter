# Tests Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Cross-workspace fixtures and any future top-level integration harnesses that do not belong to a single crate or app.

## Current State

- `tests/` is currently light and mostly reserved for shared fixtures / future cross-workspace tests.
- Most active tests live beside their owning crate or app and should usually stay there.

## Boundary Rules

1. If a test belongs clearly to one crate or one app, keep it with that owner instead of moving it here.
2. Use this directory only for truly cross-domain test assets or harnesses.
