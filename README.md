# any-converter

`any-converter` is an LLM API gateway for routing one client format to a different upstream provider format.

It is built for practical cross-vendor setups such as:

- Claude Code -> OpenAI-compatible or Gemini backends
- Codex CLI / Responses clients -> Claude backends
- OpenAI-compatible apps -> Anthropic or Gemini
- Multi-provider routing, failover, and observability behind one local endpoint

## What it supports

### Client and upstream API formats

| Format                  | Endpoint                                 |
| ----------------------- | ---------------------------------------- |
| OpenAI Chat Completions | `/v1/chat/completions`                   |
| OpenAI Responses        | `/v1/responses`                          |
| Claude Messages         | `/v1/messages`                           |
| Google Gemini           | `/v1beta/models/{model}:generateContent` |

### Gateway capabilities

- Request and response conversion across all supported formats
- Streaming SSE conversion
- Multi-provider routing by model pattern with `model_routes`
- Provider failover with `priority` or `round_robin`
- Model name remapping with exact and wildcard matches
- Request logging with redaction
- Dual log storage: SQLite primary mirror plus JSONL fallback
- One UI for playground, logs, usage, status, and config editing — the same page served to any browser and loaded by the Desktop app
- Optional boot autostart via `any-converter daemon install` (launchd on macOS, systemd user unit on Linux)
- Desktop app for local provider, route, secret, and lifecycle management

## Quick start

### 1. Install

```bash
cargo install --path crates/cli
```

### 2. Bootstrap the canonical home

Canonical configuration contains secret references, never secret values:
`config.example.toml` shows the resulting shape. Do not create it by hand on
the first run: launch Desktop to create its keychain entries, or import a
temporary legacy config containing literal keys once.

```toml
[server]
host = "127.0.0.1"
port = 3028
api_key = "keychain:any-converter:server:inference"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "keychain:any-converter:provider:anthropic"

[[model_routes]]
pattern = "*"
provider = "anthropic"
```

### 3. Start the gateway

```bash
any-converter serve
```

Your local gateway is now available at `http://127.0.0.1:3028`. `serve` prints
a URL with a one-time page key (`http://127.0.0.1:3028/?k=…`) — open it in a
browser and the session unlocks; the key is never needed again until restart.

To start the gateway at login instead, install it as a user service (no `sudo`,
no system install):

```bash
any-converter daemon install   # launchd (macOS) / systemd user unit (Linux)
any-converter daemon status    # prints the service state and the UI URL
any-converter daemon stop      # also: start / restart / uninstall
```

Desktop creates the canonical home and keychain entries on first launch. For a
headless first run, set `ANY_CONVERTER_UPSTREAM_KEY`, run `serve --format ...
--base-url ... --home ...`, and the key is imported once; later starts use
`serve` alone. A temporary legacy config can also be imported once with
`serve --config bootstrap.toml`, but an initialized home is never overwritten.

## Subscription accounts (OAuth)

Besides plain API keys, a provider can be served by a signed-in subscription
account. The account signs its own requests, its tokens are refreshed
automatically, and the wire it speaks is the vendor's own:

| Account               | Wire                                                                                                                 |
| --------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `gemini:<email>`      | Google Code Assist — `POST {base}/v1internal:generateContent`, body wrapped as `{"model":…,"request":<gemini body>}` |
| `antigravity:<email>` | The same, on Antigravity's own OAuth app and daily Code Assist host                                                  |
| `codex:<email>`       | ChatGPT's Codex backend, `POST {base}/responses`                                                                     |
| `copilot:<login>`     | `api.githubcopilot.com`, Chat or Responses per model                                                                 |
| `chatgpt-api:<email>` | OpenAI's own API, `POST https://api.openai.com/v1/responses`                                                         |
| `claude:<email>`      | Anthropic Messages, signed in through Claude Code                                                                    |

```bash
# 1. Sign in (runs the browser PKCE flow; paste the callback address if the
#    browser can't reach this machine)
any-converter accounts add gemini      # or antigravity / codex / copilot /
                                       #    chatgpt-api / claude

# 2. See what is stored — ids and kinds only, never token material
any-converter accounts list

# 3. Point a provider at the account (the CLI prints this snippet for you)
```

