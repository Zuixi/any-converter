# Providers, accounts and plugins

How a request reaches an upstream, and the three kinds of upstream this
gateway can send to.

## Three kinds of upstream

| Kind         | What it is                                                       | Where its credential lives                                |
| ------------ | ---------------------------------------------------------------- | --------------------------------------------------------- |
| **Provider** | An endpoint with one or more API keys, in `config.toml`          | `providers.json`-style entry: `api_key`, or a `keys` list |
| **Account**  | A subscription signed in here (or already signed in by an agent) | `secrets.json`, mode `0600`                               |
| **Plugin**   | A provider an OpenCode plugin serves                             | The plugin's own auth store (`plugin-auth.json`)          |

An account **replaces** `api_key`: when a provider names one, the static key
is ignored entirely.

## The wire is per provider, and on a plugin it is per model

`ProviderConfig.api` names the API an upstream speaks — `chat`, `claude`,
`responses`, `gemini` or `code_assist`. It is a different axis from the
client's `Format`: a Claude client talking to a Chat upstream is converted
once, a Claude client talking to a Claude upstream is passed through
untouched (see [`build_plan`] and `convert_request`'s identity shortcut).

Two accounts override it because their vendor's API is not the public one:

| Account                     | API                                  | Endpoint                                                                        |
| --------------------------- | ------------------------------------ | ------------------------------------------------------------------------------- |
| `gemini:…`, `antigravity:…` | `code_assist`                        | `POST {base}/v1internal:generateContent`, body wrapped as `{"model","request"}` |
| `codex:…`                   | `responses`                          | ChatGPT's Codex backend                                                         |
| `chatgpt-api:…`             | `responses`                          | `api.openai.com/v1`                                                             |
| `copilot:<login>`           | `chat` or `responses`, **per model** | `api.githubcopilot.com`, after a session exchange                               |
| `claude:…`                  | `claude`                             | Anthropic's Messages API                                                        |

A plugin provider's API comes from each model's AI SDK package
(`UpstreamApi::from_npm`), which is why a plugin's `/v1/models` can list a
Claude model and a GPT model side by side.

## Signing in

`POST /v1/accounts/signin` starts a **task**, not a blocking call: it returns
a snapshot with the page to open (or the device code), and the window polls
`GET /v1/accounts/signin/{id}` until it settles. `POST …/submit` pastes a
callback address or a code; `POST …/cancel` abandons the flow and releases
its listener and temp state.

| Provider      | Flow                                                      | What the user does                                               |
| ------------- | --------------------------------------------------------- | ---------------------------------------------------------------- |
| `gemini`      | Google OAuth, PKCE, loopback `/oauth2callback`            | Approve in the browser, or paste the address it ended on         |
| `antigravity` | The same, on Antigravity's own client and daily host      | The same, after reading the suspension warning                   |
| `codex`       | ChatGPT OAuth, fixed callback on `127.0.0.1:1455`         | Approve; if the port is taken the flow goes paste-only           |
| `chatgpt-api` | Sign in with ChatGPT, which registers this app as it goes | Paste the whole address: it carries the code _and_ the client id |
| `copilot`     | GitHub device flow                                        | Type the code at github.com/login/device                         |
| `claude`      | Claude Code's own `claude auth login`, run here           | Approve, then paste the code or the address                      |
| `plugin:<id>` | The plugin's own authorize                                | Depends on the plugin: browser, pasted code, or an API key       |

Three rules hold across all of them, taken from magpie because each was
learned the hard way:

- **Tokens are refreshed before they expire** (5 minutes; 3 for ChatGPT API),
  and a refresh never happens twice for the same refresh token: a
  cross-process lock (`<secrets>.refresh.lock`) plus a re-read under it mean
  a rotated token is only ever spent once.
- **A 401 is classified**, not guessed: `token_expired` refreshes and retries
  once, `token_revoked`/`invalid_grant`/`refresh_token_reused` marks the
  account revoked and fails over, anything else is simply another provider's
  turn.
- **Sign-in, refresh and revocation leave through the same proxy the
  account's requests will** (`ProviderConfig.proxy`, else the server's).

## Signing in is not routing

A successful sign-in only writes a credential. It does not create a
provider, and it does not make any model routable. Routing still requires a
provider whose `account` or `accounts` list contains the **exact** account id,
plus a `model_routes` entry that resolves the requested model to that provider:

```toml
[[providers]]
name = "chatgpt-api"
format = "openai_responses"
base_url = "https://api.openai.com"
api_key = ""
account = "chatgpt-api:you@example.com"

[[model_routes]]
pattern = "*"
provider = "chatgpt-api"
```

A provider that merely shares the provider name (`chatgpt-api`, `codex`, …)
does not bind an account. The per-account model response reports this as
`"bound": false`, and the Accounts page shows a "Not bound to a provider"
notice instead of implying that requests already work.

