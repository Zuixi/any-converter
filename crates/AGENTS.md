# Crates

Instructions for AI coding agents working on this repo.

> **Progressive Disclosure**: This file contains the **navigation hub** and **universal constraints** only.
> Each crate has its own `AGENTS.md` with domain-specific rules. Start here for orientation, then drill down.

## Task → Crate Quick Reference

| If you are working on... | Go to |
|--------------------------|-------|
| Format conversion logic (parsing, serialization, pairwise converters) | [`core/AGENTS.md`](./core/AGENTS.md) |
| HTTP routing, proxying, auth, streaming SSE | [`server/AGENTS.md`](./server/AGENTS.md) |
| CLI commands, argument parsing, config assembly | [`cli/AGENTS.md`](./cli/AGENTS.md) |
| Adding a **new LLM API format** | [`core/AGENTS.md`](./core/AGENTS.md) **+** update this file |
| Bug spanning multiple crates | This file (universal constraints) |

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

## Documentation Maintenance

Before finishing a task that touches this crate, verify:

- [ ] **This AGENTS.md** — Did you add/modify crate constraints, architecture, or pitfalls?
- [ ] **Root AGENTS.md** — Did you introduce a new crate-level pattern that affects cross-crate routing?
- [ ] **`../docs/memory/known-gotchas.md`** — Did you discover a new edge case specific to this crate?
- [ ] **`../docs/architecture.md`** — Did you change this crate's public interface or data flow?
- [ ] **`../CHANGELOG.md`** — Is this a user-visible change?

**Rule:** If any box is checked, update the corresponding file before ending the session.
