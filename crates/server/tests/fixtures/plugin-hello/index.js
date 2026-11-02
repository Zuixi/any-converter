// Minimal OpenCode-shaped plugin used by the `plugin-host.js` tests.
//
// It mirrors the real `@magpie-community/opencode-*` contract: the default
// export is an async factory that receives the injected OpenCode context and
// returns `{ auth: { provider, methods, loader, models, fetch } }`. The host
// is expected to drive it through `authorize` / `callback`; credentials are
// persisted by the host via `client.auth.set`, not by the plugin itself.

export default async ({ client, directory, $ }) => {
  if (!client || !client.auth || typeof client.auth.set !== "function") {
    throw new Error("plugin-hello: client.auth.set was not injected");
  }
  if (typeof directory !== "string" || directory.length === 0) {
    throw new Error("plugin-hello: directory was not injected");
  }
  // `$` is present in the OpenCode context shape; the host passes undefined.
  void $;

  return {
    auth: {
      provider: "hello",
      // Reported through init's `providers`; differs from the host's
      // built-in fallback info so tests can tell the two apart.
      name: "Hello from init",
      methods: [
        {
          type: "oauth",
          label: "Sign in",
          authorize: async (inputs) => {
            // `inputs.fail` lets the test drive a plugin-side throw.
            if (inputs && inputs.fail) {
              throw new Error("boom from plugin");
            }
            return {
              url: "https://example.test/auth",
              instructions: "paste the code",
              method: "code",
            };
          },
          callback: async (code) => {
            if (code === "deny") {
              return { type: "failed", error: "denied by provider" };
            }
            return {
              type: "success",
              refresh: "r1",
              access: "a1",
              expires: Date.now() + 3600000,
            };
          },
        },
      ],
      // Loader (magpie `auth.loader`): the host calls it once per account
      // and caches the result. The counter exposes single-invocation to the
      // tests (`apiKey` stays `k-1` on a second `load`). For accounts whose
      // stored credential is a plain api key the apiKey is omitted so the
      // host falls back to `stored.key` (magpie options()).
      loader: async (getAuth, info) => {
        globalThis.__loaderCalls = (globalThis.__loaderCalls ?? 0) + 1;
        void info; // provider metadata; the fixture does not need it
        const stored = await getAuth();
        const options = {
          baseURL: "https://hello.example.test",
          apiKey: `k-${globalThis.__loaderCalls}`,
          headers: { "X-Hello": "1" },
        };
        if (stored && stored.type === "api") delete options.apiKey;
        return options;
      },
      // Models: account "none" exercises the host's null → [] normalization.
      models: async (opts) => {
        if (opts?.account === "none") return null;
        return [
          { id: "hello-chat", name: "Hello Chat", npm: "@ai-sdk/openai-compatible" },
        ];
      },
      // A hermetic `fetch` hook so trait-reachability tests need no network:
      // it echoes the request identity back in the body.
      fetch: async (req) => ({
        status: 200,
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          url: req.url,
          provider: req.provider,
          account: req.account,
          model: req.model,
          npm: req.npm,
        }),
      }),
    },
  };
};