## Per-account model discovery

`GET /v1/accounts/{id}/models` asks what one signed-in account can use:

| Account / provider          | Where the list comes from                                                                                                              |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `gemini:…`, `antigravity:…` | Code Assist project and experiment discovery; an ineligible or unavailable account returns an error instead of the global Gemini table |
| `chatgpt-api:…`             | `GET {base}/v1/models` with the account's access token                                                                                 |
| `copilot:…`                 | GitHub token → Copilot session → `GET {session.api_base}/models`                                                                       |
| `plugin:…`                  | The serving plugin's `models` hook                                                                                                     |
| Any exact-bound provider    | Literal `model_map` keys and literal `model_routes` patterns on that provider                                                          |

Wildcard patterns describe a family, not a model id, so they are not listed
as concrete models. Discovery results and configured aliases are merged and
deduplicated by model id; each row carries its own `source`.

Codex and Claude do not yet have verified dynamic model discovery here. A
bound provider still contributes its literal configured models; with none,
the answer says `source: "unsupported"` rather than inventing a catalog.

The response is:

```json
{
  "account": "chatgpt-api:you@example.com",
  "provider": "chatgpt-api",
  "models": [{ "id": "gpt-5", "name": "gpt-5", "source": "chatgpt-api" }],
  "source": "chatgpt-api",
  "bound": true,
  "error": null
}
```

A discovery failure is still HTTP 200 with `error` populated and any literal
configured models retained; only an unknown account is a 404. This separates
"the account exists but its upstream could not be asked" from "the server
failed", which is what the Accounts page renders as a warning plus Retry.

## Account pools

A provider may hold several accounts. Two layers, deliberately separate:

1. **The user's order** — `accounts order` / the Accounts page's "move up" —
   saved to `account-order.json`. It is authoritative and never rearranged.
2. **Allowance** — an account past its line (95%, on the fullest window that
   counts for the model) is moved **last**, not reordered.

Routing never waits on a quota read: a cached window decides the order, a
stale one is refreshed in the background, and an account with nothing known
counts as having room — so a cold start keeps the user's order. Only the
quotas endpoint (the user asking) reads for real, and it does so in parallel.

## Plugins

A plugin is an npm package that owns a provider: its `auth.loader` supplies
the base URL and signs requests, its `models` hook lists what it serves, and
its `fetch` hook sends the request. The host never mints an `Authorization`
header for a plugin provider.

```
config.toml:
  [[providers]]
  name = "cursor"
  plugin = { spec = "@magpie-community/opencode-cursor-auth" }
  account = "cursor:me@example.com"
```

`any-converter plugin install <spec>` puts the package under
`<config>/plugins`; `GET /v1/accounts/plugins` lists what is installed and
what each serves (a package that won't load is listed with why, never
silently dropped).

The plugin host is a child process (`plugin-host.js`) speaking one JSON
object per line over stdio. Starting it is single-flight, and a dropped host
kills its child — both were leaks before they were fixed.

**Known limit:** a plugin's reply arrives whole, so "streaming" through a
plugin is buffered and then replayed as SSE. It is not incremental and cannot
be cancelled mid-answer.

## Source map

| Concern                                    | File                                                                                   |
| ------------------------------------------ | -------------------------------------------------------------------------------------- |
| Account resolution, refresh, revoked marks | [`credentials/account.rs`](../../crates/server/src/credentials/account.rs)             |
| Gemini / Antigravity sign-in               | [`credentials/gemini.rs`](../../crates/server/src/credentials/gemini.rs)               |
| Codex (ChatGPT) sign-in                    | [`credentials/codex_signin.rs`](../../crates/server/src/credentials/codex_signin.rs)   |
| Sign in with ChatGPT                       | [`credentials/siwc_signin.rs`](../../crates/server/src/credentials/siwc_signin.rs)     |
| Copilot device flow and session exchange   | [`credentials/copilot.rs`](../../crates/server/src/credentials/copilot.rs)             |
| Claude, through Claude Code                | [`credentials/claude_signin.rs`](../../crates/server/src/credentials/claude_signin.rs) |
| Codex `auth.json` read and write-back      | [`credentials/codex_store.rs`](../../crates/server/src/credentials/codex_store.rs)     |
| Importing other tools' exports             | [`credentials/import.rs`](../../crates/server/src/credentials/import.rs)               |
| Task-based sign-in                         | [`signin_task.rs`](../../crates/server/src/signin_task.rs)                             |
| Pools and ordering                         | [`provider_pool.rs`](../../crates/server/src/provider_pool.rs)                         |
| Plugin protocol and host                   | [`plugins/`](../../crates/server/src/plugins/)                                         |
| Request path                               | [`handlers/chat/`](../../crates/server/src/handlers/chat/)                             |