```toml
[[providers]]
name = "gemini"
format = "gemini"
base_url = "https://cloudcode-pa.googleapis.com"
api_key = ""                            # unused: the account signs
account = "gemini:you@example.com"
```

**Sign in with ChatGPT** (`chatgpt-api`) is the one flow that always ends by
pasting: OpenAI registers this app as it signs in, so the address the browser
ends on carries both the code and the client id it issued. Each account keeps
that client id, and this machine keeps one `urn:uuid:` id (`chatgpt-api-host`)
that OpenAI sees every time.

**Claude** is signed in by Claude Code itself — any-converter runs
`claude auth login --claudeai` in a temporary config directory and takes what
it produces, so the `claude` command line must be installed.

An account replaces `api_key` entirely. A `401` that says the token expired
refreshes and retries once; one that says the sign-in was revoked marks the
account (in `revoked.json`, next to `secrets.json`) and fails over, and
`any-converter accounts add` clears the mark on a fresh sign-in. Refreshes
are serialized across processes by a lock file, so the CLI and the server
never spend the same refresh token twice.

Credentials live in `secrets.json` (mode `0600`, atomic writes, last-good
backup); `ANY_CONVERTER_SECRETS` overrides the path.

## Connect coding agents in one command

The `agents` commands wire a coding agent's own config to the gateway for you —
base URL, a per-agent gateway key, and the full model catalog — and disconnect
restores the config **byte for byte** (a `.before-any-converter` backup sits
beside each touched file as the offline rescue copy):

```bash
any-converter agents list                    # who's installed, who's connected
any-converter agents connect claude-code     # wire Claude Code (needs the server running)
any-converter agents connect pi --model deepseek/deepseek-chat
any-converter agents disconnect claude-code  # restore the pre-connect config
any-converter agents sync                    # refresh model catalogs after config changes
any-converter agents rotate-key pi           # issue a fresh per-agent key
```

Supported agents: **Claude Code**, **Codex**, **Gemini CLI**, and **Pi**.
The UI's Agents page offers the same operations with drift detection: if
another tool rewrites an agent's config while it is connected, the page shows
a "config changed externally" banner and a one-click reapply.

Each connected agent gets its own gateway key (`acw_<agent>_…`), so requests
are attributed without relying on User-Agent, keys rotate independently, and
agent keys can call inference endpoints but never the agents management API.

Notes:

- The server must be running (`any-converter serve`); the CLI is a thin client.
  If the server is down and you need to disconnect urgently, restore the
  `.before-any-converter` file beside the agent's config manually.
- Codex wires `~/.codex` (the main `CODEX_HOME`); its OAuth sign-in is never
  touched — the gateway key goes into `auth.json`'s `OPENAI_API_KEY` field.
- Claude Code: a project-level `settings.local.json` overrides user-level
  settings, so a per-project `env` block can shadow the wiring there.

## Common client setups

Manual wiring, for clients without an `agents` integration (or when you prefer
editing configs yourself):

### Claude Code

