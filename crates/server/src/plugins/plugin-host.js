#!/usr/bin/env node
// OpenCode-compatible plugin host.
//
// The Rust side (`host.rs`) keeps the ndjson JSON-RPC transport it already
// speaks with child processes; this file is the missing JS half: it loads
// *real* OpenCode plugin modules
//
//   export default async ({ client, directory, $ }) => ({
//     auth: { provider, methods: [{ type, label, authorize, callback, ... }] },
//   })
//
// injects an OpenCode-shaped `client`, and answers the host's RPC calls from
// the returned `auth` hooks. `client.auth.set/remove` write the same auth
// file the Rust `PluginAuthStore` reads, so both sides share one ledger.
//
// Wire format (one JSON object per line, exactly like the Rust transport):
//
//   host → here:  {"id":1,"method":"authorize","params":{...}}
//   here → host:  {"id":1,"result":{...}} | {"id":1,"error":{"message":"..."}}
//
// Lines without an `id` are events and are ignored. stdout carries protocol
// JSON only — every log line goes to stderr (console.log included, because
// plugins are imported into this process and may print at any time).

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { pathToFileURL } from "node:url";

// ---------------------------------------------------------------------------
// stdout hygiene: keep the protocol stream clean even if a plugin logs
// ---------------------------------------------------------------------------

const log = (...args) => {
  try {
    process.stderr.write(`[plugin-host] ${args.join(" ")}\n`);
  } catch {
    // stderr gone: nothing sensible left to do
  }
};

for (const name of ["log", "info", "debug", "warn", "trace"]) {
  console[name] = (...args) => log(...args);
}

process.on("uncaughtException", (err) => log("uncaught exception:", describe(err)));
process.on("unhandledRejection", (err) => log("unhandled rejection:", describe(err)));

/** Best-effort human description of a thrown value. */
function describe(err) {
  if (err instanceof Error) return err.stack || err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

/** Message of a thrown value, as sent in an `error` response. */
function errorMessage(err) {
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string") return err;
  return String(err);
}

// ---------------------------------------------------------------------------
// host state
// ---------------------------------------------------------------------------

const state = {
  /** Absolute path of the shared auth file (`<provider>:<account>` keys). */
  authPath: null,
  /** provider id → the plugin's `auth` hook object. */
  providers: new Map(),
  /** sign-in handle → { provider, account, auth } for `callback`. */
  sessions: new Map(),
  /** Monotonic sign-in counter. */
  seq: 0,
  /** accountKey → normalized loader options (`load` cache). */
  loaders: new Map(),
};

/** Plugins baked into argv by `default_launch` (fallback for `init`). */
const launchPlugins = parseArgvPlugins(process.argv.slice(2));

function parseArgvPlugins(argv) {
  const at = argv.indexOf("--plugins");
  if (at < 0 || at + 1 >= argv.length) return [];
  try {
    const parsed = JSON.parse(argv[at + 1]);
    return Array.isArray(parsed) ? parsed : [];
  } catch (err) {
    log("ignoring unparsable --plugins argument:", errorMessage(err));
    return [];
  }
}

// ---------------------------------------------------------------------------
// auth file: atomic 0600 writes, `.bak` refresh, `<path>.lock` mutual
// exclusion with the Rust PluginAuthStore
// ---------------------------------------------------------------------------

const LOCK_POLL_MS = 10;
const LOCK_STALE_MS = 60_000;
const LOCK_TIMEOUT_MS = 10_000;

/** Synchronous sleep (main thread is allowed to block on Atomics.wait). */
function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function ensureDir(dir) {
  if (!dir || dir === ".") return;
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
}

/**
 * Run `fn` while holding `<authPath>.lock`.
 *
 * Same lock file the Rust store uses (`O_CREAT | O_EXCL`, 0600, stale
 * takeover after 60s), so the two writers never interleave.
 */
function withAuthLock(fn) {
  const file = requireAuthPath();
  const lock = `${file}.lock`;
  ensureDir(path.dirname(file));
  const deadline = Date.now() + LOCK_TIMEOUT_MS;
  for (;;) {
    try {
      fs.closeSync(fs.openSync(lock, "wx", 0o600));
      break;
    } catch (err) {
      if (err.code !== "EEXIST") throw err;
      let stale = true;
      try {
        stale = Date.now() - fs.statSync(lock).mtimeMs > LOCK_STALE_MS;
      } catch {
        stale = true; // vanished between calls: retry
      }
      if (stale) {
        try {
          fs.unlinkSync(lock);
        } catch {
          // another contender got it first
        }
        continue;
      }
      if (Date.now() > deadline) {
        throw new Error("timed out waiting for the auth file lock");
      }
      sleepSync(LOCK_POLL_MS);
    }
  }
  try {
    return fn();
  } finally {
    try {
      fs.unlinkSync(lock);
    } catch {
      // release is best effort; the lock goes stale on its own
    }
  }
}

function requireAuthPath() {
  if (!state.authPath) throw new Error("authPath is not initialized");
  return state.authPath;
}

/**
 * Read and parse the auth file.
 *
 * A missing file is a fresh (empty) store. An unparsable file is *treated*
 * as empty but never silently overwritten: `raw` carries the original bytes
 * so the writer can quarantine them first.
 */
function readAuthFile(file) {
  let raw;
  try {
    raw = fs.readFileSync(file, "utf8");
  } catch (err) {
    if (err.code === "ENOENT") return { map: {}, corrupt: false };
    throw err;
  }
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { map: parsed, corrupt: false };
    }
  } catch {
    // fall through: corrupt
  }
  return { map: {}, corrupt: true, raw };
}

