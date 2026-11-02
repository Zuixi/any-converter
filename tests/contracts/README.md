# Contract Fixtures

Sanitized **record-replay** fixtures for the client → gateway → upstream contract.

Each case is one recorded client conversation: the body the client sends, the body
the gateway must send upstream after conversion, the upstream answer, and the answer
the client must receive. `crates/server/tests/contract_replay.rs` replays every case
through the real router against an in-process fake upstream, so a change to any
converter, router path, model rewrite, or stream emitter that breaks a real AI client
fails CI without network access or API keys.

## Layout

```
tests/contracts/<tool>/<case>/
  meta.toml                      # required: describes the case
  client.request.json            # required: body the client sends
  upstream.request.json          # required: body the gateway must send upstream
  upstream.response.json         # non-stream: what the fake upstream answers
  upstream.response.sse          # stream: what the fake upstream answers
  expected.client.response.json  # non-stream: what the client must receive
  expected.client.response.sse   # stream: what the client must receive
```

`<tool>` is the real client the shape came from (`claude-code`, `codex`, …).
`<case>` is the scenario; it must equal `name` in `meta.toml`.

## `meta.toml` schema

| key | type | meaning |
|-----|------|---------|
| `name` | string | case name, must equal the directory name |
| `tool` | string | client tool that speaks `client_format` |
| `client_format` | string | wire format the client uses: `claude`, `openai_chat`, `openai_responses`, `gemini` (aliases: `anthropic`, `openai`, `responses`, `google`) |
| `upstream_format` | string | wire format of the fake upstream provider, same alias set |
| `path` | string | client path the gateway is called on, e.g. `/v1/messages` |
| `method` | string | must be `POST` (the only replayed method today) |
| `mode` | string | `identity` (client format == upstream format) or `convert` |
| `stream` | bool | `true` reads/writes the `.sse` files instead of `.json` |
| `normalize` | array of strings | dot-paths whose values legitimately change run to run; both sides are replaced with `"<n>"` before comparing |

`normalize` examples: `id`, `created_at`, `message.id`. Array indices are not part of
the path, so `output.id` normalizes every item's `id`.

The fake upstream answers on the endpoint the gateway derives from `upstream_format`
(`/v1/messages`, `/v1/chat/completions`, `/v1/responses`); the runner asserts the
gateway called exactly that path. The provider config keeps `model_map` empty, so the
upstream model equals the client model — fixtures that need a rewrite must record the
rewritten name in `upstream.request.json`.

Stream cases compare the ordered `(event, data)` pairs, so a reordering, a lost event,
a changed text delta, or a missing terminal event (`message_stop` / `response.completed`)
all fail.

## How to run

```bash
cargo test -p any-converter-server --test contract_replay
```

Add `-- --nocapture` to see the full body dump of a failing case. Failures name the
fixture and diff the mismatching top-level keys.

## Recording a new case

1. Send the real client request through a local gateway with the request log on
   (`logging.request_log.enabled = true`), or simply `curl` the gateway:

   ```bash
   curl -sS http://127.0.0.1:8080/v1/messages \
     -H 'content-type: application/json' \
     -H 'x-api-key: <redacted>' \
     -d @client.request.json -o expected.client.response.json
   ```

   The request log gives you both halves of the exchange at once
   (client body, upstream request body, upstream response, converted response):
   `logging.dir` → `<dir>/requests/*.json`.
2. Save the client body as `client.request.json` and the body the gateway forwarded
   upstream as `upstream.request.json`. For a stream case save the raw SSE as
   `upstream.response.sse` and `expected.client.response.sse`.
3. Write `meta.toml`, listing every run-to-run field in `normalize`.
4. Run the replay test until green.

### Sanitize checklist

Recordings come from real traffic and must be scrubbed before committing:

- [ ] `authorization`, `x-api-key`, `x-goog-api-key`, `api-key`, `cookie` headers
      removed or replaced with a placeholder.
- [ ] No raw key material anywhere — **CI greps this tree for anything shaped like an
      API key (`sk-` followed by 20+ alphanumerics, and the vendor-specific prefixes)**
      and fails the build**. Use `<redacted>` or `contract-fixture-key`.
- [ ] Emails, phone numbers, names, account/org ids replaced with stable fakes.
- [ ] Absolute paths (`/Users/<name>/…`, `/home/<name>/…`) and host names replaced.
- [ ] `file_id` / `container_id` / signed URLs replaced — they leak tenants and expire.
- [ ] Large base64 payloads (images, audio) replaced with a short dummy value.
- [ ] Re-run the replay test; a scrubbed value that the gateway echoes back needs to
      be scrubbed in *every* file of the case, including `expected.*`.

## Adding a new client tool

Drop a `tests/contracts/<tool>/<case>/` directory; nothing else to register. Keep at
least one case per (client format → upstream format) pair the project claims to
support, and prefer a real capture over a hand-written body.
