import type { ApiClient } from "@any-converter/core";
import { useI18n } from "@any-converter/core";
import { useCallback } from "react";

import type { ProxyErrorShape } from "./proxy-error";
import { requestServer } from "./transport";

/**
 * The single façade for the server's management API.
 *
 * Every management call (accounts, sign-ins, agent wiring) goes through the
 * typed methods below. Pages never assemble URLs, never `fetch` loopback
 * directly, and never re-declare these payload shapes. `requestServer` is
 * the transport: Tauri IPC in the desktop webview (the Rust side dials the
 * embedded server), same-origin fetch with the session cookie in the
 * browser.
 *
 * Adding a management operation = adding one method here. The HTTP path
 * lives in exactly one file.
 */

/** `GET /v1/accounts` entry (tokens are never part of the payload). */
export interface AccountEntry {
  id: string;
  kind: string;
  provider: string;
  account: string;
  source?: "builtin" | "plugin";
}

/** One model `GET /v1/accounts/{id}/models` discovered for an account. */
export interface AccountModel {
  id: string;
  name: string;
  source: string;
}

/** `GET /v1/accounts/{id}/models`; discovery failures use `error`, not HTTP 500. */
export interface AccountModels {
  account: string;
  provider: string;
  models: AccountModel[];
  source: string;
  bound?: boolean;
  error: string | null;
}

/** `SignInTask` snapshot (`POST/GET /v1/accounts/signin*`). */
export interface SignInTask {
  id: string;
  provider: string;
  state: string;
  url: string;
  code: string;
  instructions: string;
  paste_callback: boolean;
  paste_code: boolean;
  user: string;
  plan: string;
  error: string;
}

/** One allowance window of `GET /v1/accounts/quotas`. */
export interface QuotaWindow {
  name: string;
  used: number;
  resets?: string;
}

export interface QuotaEntry {
  provider: string;
  account: string;
  windows: QuotaWindow[];
}

/** `GET /v1/accounts/plugins`. */
export interface PluginEntry {
  spec: string;
  providers: { id: string; name: string; methods: { type: string; label: string }[] }[];
  error: string;
}

/** `POST /v1/accounts/plugins/install` — what the installed package serves. */
export interface InstalledPlugin {
  spec: string;
  providers: PluginEntry["providers"];
  error: string;
}

export type AgentStatus = "connected" | "drifted" | "disconnected" | "absent";

export interface AgentSummary {
  id: string;
  status: AgentStatus;
  model?: string;
  key_id?: string;
}

export interface ConnectReport {
  agent: string;
  status: string;
  files_written?: string[];
}

export type ConfigFormat = "openai_chat" | "openai_responses" | "claude" | "gemini";

/**
 * How a provider's key is sent upstream. `auto` uses the format's default
 * (bearer for OpenAI, x-api-key + version for Claude, x-goog-api-key for
 * Gemini); the other values override it for vendors that differ from the
 * wire format, such as Anthropic-compatible gateways that want Bearer.
 */
export type AuthSchemeOption =
  | "auto"
  | "bearer"
  | "api_key_header"
  | "x_api_key"
  | "google_api_key"
  | "anthropic"
  | "none";

export interface AdminConfigView {
  revision: string;
  restart_required: boolean;
  server: {
    host: string;
    port: number;
    proxy_configured: boolean;
    inference_key_configured: boolean;
    management_key_configured: boolean;
  };
  providers: Array<{
    name: string;
    format: ConfigFormat;
    base_url: string;
    api_key_configured: boolean;
    account?: string | null;
    accounts: string[];
    model_map: Record<string, string>;
    auth_scheme: AuthSchemeOption;
    auth_scheme_effective: AuthSchemeOption;
    proxy_configured: boolean;
    plugin: boolean;
  }>;
  model_routes: Array<{
    pattern: string;
    providers: string[];
    upstream_model?: string | null;
    strategy: "priority" | "round_robin";
  }>;
  logging: {
    dir?: string | null;
    max_disk_mb: number;
    request_log_enabled: boolean;
  };
}

export interface ConfigPatch {
  server?: { host?: string; port?: number; proxy?: string | null };
  logging?: {
    dir?: string | null;
    max_disk_mb?: number;
    request_log_enabled?: boolean;
  };
  upsert_providers?: Array<{
    name: string;
    format?: ConfigFormat;
    base_url?: string;
    api_key_ref?: string;
    model_map?: Record<string, string>;
    account?: string | null;
    accounts?: string[];
    auth_scheme?: AuthSchemeOption;
    proxy?: string | null;
  }>;
  remove_providers?: string[];
  upsert_model_routes?: Array<{
    pattern: string;
    providers: string[];
    upstream_model?: string | null;
    strategy?: "priority" | "round_robin";
  }>;
  remove_model_routes?: string[];
}

export type DiscoveryStatus = "ok" | "unsupported" | "error";

