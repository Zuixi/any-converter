# Scripts Domain

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Utility shell scripts for local workflows, environment setup, and operational helpers.

## Current Contents

- `build.mjs`:
  cross-platform (macOS / Linux / Windows) build runner with RAM-aware Cargo/Node concurrency limits and process-tree cleanup
- `fetch-node.mjs`:
  downloads the bundled Node runtime sidecar required by Tauri desktop builds
- `guard-no-loopback-fetch.sh`:
  guard script ensuring frontend code uses same-origin paths instead of hardcoded loopback URLs
- `om-any-converter-tunnel.sh`:
  helper for starting the service and wiring SSH port forwarding for remote testing

## Boundary Rules

1. Keep scripts operational and task-focused.
2. If a script becomes product behavior rather than a local helper, move it into the owning runtime or app.
3. Document user-facing script behavior in `README` or `docs/build.md` when appropriate.
