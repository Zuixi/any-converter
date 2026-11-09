# AnyConverter Resident Gateway

Status: accepted for v1

Supersedes: the SQLite-as-configuration-authority assumptions in
[`desktop_design.md`](./desktop_design.md)

## 1. Product contract

AnyConverter is a long-lived local gateway. The desktop app is a supervisor
and management console for that gateway, not a second application runtime with
its own configuration database.

The server owns:

- the authoritative `config.toml`;
- secret references stored in that file;
- the management API;
- process readiness and the active listener;
- request routing and observability.

The desktop app, CLI, and Web UI own presentation and transport only. They may
cache a snapshot for rendering, but a cache is never a second configuration
authority.

## 2. Ownership matrix

| Concern                                  | Authority                                   | Allowed caches / adapters       |
| ---------------------------------------- | ------------------------------------------- | ------------------------------- |
| Server settings, providers, model routes | `ConfigStore`                               | Typed admin DTOs, UI form state |
| Provider API keys                        | OS keychain                                 | Keychain reference in TOML      |
| Inference/client key                     | OS keychain                                 | Keychain reference in TOML      |
| Management key                           | OS keychain                                 | Keychain reference in TOML      |
| OAuth account credentials                | `secrets.json`                              | In-memory account resolver      |
| Request/usage records                    | server SQLite writer                        | Read-only desktop/Web readers   |
| Process lifecycle                        | `ServerManager` (desktop) or foreground CLI | UI status projection            |
| Network address                          | successful `TcpListener::bind`              | Supervisor status               |

SQLite remains valid for request logs and migration bookkeeping. It must not be
used to construct the next server configuration after the migration described
in section 9.

## 3. Configuration model

`config.toml` stays human-editable. Every read and write must go through
`ConfigStore`, which provides:

- strict parsing and semantic validation;
- a SHA-256 revision over exact file bytes;
- compare-and-swap writes using `If-Match`;
- a kernel-backed cross-process lock with no age-based takeover;
- same-directory atomic replacement;
- an optional last-good `.bak`;
- sanitized read models that are not write inputs.

`ServerConfig` is a runtime snapshot and is never serialized wholesale. Public
API responses use explicit DTOs. Secret fields are represented as
`configured: bool`, never as a redacted placeholder that could be written back.

Writes use typed management operations, not replacement with a sanitized TOML
document. A document containing the literal `<redacted>` sentinel in a secret
field is rejected.

## 4. Secret model

The server crate owns secret-reference parsing and resolution. Desktop, CLI,
and Web must not each invent a keychain namespace.

Canonical references:

```text
keychain:any-converter:provider:<provider-name>
keychain:any-converter:server:inference
keychain:any-converter:server:management
```

The legacy provider form `keychain:any-converter:<provider-name>` is readable
for migration but never written by new code.

The default home keeps the compatibility service name `any-converter`. A
non-default `ANY_CONVERTER_HOME` uses a deterministic keychain service derived
from the normalized root, so multiple installations cannot share credentials.

Rules:

- A missing or unreadable referenced secret fails the operation closed.
- A keychain reference is never sent to an upstream provider.
- A configured reference that cannot be resolved prevents server startup.
- Local anonymous inference is allowed when no inference key is configured;
  this does not make the management API anonymous.
- Management requests accept only the management key. Agent keys are
  inference-only and never satisfy management authentication.
- Authentication comparisons are constant time.

The first managed start bootstraps a random management key in the system
keychain and writes only its reference to `[management]`. An interrupted first
start reuses an already-created management key instead of replacing it. If the
keychain is unavailable, startup fails with an actionable error instead of
exposing an unauthenticated management surface.

## 5. Management API

Management endpoints live under `/v1/admin`:

```text
GET   /v1/admin/config
PATCH /v1/admin/config
GET   /v1/admin/server/status
PUT   /v1/admin/secrets/provider/{provider}
DELETE /v1/admin/secrets/provider/{provider}
PUT   /v1/admin/secrets/inference
DELETE /v1/admin/secrets/inference
POST  /v1/admin/secrets/management/rotate
```

Configuration reads return a sanitized typed DTO with an `ETag` equal to the
config revision. Mutations require `If-Match`. A missing or stale precondition
returns `412 Precondition Failed` and never writes.

Patch operations preserve comments and unrelated TOML keys by editing the
existing `toml_edit::Document`. Secret mutations are separate endpoints so a
secret never appears in the structured configuration body.

The v1 patch surface is deliberately narrow:

- server host/port/proxy;
- provider upsert/remove;
- model-route upsert/remove.

Unknown fields are rejected by serde. A successful config or secret mutation
returns `restart_required: true`; v1 applies runtime changes through a
controlled restart, not partial hot reload.

## 6. Authentication matrix

