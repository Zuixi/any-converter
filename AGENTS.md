# any-converter — Agent Entrypoint

> **What is this?** A workspace for running one LLM client protocol against a different upstream provider protocol, with conversion, routing, logging, Web UI, and Desktop control surfaces.
>
> **Progressive Disclosure**: This file is the workspace navigation hub only. Pick the domain you are changing, then drill down into that domain's `AGENTS.md`.

---

## Workspace Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| Rust conversion, proxying, CLI, or native bridge code | [`crates/AGENTS.md`](./crates/AGENTS.md) |
| Web or Desktop application code | [`apps/AGENTS.md`](./apps/AGENTS.md) |
| Shared frontend components, hooks, types, or UI primitives | [`packages/AGENTS.md`](./packages/AGENTS.md) |
| Build, architecture, design notes, or memory docs | [`docs/AGENTS.md`](./docs/AGENTS.md) |
| Cross-workspace test fixtures or future integration tests | [`tests/AGENTS.md`](./tests/AGENTS.md) |
| Utility shell scripts and local workflow helpers | [`scripts/AGENTS.md`](./scripts/AGENTS.md) |
| Bug spanning multiple domains | Start here, then read the relevant domain `AGENTS.md` files |

---

## Quick Orientation

### Workspace Map

```
any-converter/
├── crates/     # Rust runtime and native bridge code
├── apps/       # User-facing Web and Desktop applications
├── packages/   # Shared frontend building blocks
├── docs/       # Build, architecture, design, and memory docs
├── tests/      # Cross-workspace fixtures / future integration harnesses
└── scripts/    # Local utility scripts
```

### Primary Runtime Graph

```
Clients
  │
  ▼
apps/web ───────────────┐
apps/desktop ───────────┼────► crates/server ───► upstream providers
                        │
packages/views/core/ui ─┘

crates/cli ─────────────► crates/server / crates/core
packages/bridge ────────► crates/web-bridge ─► crates/core
```

### Read These Next

| Document | Purpose | When to read |
|----------|---------|--------------|
| [`crates/AGENTS.md`](./crates/AGENTS.md) | Rust-domain routing and constraints | Before touching any Rust crate |
| [`apps/AGENTS.md`](./apps/AGENTS.md) | App-domain routing for Web and Desktop | Before touching app entrypoints |
| [`packages/AGENTS.md`](./packages/AGENTS.md) | Shared frontend package boundaries | Before touching hooks, views, or shared types |
| [`docs/AGENTS.md`](./docs/AGENTS.md) | Documentation routing | Before updating docs beyond a single file |
| [`docs/architecture.md`](./docs/architecture.md) | Full architecture and data flow | When you need the big picture |
| [`docs/memory/AGENTS.md`](./docs/memory/AGENTS.md) | Known gotchas and project context | Before touching tricky behavior |

---

## Boundary Rules

- This file should stay high-level. Do not duplicate crate-level or package-level implementation details here.
- Every child `AGENTS.md` should link back to its parent and route further down when needed.
- Prefer adding or updating the narrowest relevant `AGENTS.md` rather than expanding this file with component detail.

---

## Maintenance Checklist

Before finishing a task, review whether the following docs need updates:

- [ ] **This AGENTS.md** — Did you change workspace-level routing or domain boundaries?
- [ ] **Domain AGENTS.md** — Did you add or reshape a major area under `crates/`, `apps/`, `packages/`, `docs/`, `tests/`, or `scripts/`?
- [ ] **Component AGENTS.md** — Did you change a component's ownership boundary, substructure, or pitfalls?
- [ ] **`docs/memory/project_context.md`** — Did project structure or product scope change?
- [ ] **`docs/architecture.md`** — Did cross-domain data flow or runtime topology change?
- [ ] **`CHANGELOG.md`** — Is this a user-visible fix or feature?
- [ ] **`README.md` / `README-zh.md`** — Did public setup or user-facing behavior change?

**Rule:** If you add a new lasting rule or principle for future agents, ask the user before encoding it as a permanent constraint.