/** Write the whole map atomically: 0600 temp file → fsync → rename. */
function writeAuthFile(file, map) {
  ensureDir(path.dirname(file));
  const tmp = `${file}.tmp-${process.pid}-${Date.now()}`;
  const payload = `${JSON.stringify(map, null, 2)}\n`;
  const fd = fs.openSync(tmp, "w", 0o600);
  try {
    fs.writeSync(fd, payload);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
  try {
    fs.chmodSync(tmp, 0o600);
  } catch {
    // non-fatal: the file was created 0600 on POSIX anyway
  }
  // Last-good backup of the committed state, taken before the rename
  // replaces it (`.bak` always mirrors the previous contents).
  if (fs.existsSync(file)) {
    try {
      fs.copyFileSync(file, `${file}.bak`);
    } catch (err) {
      log("failed to refresh auth backup:", errorMessage(err));
    }
  }
  fs.renameSync(tmp, file);
  try {
    fs.chmodSync(file, 0o600);
  } catch {
    // non-fatal
  }
}

/**
 * Read-modify-write the auth file under the lock.
 *
 * `fn` mutates the parsed map in place; whatever it returns is passed back.
 */
function updateAuth(fn) {
  return withAuthLock(() => {
    const file = requireAuthPath();
    const { map, corrupt, raw } = readAuthFile(file);
    const out = fn(map);
    if (corrupt && raw !== undefined) {
      // Preserve the unreadable file instead of destroying it.
      const bad = `${file}.bad-${Date.now()}`;
      try {
        fs.writeFileSync(bad, raw, { mode: 0o600 });
        log(`quarantined unparsable auth file to ${bad}`);
      } catch (err) {
        log("failed to quarantine unparsable auth file:", errorMessage(err));
      }
    }
    writeAuthFile(file, map);
    return out;
  });
}

/** Current auth entries (`{}` when the file is missing or unreadable). */
function readAuth() {
  return readAuthFile(requireAuthPath()).map;
}

/** Deep copy of the stored auth entry for `key` (`{}` when absent). */
function authCopy(key) {
  const stored = readAuth()[key];
  return stored === undefined ? {} : JSON.parse(JSON.stringify(stored));
}

/**
 * Auth file key for a provider/account pair.
 *
 * OpenCode keys entries by provider; magpie (and the Rust `PluginAuthStore`)
 * key them `<provider>:<account>` so multiple accounts can coexist. We use
 * the latter, with a bare provider key when no account is given.
 */
function accountKey(provider, account) {
  return account ? `${provider}:${account}` : provider;
}

// ---------------------------------------------------------------------------
// OpenCode SDK subset: `client.auth.set` / `client.auth.remove`
// ---------------------------------------------------------------------------

/** Replace the entry under `id` with `body` (whole object, no merging). */
function setAuthEntry(id, body) {
  if (!id) throw new Error("auth entry needs an id");
  updateAuth((map) => {
    map[id] = body;
  });
  return true;
}

/** Delete the entry under `id` (missing keys are fine). */
function removeAuthEntry(id) {
  if (!id) throw new Error("auth entry needs an id");
  updateAuth((map) => {
    delete map[id];
  });
  return true;
}

/** Delete every entry belonging to `provider` (bare and `<provider>:*`). */
function removeProviderEntries(provider) {
  updateAuth((map) => {
    for (const key of Object.keys(map)) {
      if (key === provider || key.startsWith(`${provider}:`)) delete map[key];
    }
  });
  return true;
}

/**
 * The `client` handed to plugin factories.
 *
 * Mirrors magpie host.js `makeClient`: `client.auth.set({path:{id}, body})`
 * persists `body` under `id`, `client.auth.remove({path:{id}})` drops it.
 * Real OpenCode SDK calls that plugins use beyond auth are out of scope; the
 * `auth` hook is where request signing lives.
 */
const client = {
  auth: {
    set: async ({ path: target, body } = {}) =>
      setAuthEntry(target?.id, body),
    remove: async ({ path: target } = {}) => removeAuthEntry(target?.id),
  },
};

// ---------------------------------------------------------------------------
// plugin loading
// ---------------------------------------------------------------------------

/** Resolve the on-disk directory of a plugin spec. */
function resolveSpecDir(spec, target) {
  if (!spec) throw new Error("plugin spec is empty");
  const base = target ? path.resolve(target) : process.cwd();
  if (path.isAbsolute(spec)) return spec;
  if (spec.startsWith(".")) return path.resolve(base, spec);
  // Bare package name: walk up looking for node_modules/<spec>.
  let dir = base;
  for (;;) {
    const candidate = path.join(dir, "node_modules", spec);
    if (fs.existsSync(candidate)) return candidate;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  throw new Error(`cannot resolve plugin "${spec}" from ${base}`);
}

/** First string found in a `package.json` `exports` value. */
function pickExport(value, depth = 0) {
  if (depth > 4 || value === null || value === undefined) return undefined;
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = pickExport(item, depth + 1);
      if (found) return found;
    }
    return undefined;
  }
  if (typeof value !== "object") return undefined;
  // `{".": ...}` wraps the root export; anything else is a condition map.
  if (value["."] !== undefined) return pickExport(value["."], depth + 1);
  for (const condition of ["import", "module", "node", "default", "require"]) {
    if (value[condition] !== undefined) {
      const found = pickExport(value[condition], depth + 1);
      if (found) return found;
    }
  }
  return undefined;
}