Point Claude-compatible traffic at the gateway:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3028
export ANTHROPIC_AUTH_TOKEN=<inference-key>
```

### Codex CLI and other Responses clients

Use the OpenAI Responses wire API, not Chat Completions:

```toml
base_url = "http://127.0.0.1:3028/v1"
wire_api = "responses"
api_key = "<inference-key>"
```

Important:

- `base_url` should end at `/v1`
- `wire_api` should be `"responses"`
- If you expose `/v1/models` to Codex, keep model names lowercase in your config

### OpenAI-compatible clients

```bash
export OPENAI_BASE_URL=http://127.0.0.1:3028/v1
export OPENAI_API_KEY=<inference-key>
```

## Routing models to providers

The recommended setup is `model_routes`, not legacy format-only routes.

```toml
[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "keychain:any-converter:provider:openai"

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "keychain:any-converter:provider:deepseek"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "keychain:any-converter:provider:anthropic"

[[model_routes]]
pattern = "gpt-*"
providers = ["openai", "deepseek"]
strategy = "priority"

[[model_routes]]
pattern = "claude-*"
provider = "anthropic"

[[model_routes]]
pattern = "*"
provider = "openai"
upstream_model = "gpt-4.1-mini"
```

Notes:

- `priority` tries providers in order
- `round_robin` rotates the starting provider while keeping failover
- `upstream_model` lets you pin the final model sent upstream

## Model mapping

Each provider can rewrite incoming model names before the upstream request is sent:

```toml
[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "keychain:any-converter:provider:openai"

[providers.model_map]
"claude-sonnet-4" = "gpt-4.1"
"gpt-4o*" = "gpt-4.1"
"*" = "gpt-4.1-mini"
```

Matching order is exact match -> longest wildcard match -> passthrough.

## Observability

Enable request logging:

```toml
[logging]
level = "info"
dir = "./logs"
max_disk_mb = 500
retention_days = 30          # request metadata
body_retention_days = 7      # captured bodies (biggest by far)
rollup_retention_days = 0    # hourly aggregates, 0 = forever          # 0 keeps everything
# jsonl_mirror = true        # optional usage.YYYY-MM-DD.jsonl mirror

[logging.request_log]
enabled = true
trace_enabled = true
```

This gives you:

- JSONL audit files in `./logs`
- SQLite mirror at `./logs/any-converter.sqlite3`
- Request audit files rotate into 10 MiB daily segments such as `requests.2026-07-22.000.jsonl`
- Redacted request and response capture
- Usage extraction for both non-streaming and streaming traffic
- Structured trace summaries for messages, tools, and tool results

Important:

- `logging.request_log.enabled = true` does nothing unless `logging.dir` is also set
- For streaming requests, logged latency is time-to-first-byte, not full stream duration

## Browser UI

The gateway serves the UI itself — there is no second process and no separate
web app to start. Release binaries embed the built SPA; from a repo checkout,
build it once with `pnpm --filter @any-converter/desktop build` (or point
`ANY_CONVERTER_WEB_DIR` at any directory containing the built `index.html`).

Open the URL `serve` (or `daemon status`) prints. The first visit asks for the
page key; after that the browser holds a loopback-only session cookie. The UI
provides:

- `/playground` for interactive conversion
- `/logs` for request log inspection
- `/usage` for token and latency views, with ledger CSV export
- `/settings` for editing `config.toml`
- `/accounts` and `/agents` for subscription accounts and agent wiring

## Desktop app

The Tauri desktop app provides a local control plane for:

- Provider and model route management
- Embedded server start, stop, and restart
- Local logs and usage views
- Conversion playground

It talks to the same `/v1/admin/*` API over Tauri IPC. If a gateway is
already running for the home (for example under `daemon`), the app attaches to
it instead of starting a second instance, and lifecycle controls step aside —
the gateway keeps running when the window closes.

Run it from the **repo root** (always reinstall after pulling dependency changes):

```bash
pnpm install
pnpm --filter @any-converter/desktop tauri dev
pnpm --filter @any-converter/desktop tauri build
```

`tauri build` runs `pnpm install && pnpm build` before packaging, so a fresh pull that adds frontend deps (for example `react-router-dom`) will not fail with `TS2307: Cannot find module` once install completes.

## CLI utilities

### Convert JSON payloads offline

```bash
echo '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}' \
  | any-converter convert --from openai-chat --to claude
```

### Convert SSE streams

```bash
curl -N https://example.com/stream \
  | any-converter stream --from openai-chat --to claude
```

## Troubleshooting

### Codex fails even though the gateway is up

Check all three:

- client uses `wire_api = "responses"`
- client points to `.../v1`
- gateway route resolves to a valid upstream provider for that model

### Logs are empty

Check all three:

- `logging.dir` is set
- `[logging.request_log].enabled = true`
- the server process has actually handled requests since startup

## More documentation

- Full config example: [config.example.toml](./config.example.toml)
- Build and run details: [docs/build.md](./docs/build.md)
- Architecture and data flow: [docs/architecture.md](./docs/architecture.md)
- Chinese README: [README-zh.md](./README-zh.md)

## License

MIT
