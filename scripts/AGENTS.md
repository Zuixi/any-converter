# Scripts Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Utility shell scripts for local workflows, environment setup, and operational helpers.

## Current Contents

- `om-any-converter-tunnel.sh`:
  helper for starting the service and wiring SSH port forwarding for remote testing
- `pr-create.sh`:
  open a GitHub PR from a feature branch; never force-pushes `main`; pairs with
  `.githooks/commit-msg` which strips `Co-authored-by: Cursor <cursoragent@cursor.com>`

## Boundary Rules

1. Keep scripts operational and task-focused.
2. If a script becomes product behavior rather than a local helper, move it into the owning runtime or app.
3. Document user-facing script behavior in `README` or `docs/build.md` when appropriate.
