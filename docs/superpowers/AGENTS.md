# Superpowers Specs & Plans

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Progressive Disclosure:** Design specs and bite-sized implementation plans produced via the Superpowers workflow (`brainstorming` → spec → `writing-plans` → execution).

## Task Routing Matrix

| If you are working on... | Go to |
|--------------------------|-------|
| An approved design / requirements doc | [`specs/`](./specs/) |
| A task-by-task implementation plan | [`plans/`](./plans/) |
| Transparent proxy product design (pre-superpowers) | [`../design/2026-08-05-transparent-proxy.md`](../design/2026-08-05-transparent-proxy.md) |

## Current Artifacts

| Artifact | Path |
|----------|------|
| Automated testing harness design | [`specs/2026-08-13-automated-testing-harness-design.md`](./specs/2026-08-13-automated-testing-harness-design.md) |
| Automated testing harness plan | [`plans/2026-08-13-automated-testing-harness.md`](./plans/2026-08-13-automated-testing-harness.md) |

## Boundary Rules

1. Specs describe **what / why / success criteria**. Plans describe **how / which files / TDD steps**.
2. Do not duplicate long product designs that already live under `docs/design/` — link them from the spec instead.
3. When a plan finishes, update related tests/docs/config and tick plan checkboxes (or note deviations in the PR).
4. Execution should use `subagent-driven-development` or `executing-plans` after the human chooses an approach.

## Maintenance Checklist

- [ ] New spec added under `specs/YYYY-MM-DD-<topic>-design.md`?
- [ ] Matching plan under `plans/YYYY-MM-DD-<topic>.md` with Spec link?
- [ ] Parent [`../AGENTS.md`](../AGENTS.md) still routes here?
