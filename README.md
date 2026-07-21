# any-converter

`any-converter` is an LLM API gateway for routing one client format to a different upstream provider format.

It is built for practical cross-vendor setups such as:

- Claude Code -> OpenAI-compatible or Gemini backends
- Codex CLI / Responses clients -> Claude backends
- OpenAI-compatible apps -> Anthropic or Gemini
- Multi-provider routing, failover, and observability behind one local endpoint

## What it supports

### Client and upstream API formats

| Format | Endpoint |
|--------|----------|
| OpenAI Chat Completions | `/v1/chat/completions` |
| OpenAI Responses | `/v1/responses` |
| Claude Messages | `/v1/messages` |
| Google Gemini | `/v1beta/models/{model}:generateContent` |

### Gateway capabilities

- Request and response conversion across all supported formats
- Streaming SSE conversion
- Multi-provider routing by model pattern with `model_routes`
- Provider failover with `priority` or `round_robin`
- Model name remapping with exact and wildcard matches
- Request logging with redaction
- Dual log storage: SQLite primary mirror plus JSONL fallback
- Web UI for playground, logs, usage, status, and config editing
- Desktop app for local provider, route, and embedded server management

## Quick start

### 1. Install

```bash
cargo install --path crates/cli
```

### 2. Create a config

Start from the example:

```bash
cp config.example.toml config.toml
```

Minimum example:

```toml
[server]
host = "127.0.0.1"
port = 8080
api_key = "hello-any"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-xxx"

[[model_routes]]
pattern = "*"
provider = "anthropic"
```

### 3. Start the gateway

```bash
any-converter serve --config config.toml
```

Your local gateway is now available at `http://127.0.0.1:8080`.

## Common client setups

### Claude Code

Point Claude-compatible traffic at the gateway:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
export ANTHROPIC_AUTH_TOKEN=hello-any
```

### Codex CLI and other Responses clients

Use the OpenAI Responses wire API, not Chat Completions:

```toml
base_url = "http://127.0.0.1:8080/v1"
wire_api = "responses"
api_key = "hello-any"
```

Important:

- `base_url` should end at `/v1`
- `wire_api` should be `"responses"`
- If you expose `/v1/models` to Codex, keep model names lowercase in your config

### OpenAI-compatible clients

```bash
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
export OPENAI_API_KEY=hello-any
```

## Routing models to providers

The recommended setup is `model_routes`, not legacy format-only routes.

```toml
[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-xxx"

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
api_key = "sk-proj-xxx"

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

[logging.request_log]
enabled = true
trace_enabled = true
```

This gives you:

- JSONL audit files in `./logs`
- SQLite mirror at `./logs/any-converter.sqlite3`
- Redacted request and response capture
- Usage extraction for both non-streaming and streaming traffic
- Structured trace summaries for messages, tools, and tool results

Important:

- `logging.request_log.enabled = true` does nothing unless `logging.dir` is also set
- For streaming requests, logged latency is time-to-first-byte, not full stream duration

## Web UI

The web app runs as a separate process beside the Rust gateway and provides:

- `/playground` for interactive conversion
- `/logs` for request log inspection
- `/usage` for token and latency views
- `/status` for health and disk status
- `/config` for editing `config.toml`

Start it after the Rust server is running:

```bash
SERVER_URL=http://127.0.0.1:8080 \
LOG_DIR=../../logs \
CONFIG_PATH=../../config.toml \
  pnpm --filter @any-converter/web dev
```

By default it opens on `http://localhost:3000`.

## Desktop app

The Tauri desktop app provides a local control plane for:

- Provider management
- Model route management
- Embedded server start, stop, and restart
- Local logs and usage views
- Conversion playground

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

### Web UI shows no logs or usage

Check:

- `LOG_DIR` points to the same directory used by `logging.dir`
- the server has created `any-converter.sqlite3` or JSONL files there

## More documentation

- Full config example: [config.example.toml](./config.example.toml)
- Build and run details: [docs/build.md](./docs/build.md)
- Architecture and data flow: [docs/architecture.md](./docs/architecture.md)
- Chinese README: [README-zh.md](./README-zh.md)

## License

MIT