/**
 * Entry file for a plugin directory: `package.json` `exports` → `main` →
 * `index.js` / `index.mjs` / `index.cjs`.
 */
function resolveEntry(dir) {
  const candidates = [];
  const pkgPath = path.join(dir, "package.json");
  if (fs.existsSync(pkgPath)) {
    let pkg = {};
    try {
      pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8"));
    } catch (err) {
      log(`ignoring unparsable ${pkgPath}:`, errorMessage(err));
    }
    const fromExports = pickExport(pkg.exports);
    if (fromExports) candidates.push(fromExports);
    if (typeof pkg.main === "string") candidates.push(pkg.main);
  }
  candidates.push("index.js", "index.mjs", "index.cjs");
  for (const candidate of candidates) {
    const abs = path.resolve(dir, candidate);
    if (fs.existsSync(abs) && fs.statSync(abs).isFile()) return abs;
  }
  throw new Error(`no plugin entry point in ${dir}`);
}

/** Import a plugin module from `spec` (resolved against `target`). */
async function importPlugin(spec, target) {
  const dir = resolveSpecDir(spec, target);
  return import(pathToFileURL(resolveEntry(dir)).href);
}

// ---------------------------------------------------------------------------
// auth hook helpers
// ---------------------------------------------------------------------------

/**
 * The `auth` hooks for `provider`, or `undefined` when there is no match.
 *
 * A mismatch is tolerated when exactly one plugin is loaded (magpie-style
 * single-plugin setups where the provider id differs from the plugin name).
 */
