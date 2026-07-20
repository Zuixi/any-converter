# Web Bridge Crate

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Native `napi-rs` bridge exposing Rust conversion functions to the Web application through `@any-converter/bridge`.

## Read This When

- You are changing `src/lib.rs`
- You are changing exported native functions or their JS-visible shapes
- You are debugging native bridge build/runtime loading issues

## Local Structure

```
src/lib.rs   # napi exports and bridge glue
build.rs     # native build integration
Cargo.toml   # crate definition
```

## Boundary Rules

1. Conversion logic still belongs in [`../core/AGENTS.md`](../core/AGENTS.md); this crate is a transport shim.
2. JS package loading and TypeScript wrapper logic belong in [`../../packages/bridge/AGENTS.md`](../../packages/bridge/AGENTS.md).
3. Keep exported functions narrow and stable; bridge surface changes ripple into the Web app.
