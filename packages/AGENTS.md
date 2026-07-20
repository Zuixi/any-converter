# Packages Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** This file routes shared frontend package work. After selecting a package, read that package's own `AGENTS.md`.

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Shared business hooks and reusable composed components | [`core/AGENTS.md`](./core/AGENTS.md) |
| Page-level shared views used by apps | [`views/AGENTS.md`](./views/AGENTS.md) |
| Shared types, constants, and simple utilities | [`shared/AGENTS.md`](./shared/AGENTS.md) |
| UI primitives and low-level design system pieces | [`ui/AGENTS.md`](./ui/AGENTS.md) |
| JS wrapper around the native Rust bridge | [`bridge/AGENTS.md`](./bridge/AGENTS.md) |
| Shared TypeScript compiler presets | [`tsconfig/AGENTS.md`](./tsconfig/AGENTS.md) |

## Package Overview

| Package | Purpose |
|---------|---------|
| `@any-converter/core` | Shared hooks, API client abstraction, and composed components |
| `@any-converter/views` | Shared page-level view assemblies |
| `@any-converter/shared` | Shared types, constants, and utilities |
| `@any-converter/ui` | Low-level UI primitives and styling helpers |
| `@any-converter/bridge` | JS package exposing the native conversion bridge |
| `@any-converter/tsconfig` | Shared TypeScript config presets |

## Shared Constraints

1. `packages/shared` should stay dependency-light and foundational.
2. `packages/ui` should remain presentational and low-level.
3. `packages/core` may depend on `shared` and `ui`; `views` may depend on `core`, `shared`, and `ui`.
4. App-specific shell concerns belong in `apps/`, not here.
