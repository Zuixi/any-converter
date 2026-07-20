# Shared Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Shared types, constants, and simple utilities consumed across apps and frontend packages.

## Read This When

- You are changing `src/types/*`
- You are changing shared constants or format labels
- You are changing tiny dependency-light utilities

## Boundary Rules

1. Keep this package foundational and dependency-light.
2. Do not put React hooks, JSX components, or app-specific behavior here.
3. If a change affects Rust-facing contracts, verify the corresponding runtime and docs under [`../../crates/AGENTS.md`](../../crates/AGENTS.md).
