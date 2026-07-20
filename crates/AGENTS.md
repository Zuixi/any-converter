# Crates Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** This file covers Rust-domain routing and shared constraints only. After choosing a crate, read that crate's own `AGENTS.md`.

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Format conversion logic, typed formats, IR, SSE adapters | [`core/AGENTS.md`](./core/AGENTS.md) |
| HTTP routing, proxying, auth, logging, storage, streaming | [`server/AGENTS.md`](./server/AGENTS.md) |
| CLI commands, stdin/stdout handling, config assembly | [`cli/AGENTS.md`](./cli/AGENTS.md) |
| Native bridge code used by the Web playground | [`web-bridge/AGENTS.md`](./web-bridge/AGENTS.md) |
| Cross-crate Rust refactor | Read this file, then all affected crate `AGENTS.md` files |

## Crate Overview

| Crate | Path | Purpose |
|-------|------|---------|
| `any-converter-core` | [`core/`](./core/) | Pure conversion library |
| `any-converter-server` | [`server/`](./server/) | HTTP proxy server and observability runtime |
| `any-converter` | [`cli/`](./cli/) | User-facing CLI wrapper |
| `web-bridge` | [`web-bridge/`](./web-bridge/) | `napi-rs` bridge exposing `core` to the Web app |

## Shared Constraints

1. Keep business boundaries sharp:
   `core` owns conversion, `server` owns transport and logging, `cli` owns user entrypoints, `web-bridge` owns native JS interop.
2. Do not duplicate format knowledge outside `core`.
3. When Rust work changes frontend-facing contracts, coordinate with [`../packages/AGENTS.md`](../packages/AGENTS.md) or [`../apps/AGENTS.md`](../apps/AGENTS.md).
4. When Rust work changes user-visible setup or runtime shape, update [`../docs/AGENTS.md`](../docs/AGENTS.md) targets as needed.

## Maintenance Checklist

- [ ] **This AGENTS.md** — Did Rust-domain routing or crate boundaries change?
- [ ] **Child crate AGENTS.md** — Did a crate gain new constraints, pitfalls, or internal structure?
- [ ] **`../docs/memory/known-gotchas.md`** — Did you discover a Rust/runtime edge case?
- [ ] **`../docs/architecture.md`** — Did Rust data flow or runtime wiring change?
- [ ] **`../CHANGELOG.md`** — Is this user-visible?