function findAuth(provider) {
  const direct = state.providers.get(provider);
  if (direct) return direct;
  if (state.providers.size === 1) {
    const [only] = [...state.providers.values()];
    return only;
  }
  return undefined;
}

/** Like [`findAuth`], but a missing provider is an error. */
function providerAuth(provider) {
  const auth = findAuth(provider);
  if (!auth) throw new Error(`no plugin for provider "${provider}"`);
  return auth;
}

/** The auth method at index `method`, or the first matching type/label. */
function pickMethod(auth, method) {
  const methods = Array.isArray(auth.methods) ? auth.methods : [];
  const found =
    typeof method === "number"
      ? methods[method]
      : methods.find(
          (m) => m.type === method || m.label === method || m.id === method,
        );
  if (!found) throw new Error(`no method ${method}`);
  return found;
}

/** Credential body persisted for a `callback` result. */
function credentialOf(result) {
  const hasToken = result.key !== undefined &&
    result.refresh === undefined &&
    result.access === undefined;
  if (hasToken) return { type: "api", key: result.key };
  // `oauth`: keep refresh/access/expires plus any provider-specific extras.
  const { type: _type, provider: _provider, ...rest } = result;
  return { type: "oauth", ...rest };
}

/**
 * Declared provider metadata of a plugin (magpie `infoOf`): the second
 * argument of `auth.loader` and the `npm` fallback for `models`.
 *
 * OpenCode plugins declare the provider either as a plain id string or as
 * an object `{id, name, npm, url}`; the string form has no npm to offer.
 */
function providerInfo(provider) {
  const auth = findAuth(provider);
  const declared =
    auth?.provider && typeof auth.provider === "object" ? auth.provider : {};
  return {
    id: provider,
    name: auth?.name ?? declared.name ?? provider,
    npm: typeof declared.npm === "string" ? declared.npm : "",
    url: typeof declared.url === "string" ? declared.url : "",
  };
}

/**
 * The auth methods of a provider as reported by `init`:
 * `[{type, label, prompts}]` (magpie host.js `methodsOf`).
 */
function methodsOf(auth) {
  const methods = Array.isArray(auth.methods) ? auth.methods : [];
  return methods.map((method) => ({
    type: method?.type ?? "oauth",
    label: method?.label ?? "",
    prompts: (Array.isArray(method?.prompts) ? method.prompts : []).map((p) => {
      const prompt = {
        key: String(p?.key ?? ""),
        label: String(p?.label ?? ""),
        type: p?.type ?? "text",
      };
      if (prompt.type === "select") {
        prompt.options = Array.isArray(p?.options) ? p.options.map(String) : [];
      }
      return prompt;
    }),
  }));
}

/** First non-empty string among the arguments (else the last one). */
function firstString(...values) {
  for (const value of values) {
    if (typeof value === "string" && value) return value;
  }
  return "";
}

/** Non-negative integer from `m.<field>` or `m.limit.<field>`, else 0. */
function numberOf(...values) {
  for (const value of values) {
    if (typeof value === "number" && Number.isFinite(value) && value >= 0) {
      return Math.floor(value);
    }
  }
  return 0;
}

/** Drop cached loader options for one account (or the whole provider). */
function clearLoaderCache(provider, account) {
  if (!account) {
    for (const key of [...state.loaders.keys()]) {
      if (key === provider || key.startsWith(`${provider}:`)) {
        state.loaders.delete(key);
      }
    }
    return;
  }
  state.loaders.delete(accountKey(provider, account));
}

// ---------------------------------------------------------------------------
// RPC handlers (mirror magpie host.js `handlers`)
// ---------------------------------------------------------------------------

/**
 * `init {authPath, plugins}`: load every plugin, adopt its `auth` hooks, and
 * report one entry per spec (failures carry `error` instead of failing the
 * whole handshake).
 */
