# Tests Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Cross-workspace fixtures and any future top-level integration harnesses that do not belong to a single crate or app.

## Routing

| If you are working on... | Go to |
|--------------------------|-------|
| Record-replay contract fixtures (client → gateway → upstream) | [`contracts/AGENTS.md`](./contracts/AGENTS.md) |

## Current State

- `tests/contracts/` holds the shared client-integration contract fixtures, replayed by
  `crates/server/tests/contract_replay.rs`.
- Everything else in `tests/` is reserved for shared fixtures / future cross-workspace tests.
- Most active tests live beside their owning crate or app and should usually stay there.

## Boundary Rules

1. If a test belongs clearly to one crate or one app, keep it with that owner instead of moving it here.
2. Use this directory only for truly cross-domain test assets or harnesses.
3. Fixtures under `tests/contracts/` must be sanitized — no keys, tokens, emails, or absolute home paths.
