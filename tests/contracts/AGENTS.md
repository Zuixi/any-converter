# Contract Fixtures

> **Parent:** [`../AGENTS.md`](../AGENTS.md)

Sanitized record-replay fixtures for gateway contract tests.
Runner: `crates/server/tests/contract_replay.rs`.
Do not store secrets. Prefer copying from `crates/core/tests/fixtures/` then adapting.

## Commands

- `cargo test -p any-converter-server --test contract_replay` — replay every case.

## Rules

- One directory per case: `tests/contracts/<tool>/<case>/` with `meta.toml`,
  `client.request.json`, `upstream.request.json`, and `upstream.response.{json,sse}` +
  `expected.client.response.{json,sse}`.
- `meta.toml` `name` must equal the directory name; `mode` is `identity` or `convert`.
- Never commit real keys, tokens, emails, tenant ids, or absolute home paths. CI rejects
  `sk-` key strings under this tree.
- Compare structure, not run-to-run ids: list unstable dot-paths in `meta.normalize`.
- Fixtures are the client-integration CI lock: when a conversion legitimately changes,
  update the expectation file and say why in the commit message — never loosen the
  runner to make it pass.
- See [`README.md`](./README.md) for the full schema and recording/sanitizing steps.
