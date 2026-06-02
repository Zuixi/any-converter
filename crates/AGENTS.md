# Crates

Instructions for AI coding agents working on this repo.

> **Progressive Disclosure**: This file contains the **navigation hub** and **universal constraints** only.
> Each crate has its own `AGENTS.md` with domain-specific rules. Start here for orientation, then drill down.

## Crate Overview

| Crate | Path | Purpose | Key Docs |
|-------|------|---------|----------|
| `core` | [`core/`](./core/) | Pure conversion library (no IO, no network) | [`core/AGENTS.md`](./core/AGENTS.md) |
| `server` | [`server/`](./server/) | HTTP proxy server (axum + reqwest) | [`server/AGENTS.md`](./server/AGENTS.md) |
| `cli` | [`cli/`](./cli/) | CLI frontend (clap + tokio) | [`cli/AGENTS.md`](./cli/AGENTS.md) |

**Key Documentations**:
- [ASCII diagrams rules](../docs/references/ascii.md)
- [project architecture](../docs/architecture.md)
- [project memories](../docs/memory/AGENTS.md)

## Universal Constraints (All Crates)

You MUST strictly adhere to the following principles when generating or modifying code:

1. **DRY (Don't Repeat Yourself)**:
   - Reuse existing utilities in `src/utils`.
   - Do NOT copy-paste logic; extract shared functions.
2. **SRP (Single Responsibility Principle)**:
   - A file must do ONE thing.
   - Example: `codeSystemPrompt` handles assembly; `applyMemoryStack` handles memory. Do not merge them.
3. **LoD (Law of Demeter)**:
   - Do not deeply traverse objects. Use data transfer objects (DTOs) or direct imports.
4. **No Overwrites**:
   - **NEVER** overwrite existing components or core logic without explicit user confirmation.
   - Prefer extending functionality via new functions or parameters.

## Maintaining Documentation

When making changes to the codebase, you MUST maintain synchronization:

- **Update `AGENTS.md`**: If you introduce a new architectural pattern or constraint.
- **Update `../docs/memory/`**:
  - `project-context.md`: Update if business scope or project structure changes.
  - `known-gochas.md`: Add any critical pitfall discovered during implementation (e.g., "Do not modify file X without running Y").
- **Post-Run Review**: At the end of every run, re-read `AGENTS.md` and the relevant skill files. If guidance is outdated, propose an update. **ASK the user before adding new permanent principles.**
