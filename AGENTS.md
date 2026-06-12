# any-converter — Agent Entrypoint

> **What is this?** A Rust workspace that translates between major LLM provider API formats via a typed Intermediate Representation.
>
> **Progressive Disclosure**: This file is the navigation hub only. Pick your task below, then drill down to the crate-specific `AGENTS.md`.

---

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Format conversion logic (parsing, serialization, IR) | [`crates/core/AGENTS.md`](./crates/core/AGENTS.md) |
| HTTP routing, proxying, auth, or streaming SSE | [`crates/server/AGENTS.md`](./crates/server/AGENTS.md) |
| CLI commands, argument parsing, or config assembly | [`crates/cli/AGENTS.md`](./crates/cli/AGENTS.md) |
| Adding a **new LLM API format** | [`crates/core/AGENTS.md`](./crates/core/AGENTS.md) **+** update [`crates/AGENTS.md`](./crates/AGENTS.md) |
| Bug spanning multiple crates / cross-crate refactor | Start at [`crates/AGENTS.md`](./crates/AGENTS.md) (universal constraints) |
| Documentation or project-wide rules | This file + [`crates/AGENTS.md`](./crates/AGENTS.md) |

---

## Quick Orientation

### Crate Graph

```
┌─────────────────────────────────────────────────────────────┐
│                      CLI Binary (any-converter)              │
│                   ┌──────────────────┐                      │
│                   │  crates/cli      │                      │
│                   └────────┬─────────┘                      │
│                            │ depends on                     │
│            ┌───────────────┴───────────────┐                │
│            ▼                               ▼                │
│   ┌─────────────────┐            ┌─────────────────┐       │
│   │  crates/server  │            │  crates/core    │       │
│   │  (axum + reqwest│◄───────────│  (pure serde    │       │
│   │   + tokio)      │  depends on│   library)      │       │
│   └─────────────────┘            └─────────────────┘       │
│                                                         │
└─────────────────────────────────────────────────────────────┘
```

### Key Documents

| Document | Purpose | When to read |
|----------|---------|--------------|
| [`crates/AGENTS.md`](./crates/AGENTS.md) | Universal constraints + crate navigation | Before any cross-crate work |
| [`docs/architecture.md`](./docs/architecture.md) | Full system architecture (529 lines) | When you need the big picture |
| [`docs/memory/AGENTS.md`](./docs/memory/AGENTS.md) | Known gotchas + project context | Before touching code |
| [`docs/build.md`](./docs/build.md) | Build & test commands | When setting up |
| [`CHANGELOG.md`](./CHANGELOG.md) | Version history with root-cause analysis | To understand recent changes |

---

## Maintenance Checklist

Before finishing this task, review whether the following docs need updates:

- [ ] **This AGENTS.md** — Did you introduce new architectural patterns or constraints?
- [ ] **Crate AGENTS.md** — Did you change crate-specific rules, boundaries, or pitfalls?
- [ ] **`docs/memory/known-gotchas.md`** — Did you discover a new critical edge case?
- [ ] **`docs/memory/project_context.md`** — Did the project structure or scope change?
- [ ] **`docs/architecture.md`** — Did you add/remove components or change data flow?
- [ ] **`CHANGELOG.md`** — Is this a user-visible fix or feature?
- [ ] **`README.md`** — Did public APIs or setup steps change?

**Rule:** If you checked any box, update the corresponding file before ending the session. Ask the user for confirmation before adding new permanent principles.
