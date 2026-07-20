# Docs Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** This file routes documentation work by audience and depth. Read only the subsection that matches the doc you are changing.

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| User setup, local run, build, and Web/Desktop usage docs | [`build.md`](./build.md) |
| System data flow, runtime topology, storage, and interfaces | [`architecture.md`](./architecture.md) |
| Living gotchas and project context for agents | [`memory/AGENTS.md`](./memory/AGENTS.md) |
| Product or implementation design notes | `design/*.md` |
| Reference conventions such as ASCII diagrams or API notes | `references/*.md` |
| Root entrypoint or workspace routing docs | [`../AGENTS.md`](../AGENTS.md) |

## Documentation Layers

- `README*.md`:
  user-facing product and setup summary
- `build.md`:
  operational and developer runbook
- `architecture.md`:
  system explanation and component relationships
- `memory/*`:
  persistent agent context and pitfalls
- `design/*`:
  design proposals and planning artifacts
- `references/*`:
  reusable conventions and reference material

## Boundary Rules

- Keep user-facing docs concise and task-oriented.
- Keep architecture docs explanatory, not procedural.
- Keep memory docs focused on pitfalls and volatile context, not general onboarding.
- If a docs change introduces a new subtree or new long-lived audience split, consider whether that subtree now needs its own `AGENTS.md`.