async function handleInit(params) {
  if (typeof params.authPath === "string" && params.authPath) {
    state.authPath = params.authPath;
  } else if (!state.authPath) {
    throw new Error("init requires authPath");
  }
  state.providers.clear();
  state.sessions.clear();
  state.loaders.clear();
  state.seq = 0;

  const specs =
    Array.isArray(params.plugins) && params.plugins.length > 0
      ? params.plugins
      : launchPlugins;

  const loaded = [];
  for (const raw of specs) {
    const entry = typeof raw === "string" ? { spec: raw, target: "" } : raw || {};
    const spec = String(entry.spec ?? "");
    const target = entry.target ? String(entry.target) : "";
    try {
      const mod = await importPlugin(spec, target);
      const factory =
        typeof mod.default === "function"
          ? mod.default
          : typeof mod === "function"
            ? mod
            : undefined;
      if (!factory) {
        throw new Error(`plugin "${spec}" has no default export function`);
      }
      const hooks = await factory({
        client,
        directory: process.cwd(),
        $: undefined,
      });
      const auth = hooks?.auth;
      if (!auth) {
        // Tool-only plugins are legitimate; they just answer no auth RPCs.
        log(`plugin "${spec}" exposes no auth hooks`);
        loaded.push({ spec });
        continue;
      }
      // `auth.provider` is a plain id string or an object
      // `{id, name, npm, url}`; the map key is always the id string.
      const providerId = String(
        auth.provider && typeof auth.provider === "object"
          ? auth.provider.id ?? ""
          : auth.provider ?? "",
      );
      if (!providerId) {
        throw new Error(`plugin "${spec}" auth hooks have no provider id`);
      }
      if (state.providers.has(providerId)) {
        log(`plugin "${spec}" replaces the auth hooks of ${providerId}`);
      }
      state.providers.set(providerId, auth);
      loaded.push({ spec });
      log(`loaded plugin "${spec}" (provider ${providerId})`);
    } catch (err) {
      log(`failed to load plugin "${spec}":`, describe(err));
      loaded.push({ spec, error: errorMessage(err) });
    }
  }
  return {
    plugins: loaded,
    providers: [...state.providers.entries()].map(([id, auth]) => ({
      id,
      name: auth.name ?? id,
      methods: methodsOf(auth),
    })),
  };
}

/** `authorize`: start an OAuth flow for `provider` / `account`. */
async function handleAuthorize(params) {
  const provider = String(params.provider ?? "");
  const account = String(params.account ?? "");
  const auth = providerAuth(provider);
  const method = pickMethod(auth, params.method);
  if (method.type !== "oauth") throw new Error("not an oauth method");
  if (typeof method.authorize !== "function") {
    throw new Error("method has no authorize()");
  }

  const started = (await method.authorize(params.inputs ?? {})) ?? {};
  const session = String(++state.seq);
  // Real OpenCode plugins return `{url, instructions, method, callback}` from
  // `authorize()` (the callback belongs to the flow); tolerate plugins that
  // keep `callback` on the method instead.
  state.sessions.set(session, { provider, account, auth: started, method });
  return {
    session,
    url: started.url ?? "",
    instructions: started.instructions ?? "",
    method: started.method ?? "code",
    opened: started.opened === true,
  };
}

/** `callback`: finish the flow and let the plugin's hooks decide storage. */
async function handleCallback(params) {
  const session = String(params.session ?? "");
  const pending = state.sessions.get(session);
  if (!pending) throw new Error("no such sign-in");

  const callback =
    typeof pending.auth.callback === "function"
      ? pending.auth.callback.bind(pending.auth)
      : typeof pending.method?.callback === "function"
        ? pending.method.callback.bind(pending.method)
        : undefined;
  if (!callback) throw new Error("authorize() returned no callback");

  const result = (await callback(params.code ?? "")) ?? {};
  if (result.type !== "success") {
    return {
      ok: false,
      error: result.error ?? "",
      provider: pending.provider,
      account: pending.account,
    };
  }

  const provider = result.provider ?? pending.provider;
  const key = accountKey(provider, pending.account);
  if (key !== accountKey(pending.provider, pending.account)) {
    removeAuthEntry(accountKey(pending.provider, pending.account));
  }
  setAuthEntry(key, credentialOf(result));
  state.sessions.delete(session);
  return { ok: true, provider, account: pending.account };
}

