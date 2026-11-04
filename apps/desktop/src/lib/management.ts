import type { ApiClient } from "@any-converter/core";
import { useI18n } from "@any-converter/core";
import { useCallback } from "react";

import { api } from "./api";
import type { ProxyErrorShape } from "./proxy-error";

/**
 * The single façade for the server's management API.
 *
 * Every management call (accounts, sign-ins, agent wiring) goes through the
 * typed methods below. Pages never assemble URLs, never `fetch` the
 * embedded server directly (a browser origin cannot cross into loopback:
 * the CORS preflight the server never answers surfaces as
 * `TypeError: Failed to fetch`), and never re-declare these payload shapes.
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

/** One management call over the `server_request` Tauri command. */
async function call<T>(path: string, method: "GET" | "POST" = "GET", body?: string): Promise<T> {
  const value = await api.serverRequest({ method, path, body });
  return value as T;
}

export const management = {
  /** Accounts, quota windows and plugins in one refresh. */
  async accountsOverview(): Promise<{
    accounts: AccountEntry[];
    quotas: QuotaEntry[];
    plugins: PluginEntry[];
  }> {
    const [accounts, quotas, plugins] = await Promise.all([
      call<{ accounts: AccountEntry[] }>("/v1/accounts"),
      call<{ accounts: QuotaEntry[] }>("/v1/accounts/quotas"),
      call<{ plugins: PluginEntry[] }>("/v1/accounts/plugins").catch(
        () => ({ plugins: [] as PluginEntry[] }),
      ),
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
    return call(`/v1/accounts/signin/${id}`);
  },

  submitSignIn(id: string, input: string): Promise<SignInTask> {
    return call(`/v1/accounts/signin/${id}/submit`, "POST", JSON.stringify({ input }));
  },

  cancelSignIn(id: string): Promise<void> {
    return call(`/v1/accounts/signin/${id}/cancel`, "POST");
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

  /** Agents plus the model catalog (best-effort) for the default-model picker. */
  async agentsWithModels(): Promise<{ agents: AgentSummary[]; models: string[] }> {
    const [list, modelList] = await Promise.all([
      call<{ agents: AgentSummary[] }>("/v1/agents"),
      call<{ data?: { id?: string }[] } | { models?: { id?: string }[] }>("/v1/models").catch(
        () => ({ data: [] as { id?: string }[] }),
      ),
    ]);
    const raw =
      "data" in modelList
        ? (modelList.data ?? [])
        : ((modelList as { models?: { id?: string }[] }).models ?? []);
    return {
      agents: list.agents ?? [],
      models: raw.map((entry) => entry.id ?? "").filter(Boolean),
    };
  },

  agentAction(
    id: string,
    action: "connect" | "disconnect" | "sync" | "key/rotate",
    body?: object,
  ): Promise<ConnectReport> {
    return call(`/v1/agents/${id}/${action}`, "POST", JSON.stringify(body ?? {}));
  },
};

/**
 * One typed `server_request` call over Tauri IPC. The Rust side dials the
 * embedded server; the webview never fetches it cross-origin. Prefer the
 * typed `management` façade for every management operation — this hook is
 * the transport it rides on.
 */
export function useServerRequest() {
  const { t } = useI18n();

  const request = useCallback(
    async <T,>(path: string, init?: { method?: "GET" | "POST"; body?: string }): Promise<T> =>
      call<T>(path, init?.method ?? "GET", init?.body),
    [],
  );

  /** Render a `server_request` rejection as localized user-facing text. */
  const describeError = useCallback(
    (cause: unknown): string => {
      const shape = cause as ProxyErrorShape | undefined;
      if (!shape || typeof shape !== "object" || !("kind" in shape)) {
        return String(cause);
      }
      switch (shape.kind) {
        case "not_running":
          return t("server_proxy.not_running");
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
