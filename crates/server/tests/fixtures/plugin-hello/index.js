// Minimal OpenCode-shaped plugin used by the `plugin-host.js` tests.
//
// It mirrors the real `@magpie-community/opencode-*` contract: the default
// export is an async factory that receives the injected OpenCode context and
// returns `{ auth: { provider, methods } }`. The host is expected to drive it
// through `authorize` / `callback`; credentials are persisted by the host via
// `client.auth.set`, not by the plugin itself.

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
    },
  };
};