/** `apiKey`: store a manually supplied key under `provider:account`. */
async function handleApiKey(params) {
  const provider = String(params.provider ?? "");
  const account = String(params.account ?? "");
  const auth = providerAuth(provider);
  const method = pickMethod(auth, params.method);
  // Plugins may validate/normalize the inputs before the key is persisted.
  if (typeof method.authorize === "function") {
    await method.authorize(params.inputs ?? {});
  }
  setAuthEntry(accountKey(provider, account), {
    type: "api",
    key: String(params.key ?? ""),
    metadata: params.inputs ?? {},
  });
  // The stored credential changed: cached loader options are stale.
  clearLoaderCache(provider, account);
  return { ok: true, provider, account };
}

/** `signOut`: drop one account, or every account of the provider. */
async function handleSignOut(params) {
  const provider = String(params.provider ?? "");
  const account = String(params.account ?? "");
  if (account) removeAuthEntry(accountKey(provider, account));
  else removeProviderEntries(provider);
  // The stored credentials changed: cached loader options are stale.
  clearLoaderCache(provider, account);
  return null;
}

/** `usage`: plugin-provided usage/balance JSON, `null` when unavailable. */
async function handleUsage(params) {
  const auth = providerAuth(String(params.provider ?? ""));
  if (typeof auth.usage !== "function") return null;
  const value = await auth.usage({
    provider: params.provider,
    account: params.account,
    proxy: params.proxy,
  });
  return value ?? null;
}

/**
 * `load`: options the host needs to speak to an account, from the plugin's
 * `auth.loader` hook (magpie host.js `options()`).
 *
 * The loader runs at most once per `<provider>:<account>` and the result is
 * cached (dropped by `init`, `apiKey`, and `signOut`). It is called with
 * `(getAuth, providerInfo)`; when it returns no apiKey and the stored
 * credential is a plain api key, `stored.key` is the fallback. Without a
 * loader — or without a signed-in account — the reply is all defaults.
 */
async function handleLoad(params) {
  const provider = String(params.provider ?? "");
  const account = String(params.account ?? "");
  const key = accountKey(provider, account);
  if (state.loaders.has(key)) return state.loaders.get(key);

  const auth = findAuth(provider);
  const stored = readAuth()[key];
  let opts = {};
  if (auth && typeof auth.loader === "function" && stored !== undefined) {
    const info = providerInfo(provider);
    opts =
      (await auth.loader(
        async () => authCopy(key),
        JSON.parse(JSON.stringify(info)),
      )) ?? {};
    if (!opts || typeof opts !== "object") opts = {};
  }
  if (!opts.apiKey && stored?.type === "api") {
    opts = { ...opts, apiKey: stored.key };
  }
  const normalized = {
    baseURL: typeof opts.baseURL === "string" ? opts.baseURL : "",
    apiKey: typeof opts.apiKey === "string" ? opts.apiKey : "",
    headers:
      opts.headers && typeof opts.headers === "object" && !Array.isArray(opts.headers)
        ? opts.headers
        : {},
    fetch: typeof opts.fetch === "function",
  };
  state.loaders.set(key, normalized);
  return normalized;
}

/**
 * `models`: plugin-provided model list, normalized to the host shape
 * `{id, name, npm, url, apiId, context, output, reasoning, image, free}`.
 * A missing hook, `null`, or a non-array reply yields `[]`.
 */
