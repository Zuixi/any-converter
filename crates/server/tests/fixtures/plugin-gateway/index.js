// Gateway fixture used by `plugin_gateway_e2e.rs` (P6.2/P6.3).
//
// OpenCode-shaped plugin whose `auth.fetch` hook is hermetic: it never
// touches the network, it answers per URL suffix, and it persists every
// request it saw into the shared auth ledger under `__gw_seen` (as a
// `{type: "api"}` entry whose `key` holds the JSON array) so the Rust test
// can read the exact url/body/headers the host forwarded through
// `PluginAuthStore`.

const CHAT_OK = {
  id: "chatcmpl-gw",
  object: "chat.completion",
  created: 0,
  model: "gw-chat",
  choices: [
    {
      index: 0,
      message: { role: "assistant", content: "hi from gw chat" },
      finish_reason: "stop",
    },
  ],
  usage: { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 },
};

const CLAUDE_OK = {
  id: "msg_gw",
  type: "message",
  role: "assistant",
  model: "gw-claude",
  content: [{ type: "text", text: "hi from gw claude" }],
  stop_reason: "end_turn",
  stop_sequence: null,
  usage: { input_tokens: 1, output_tokens: 2 },
};

const RESPONSES_OK = {
  id: "resp_gw",
  object: "response",
  created_at: 0,
  status: "completed",
  model: "gw-responses",
  output: [
    {
      type: "message",
      id: "msg_resp_1",
      status: "completed",
      role: "assistant",
      content: [
        { type: "output_text", text: "hi from gw responses", annotations: [] },
      ],
    },
  ],
  usage: { input_tokens: 1, output_tokens: 2, total_tokens: 3 },
};

// SSE answers for `stream: true` requests. The plugin transport has no
// incremental streaming: this complete body is what the Rust side buffers
// and replays block by block.
const CHAT_SSE = [
  'data: {"id":"chatcmpl-gw","object":"chat.completion.chunk","created":0,"model":"gw-chat","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}',
  "",
  'data: {"id":"chatcmpl-gw","object":"chat.completion.chunk","created":0,"model":"gw-chat","choices":[{"index":0,"delta":{"content":"hi from gw chat"},"finish_reason":null}]}',
  "",
  'data: {"id":"chatcmpl-gw","object":"chat.completion.chunk","created":0,"model":"gw-chat","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
  "",
  "data: [DONE]",
  "",
].join("\n");

const CLAUDE_SSE = [
  'event: message_start',
  'data: {"type":"message_start","message":{"id":"msg_gw","type":"message","role":"assistant","model":"gw-claude","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}',
  "",
  "event: content_block_delta",
  'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi from gw claude"}}',
  "",
  "event: message_delta",
  'data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}',
  "",
  "event: message_stop",
  'data: {"type":"message_stop"}',
  "",
  "data: [DONE]",
  "",
].join("\n");

export default async ({ client, directory, $ }) => {
  void directory;
  void $;

  return {
    auth: {
      provider: "gw",
      name: "GW fixture",
      methods: [
        {
          type: "api",
          label: "API key",
        },
        {
          type: "oauth",
          label: "Sign in",
          authorize: async () => ({
            url: "https://gw.example.test/auth",
            instructions: "paste the code",
            method: "code",
          }),
          callback: async () => ({
            type: "success",
            refresh: "gw-refresh",
            access: "gw-access",
            expires: Date.now() + 3600000,
          }),
        },
      ],
      // Loader: the base URL the host must send requests to. Accounts whose
      // stored credential carries `metadata.relative` get a hostless base —
      // the same-host guard on the Rust side must reject that (e2e test 6).
      loader: async (getAuth, info) => {
        void info;
        const stored = await getAuth();
        if (stored?.metadata?.relative === true) {
          return { baseURL: "/relative-only" };
        }
        if (stored?.metadata?.versioned === true) {
          // A base that already carries the version, as @ai-sdk/anthropic's
          // npmBase does. The host must not append a second /v1.
          return { baseURL: "https://gw.example.test/v1" };
        }
        return { baseURL: "https://gw.example.test" };
      },
      models: async () => [
        {
          id: "gw-chat",
          name: "GW Chat",
          api: { npm: "@ai-sdk/openai-compatible" },
        },
        {
          id: "gw-claude",
          name: "GW Claude",
          api: { npm: "@ai-sdk/anthropic" },
        },
        {
          id: "gw-responses",
          name: "GW Responses",
          api: { npm: "@ai-sdk/openai" },
        },
      ],
      // Hermetic fetch: record the request, then answer per URL suffix.
      fetch: async (req) => {
        globalThis.__seen = globalThis.__seen ?? [];
        globalThis.__seen.push({
          url: req.url,
          method: req.method,
          body: req.body,
          headers: req.headers,
          provider: req.provider,
          account: req.account,
          model: req.model,
          npm: req.npm,
        });
        // Persist the ledger into the shared auth file so the Rust test can
        // read it back through `PluginAuthStore` (`{type:"api"}` shape keeps
        // the entry parseable by the Rust auth model).
        await client.auth.set({
          path: { id: "__gw_seen" },
          body: { type: "api", key: JSON.stringify(globalThis.__seen) },
        });

        // Failover trigger (e2e test 8): a marked message fails the plugin.
        if ((req.body ?? "").includes("TRIGGER_FAIL_500")) {
          return {
            status: 500,
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              error: { message: "plugin-side failure", type: "server_error" },
            }),
          };
        }

        const stream = (req.body ?? "").includes('"stream":true');
        const url = req.url;
        if (url.includes("/chat/completions")) {
          return stream
            ? {
                status: 200,
                headers: { "content-type": "text/event-stream" },
                body: CHAT_SSE,
              }
            : {
                status: 200,
                headers: { "content-type": "application/json" },
                body: JSON.stringify(CHAT_OK),
              };
        }
        if (url.includes("/messages")) {
          return stream
            ? {
                status: 200,
                headers: { "content-type": "text/event-stream" },
                body: CLAUDE_SSE,
              }
            : {
                status: 200,
                headers: { "content-type": "application/json" },
                body: JSON.stringify(CLAUDE_OK),
              };
        }
        if (url.includes("/responses")) {
          return {
            status: 200,
            headers: { "content-type": "application/json" },
            body: JSON.stringify(RESPONSES_OK),
          };
        }
        return {
          status: 404,
          headers: { "content-type": "text/plain" },
          body: `gw fixture: unknown url ${url}`,
        };
      },
    },
  };
};