export type DiscoveryErrorKind =
  | "missing_api_key"
  | "blocked_address"
  | "invalid_base_url"
  | "invalid_credentials"
  | "endpoint_not_found"
  | "unsupported_format"
  | "timeout"
  | "network"
  | "rate_limited"
  | "malformed_response"
  | "response_too_large"
  | "http_error";

export interface DiscoveredModel {
  id: string;
  display_name?: string | null;
  capabilities: string[];
}

export interface DiscoveryError {
  kind: DiscoveryErrorKind;
  message: string;
  hint?: string | null;
}

/** `POST /v1/admin/providers/discover` and `GET .../{name}/models`. */
export interface DiscoveryReport {
  status: DiscoveryStatus;
  source: string;
  checked_at_unix: number;
  latency_ms: number;
  models: DiscoveredModel[];
  count: number;
  error?: DiscoveryError | null;
}

export interface DiscoverProviderRequest {
  /** A saved provider name; omit to test unsaved settings. */
  name?: string;
  format?: ConfigFormat;
  base_url?: string;
  /** Never echoed back by the server; used only for this one request. */
  api_key?: string;
  /** Sign the probe like generation will; omit for the format default. */
  auth_scheme?: AuthSchemeOption;
  refresh?: boolean;
}

/** `GET /v1/admin/catalog` — what is offered, exposed, and routable. */
export interface CatalogModel {
  id: string;
  provider: string;
  upstream: string;
  exposed: boolean;
  routed: boolean;
  source: "configured" | "upstream";
  capabilities: string[];
}

export interface CatalogProvider {
  name: string;
  format: ConfigFormat;
  base_url: string;
  plugin: boolean;
  account_based: boolean;
  exposed_count: number;
  discovered_count: number;
  checked_at_unix?: number | null;
  status?: DiscoveryStatus | null;
  error?: DiscoveryError | null;
}

export interface CatalogView {
  revision: string;
  providers: CatalogProvider[];
  models: CatalogModel[];
}

export interface SecretMutationResult {
  revision: string;
  reference: string;
  restart_required: boolean;
}

export interface ServerControlStatus {
  run_state: string;
  config_revision: string;
  restart_required: boolean;
  management_key_configured: boolean;
  ready_address?: string | null;
  started_at_unix?: number | null;
}

/** One management call over the `requestServer` transport. */
async function call<T>(
  path: string,
  method: "GET" | "POST" | "PATCH" | "PUT" | "DELETE" = "GET",
  body?: string,
  ifMatch?: string,
): Promise<T> {
  return requestServer<T>({ method, path, body, ifMatch });
}

