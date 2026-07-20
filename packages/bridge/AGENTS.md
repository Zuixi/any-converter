# Bridge Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** JavaScript wrapper package around the native `napi-rs` module built from `crates/web-bridge`.

## Read This When

- You are changing `src/index.ts`
- You are changing the JS/TS surface exposed to the Web app
- You are debugging native bridge loading behavior

## Boundary Rules

1. JS wrapper shape lives here; native Rust implementation lives in [`../../crates/web-bridge/AGENTS.md`](../../crates/web-bridge/AGENTS.md).
2. Keep this package thin and type-safe.
3. If exported bridge functions change, sync Web callers and any related docs.