| Surface                            | Credential                 | No credential configured  |
| ---------------------------------- | -------------------------- | ------------------------- |
| Inference endpoints                | inference key or agent key | local anonymous inference |
| Admin and account/agent management | management key             | fail closed               |
| `/health`                          | none                       | public                    |
| Request/usage reads                | management key             | fail closed               |

An invalid or missing management credential returns `401`. A recognized
credential without management privilege returns `403`. A server built without
a management-auth extension returns `503` for management routes.

## 7. Lifecycle state machine

```text
Stopped -> Starting -> Running -> Stopping -> Stopped
                 \-> Failed
Running -> Failed (unexpected task exit)
```

Semantics:

- `Starting` begins before configuration resolution and never implies that a
  listener exists.
- The server reports readiness only after `TcpListener::bind` succeeds.
- `Running` is entered only from a readiness event and records the actual
  listener address, not merely the configured host.
- `Stopping` is entered before shutdown is signalled.
- `Stopped` is entered only after the server task has joined.
- Bind failure and unexpected task exit enter `Failed` with a stable,
  actionable error.
- A generation token prevents a late completion from an old task from
  overwriting the state of a newer task.
- Drain has a bounded deadline. A timeout is reported as `Failed`; the
  supervisor does not claim a clean stop it did not observe.

## 8. Startup and restart

Startup order:

1. Resolve `Home`.
2. Acquire the kernel-backed instance lock (no age takeover) and verify
   `instance.json`.
3. Load and validate `config.toml` through `ConfigStore`.
4. Bootstrap or resolve the management key.
5. Resolve all referenced provider/inference secrets.
6. Build router and observability state.
7. Bind the listener.
8. Publish readiness and write instance metadata.
9. Serve until shutdown, then drain and remove owned metadata.

The desktop supervisor uses the same sequence through the server crate rather
than reconstructing configuration itself.

Configuration changes in v1 use controlled restart:

```text
Running -> Stopping -> Stopped -> Starting -> Running
```

The management response reports that restart is required. It does not imply a
live configuration swap.

## 9. Desktop migration

The existing desktop SQLite configuration is a legacy source that must be
migrated once:

1. Read and validate the legacy settings/providers/routes.
2. Write each plaintext provider/server key to the keychain under the canonical
   namespace only when that entry is absent; an equal value is idempotent, while
   a conflicting existing value fails closed and is never overwritten.
3. Build the canonical `config.toml` through `ConfigStore`.
4. Verify that every secret reference resolves.
5. Record migration completion.
6. Remove plaintext secret columns/settings only after successful verification.

Migration is idempotent, resumable, and never deletes the old source when a
verification step fails. The legacy file is retained until an explicit cleanup
step.

Canonical `Home` is `%APPDATA%\any-converter` on Windows and
`$XDG_CONFIG_HOME/any-converter` (or `$HOME/.config/any-converter`) elsewhere.
`ANY_CONVERTER_HOME` overrides the root; `ANY_CONVERTER_SECRETS` overrides only
the OAuth store.

## 10. Desktop process behavior

The Tauri shell owns:

- one managed server instance per canonical home;
- single-instance activation;
- tray/close-to-hide behavior;
- typed IPC;
- lifecycle event projection.

The webview never dials loopback directly. It calls typed Tauri commands. The
generic management proxy is restricted to a typed allowlist and must not fall
back to an external server when the managed instance is stopped, unless the
user explicitly selected external-server mode.

A failed or unsupported legacy migration marks the supervisor `Failed` and
blocks automatic gateway startup. Starting remains an explicit operator action.

## 11. Failure semantics

- Invalid TOML or semantic config: fail before bind, preserve previous file.
- Revision conflict: `409` plus the current revision; caller must re-read.
- Stale `If-Match`: `412`.
- Missing management key: server start fails; no unauthenticated admin route.
- Missing provider key: server start fails and names the provider.
- Conflicting initialization/migration key: fail closed without replacing the
  existing keychain value.
- Port in use: `Failed`, error includes the attempted address.
- Drain timeout: `Failed`, error names the remaining drain deadline.
- Keychain unavailable: fail closed with recovery guidance.

## 12. Non-goals for v1

- Full hot reload.
- Socket handover or zero-downtime replacement.
- Running as a Windows Service or systemd unit.
- A second configuration database.
- Generic file/IPC command execution.
- A new frontend framework or state library.

## 13. Acceptance criteria

- `ConfigStore` rejects stale writes, invalid UTF-8, semantic errors, and
  redacted-secret round trips.
- Concurrent writers cannot silently overwrite each other.
- Provider, inference, and management secrets resolve through one server-owned
  abstraction.
- A missing keychain entry prevents startup or the affected secret operation.
- Admin requests without a management key never mutate state or reach an
  upstream.
- `Running` is observed only after bind readiness.
- Stop reports `Stopped` only after task join.
- Desktop no longer builds server configuration from SQLite after migration.
- Route inventory, auth, lifecycle, and migration tests cover the above.