export const management = {
  getConfig(): Promise<AdminConfigView> {
    return call("/v1/admin/config");
  },

  patchConfig(ifMatch: string, patch: ConfigPatch): Promise<AdminConfigView> {
    return call("/v1/admin/config", "PATCH", JSON.stringify(patch), ifMatch);
  },

  getServerStatus(): Promise<ServerControlStatus> {
    return call("/v1/admin/server/status");
  },

  /** Ask an upstream which models it offers; read-only, no If-Match. */
  discoverProvider(body: DiscoverProviderRequest): Promise<DiscoveryReport> {
    return call("/v1/admin/providers/discover", "POST", JSON.stringify(body));
  },

  getProviderModels(name: string, refresh = false): Promise<DiscoveryReport> {
    const query = refresh ? "?refresh=1" : "";
    return call(`/v1/admin/providers/${encodeURIComponent(name)}/models${query}`);
  },

  getCatalog(): Promise<CatalogView> {
    return call("/v1/admin/catalog");
  },

  putProviderSecret(ifMatch: string, provider: string, apiKey: string): Promise<SecretMutationResult> {
    return call(
      `/v1/admin/secrets/provider/${encodeURIComponent(provider)}`,
      "PUT",
      JSON.stringify({ api_key: apiKey }),
      ifMatch,
    );
  },

  deleteProviderSecret(ifMatch: string, provider: string): Promise<SecretMutationResult> {
    return call(`/v1/admin/secrets/provider/${encodeURIComponent(provider)}`, "DELETE", undefined, ifMatch);
  },

  putInferenceSecret(ifMatch: string, apiKey: string): Promise<SecretMutationResult> {
    return call("/v1/admin/secrets/inference", "PUT", JSON.stringify({ api_key: apiKey }), ifMatch);
  },

  /** Accounts, quota windows and plugins in one refresh. */
  async accountsOverview(): Promise<{
    accounts: AccountEntry[];
    quotas: QuotaEntry[];
    plugins: PluginEntry[];
  }> {
    const [accounts, quotas, plugins] = await Promise.all([
      call<{ accounts: AccountEntry[] }>("/v1/accounts"),
      call<{ accounts: QuotaEntry[] }>("/v1/accounts/quotas"),
      call<{ plugins: PluginEntry[] }>("/v1/accounts/plugins").catch(() => ({ plugins: [] as PluginEntry[] })),
    ]);
    return {
      accounts: accounts.accounts ?? [],
      quotas: quotas.accounts ?? [],
      plugins: plugins.plugins ?? [],
    };
  },

  startSignIn(payload: { provider: string; spec?: string }): Promise<SignInTask> {
    return call("/v1/accounts/signin", "POST", JSON.stringify(payload));
  },

  pollSignIn(id: string): Promise<SignInTask> {
    return call(`/v1/accounts/signin/${encodeURIComponent(id)}`);
  },

  submitSignIn(id: string, input: string): Promise<SignInTask> {
    return call(`/v1/accounts/signin/${encodeURIComponent(id)}/submit`, "POST", JSON.stringify({ input }));
  },

  cancelSignIn(id: string): Promise<void> {
    return call(`/v1/accounts/signin/${encodeURIComponent(id)}/cancel`, "POST");
  },

  accountModels(id: string): Promise<AccountModels> {
    return call(`/v1/accounts/${encodeURIComponent(id)}/models`);
  },

  saveAccountOrder(provider: string, accountIds: string[]): Promise<void> {
    return call("/v1/accounts/order", "POST", JSON.stringify({ provider, accounts: accountIds }));
  },

  adoptAccount(id: string): Promise<void> {
    return call(`/v1/accounts/${encodeURIComponent(id)}/adopt`, "POST");
  },

  releaseAccount(id: string): Promise<void> {
    return call(`/v1/accounts/${encodeURIComponent(id)}/release`, "POST");
  },

  removeAccount(id: string): Promise<{ outcome: string }> {
    return call(`/v1/accounts/${encodeURIComponent(id)}/remove`, "POST", "{}");
  },

  /** npm-install a plugin package and report what it serves (P6.3). */
  installPlugin(spec: string): Promise<InstalledPlugin> {
    return call("/v1/accounts/plugins/install", "POST", JSON.stringify({ spec }));
  },

  /** Remove a plugin package from the plugins dir. */
  uninstallPlugin(spec: string): Promise<void> {
    return call("/v1/accounts/plugins/uninstall", "POST", JSON.stringify({ spec }));
  },

  /** Agents plus the configured model catalog for the default-model picker. */
  async agentsWithModels(): Promise<{ agents: AgentSummary[]; models: string[] }> {
    const [list, config] = await Promise.all([
      call<{ agents: AgentSummary[] }>("/v1/agents"),
      call<AdminConfigView>("/v1/admin/config"),
    ]);
    const providerModels = config.providers.flatMap((provider) =>
      Object.keys(provider.model_map)
        .filter((model) => !model.includes("*") && !model.includes("?"))
        .map((model) => `${provider.name}/${model}`),
    );
    const routeModels = config.model_routes
      .filter((route) => !route.pattern.includes("*") && !route.pattern.includes("?"))
      .map((route) => `${route.providers[0]}/${route.pattern}`);
    const models = [...new Set([...providerModels, ...routeModels])];
    return {
      agents: list.agents ?? [],
      models,
    };
  },

  agentAction(
    id: string,
    action: "connect" | "disconnect" | "sync" | "key/rotate",
    body?: object,
  ): Promise<ConnectReport> {
    return call(`/v1/agents/${encodeURIComponent(id)}/${action}`, "POST", JSON.stringify(body ?? {}));
  },
};

/**
 * One typed management call. The transport dispatches: `server_request`
 * over Tauri IPC in the desktop (the Rust side dials the embedded server;
 * the webview never fetches it cross-origin), same-origin fetch with the
 * session cookie in the browser. Prefer the typed `management` façade for
 * every management operation — this hook is the transport it rides on.
 */
export function useServerRequest() {
  const { t } = useI18n();

  const request = useCallback(
    async <T>(path: string, init?: { method?: "GET" | "POST"; body?: string }): Promise<T> =>
      call<T>(path, init?.method ?? "GET", init?.body),
    [],
  );

  /** Render a transport rejection as localized user-facing text. */
  const describeError = useCallback(
    (cause: unknown): string => {
      const shape = cause as ProxyErrorShape | undefined;
      if (!shape || typeof shape !== "object" || !("kind" in shape)) {
        return String(cause);
      }
      switch (shape.kind) {
        case "not_running":
          return t("server_proxy.not_running");
        case "key_unavailable":
          return shape.message ?? "Management key is unavailable";
        case "http":
          return shape.message ?? "HTTP error";
        case "bad_request":
          return shape.message ?? "Invalid request";
        default:
          return shape.message ?? t("server_proxy.not_running");
      }
    },
    [t],
  );

  return { request, describeError };
}

// Re-exported so existing imports keep working; the façade is the entrypoint.
export type { ApiClient };