async function handleModels(params) {
  const provider = String(params.provider ?? "");
  const auth = providerAuth(provider);
  let value = [];
  if (typeof auth.models === "function") {
    value = await auth.models({
      provider,
      account: params.account,
      proxy: params.proxy,
    });
  }
  if (!Array.isArray(value)) return [];
  const p = providerInfo(provider);
  return value
    .filter((m) => m && typeof m === "object" && m.id !== undefined && m.id !== null)
    .map((m) => {
      const api = m.api && typeof m.api === "object" ? m.api : {};
      const limit = m.limit && typeof m.limit === "object" ? m.limit : {};
      return {
        id: String(m.id),
        name: String(m.name ?? ""),
        // magpie: `m.api?.npm ?? p.npm ?? ""`, with the openai-compatible
        // package as the final default when the provider declares nothing.
        npm: firstString(api.npm, p.npm, "@ai-sdk/openai-compatible"),
        url: String(api.url ?? ""),
        apiId: String(api.id ?? m.id),
        context: numberOf(m.context, limit.context),
        output: numberOf(m.output, limit.output),
        reasoning: m.reasoning === true,
        image: m.image === true || m.attachment === true,
        free: m.free === true || m.cost === "free",
      };
    });
}

/**
 * `fetch`: proxy an HTTP request for the host.
 *
 * Placeholder implementation: when the plugin exposes no `auth.fetch` hook
 * the request goes out through Node's built-in `fetch` unchanged (unsigned).
 * Real plugins that sign requests with their own SDK override `auth.fetch`
 * and that result is returned verbatim.
 */
async function handleFetch(params) {
  const url = String(params.url ?? "");
  if (!url) throw new Error("fetch requires url");
  const method = String(params.method ?? "GET").toUpperCase();
  const headers =
    params.headers && typeof params.headers === "object" ? params.headers : {};
  const body =
    params.body === undefined || params.body === null
      ? undefined
      : String(params.body);

  const auth = findAuth(String(params.provider ?? ""));
  if (auth && typeof auth.fetch === "function") {
    return await auth.fetch({
      url,
      method,
      headers,
      body,
      provider: params.provider,
      account: params.account,
      model: params.model,
      npm: params.npm,
    });
  }

  // `fetch` rejects a body on GET/HEAD, and the host always sends a string
  // (possibly empty) body.
  const init = { method, headers };
  if (body !== undefined && body !== "" && method !== "GET" && method !== "HEAD") {
    init.body = body;
  }
  const response = await globalThis.fetch(url, init);
  const text = await response.text();
  const responseHeaders = {};
  response.headers.forEach((value, name) => {
    responseHeaders[name] = value;
  });
  return { status: response.status, headers: responseHeaders, body: text };
}

const handlers = {
  init: handleInit,
  authorize: handleAuthorize,
  callback: handleCallback,
  apiKey: handleApiKey,
  signOut: handleSignOut,
  usage: handleUsage,
  load: handleLoad,
  models: handleModels,
  fetch: handleFetch,
};

// ---------------------------------------------------------------------------
// stdio loop
// ---------------------------------------------------------------------------

function write(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

/** Answer one request line. Never throws: errors become `error` responses. */
async function dispatch(line) {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    log("ignoring malformed request line:", line.slice(0, 200));
    return;
  }
  if (!request || typeof request !== "object") return;
  // No id → event/notification: nothing to answer.
  if (request.id === undefined || request.id === null) return;

  const id = request.id;
  const method = String(request.method ?? "");
  const params =
    request.params && typeof request.params === "object" ? request.params : {};
  const handler = handlers[method];
  if (typeof handler !== "function") {
    write({ id, error: { message: `no method ${method}` } });
    return;
  }
  try {
    const result = await handler(params);
    write({ id, result: result === undefined ? null : result });
  } catch (err) {
    // A failing handler must never take the process down: the host keeps
    // using the same transport for the next call.
    log(`${method} failed:`, describe(err));
    write({ id, error: { message: errorMessage(err) } });
  }
}

const input = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});
// Requests are handled one at a time so concurrent auth writes stay
// serialized; the host awaits each reply anyway.
let chain = Promise.resolve();
input.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  chain = chain
    .then(() => dispatch(trimmed))
    .catch((err) => log("request loop error:", describe(err)));
});
input.on("close", () => {
  // stdin EOF: let the queued requests finish, then exit cleanly.
  chain.then(() => {
    process.exitCode = 0;
  });
});
