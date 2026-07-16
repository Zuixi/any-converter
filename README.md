# any-converter

A Rust-based LLM API format conversion tool that supports bidirectional conversion between major LLM provider APIs. Works as a CLI tool, streaming pipe, or HTTP proxy server.

## Supported Formats

| Format | Endpoint | Provider |
|--------|----------|----------|
| OpenAI Chat Completions | `/v1/chat/completions` | OpenAI, Azure, compatible providers |
| Claude Messages | `/v1/messages` | Anthropic |
| OpenAI Responses | `/v1/responses` | OpenAI |
| Google Gemini | `/v1beta/models/{model}:generateContent` | Google AI Studio |

## Documentation Map

New here? Follow the path:
1. **Quick start** → [Build Guide](docs/build.md)
2. **Understand the system** → [Onboarding](docs/onboarding.md) → [Architecture](docs/architecture.md)
3. **Work on a component** → Read the crate's `AGENTS.md`
4. **Avoid pitfalls** → [Known Gotchas](docs/memory/known-gotchas.md)

Coding agent? Start at [`AGENTS.md`](./AGENTS.md).

## Installation

```bash
cargo install --path crates/cli
```

## Usage

### CLI — Offline Conversion

```bash
# Convert OpenAI Chat request to Claude format
echo '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}' \
  | any-converter convert --from openai-chat --to claude

# Convert from file
any-converter convert --from claude --to gemini request.json

# Pipe streaming SSE conversion
curl -N https://api.openai.com/v1/chat/completions ... \
  | any-converter stream --from openai-chat --to claude
```

### HTTP Proxy Server

Start a server that lets any LLM CLI tool use any backend provider:

```bash
# With config file
any-converter serve --config config.example.toml

# Quick start (single provider)
any-converter serve --port 8080 \
  --provider openai --format openai-chat \
  --base-url https://api.openai.com \
  --upstream-key sk-proj-xxx
```

Then configure your LLM tool to use `http://localhost:8080` as the base URL:

```bash
# Claude Code → uses OpenAI backend
export ANTHROPIC_BASE_URL=http://localhost:8080

# Codex CLI → uses Claude backend  
export OPENAI_BASE_URL=http://localhost:8080
```

### Configuration

See [config.example.toml](config.example.toml) for a full example. Key sections:

```toml
[server]
host = "127.0.0.1"
port = 8080
api_key = "sk-my-key"  # optional client auth

[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[providers.model_map]
"claude-sonnet-4" = "gpt-4.1"
"*" = "gpt-4.1-mini"

# Optional overrides for OpenAI-compatible providers with non-standard transport
# [providers.endpoints]
# path = "/custom/chat/completions"
# [providers.auth]
# scheme = "api_key_header"

[[routes]]
client_format = "claude"
provider = "openai"
```

## Architecture

```
┌─────────────┐     ┌────────────────────┐     ┌─────────────┐
│ Client      │     │ Pairwise Converter │     │ Target      │
│ Format JSON │ ──▶ │ (direct mapping)   │ ──▶ │ Format JSON │
└─────────────┘     └────────────────────┘     └─────────────┘
```

The core uses **pairwise converters** — each (source, target) format pair has a dedicated converter that translates directly, ensuring:
- No data loss from intermediate normalization
- Each converter optimized for its specific pair
- Streaming uses lightweight canonical events as intermediate

## Development

```bash
cargo test --workspace          # Run all 156 tests
cargo build --release           # Build release binary
```

## License

MIT
