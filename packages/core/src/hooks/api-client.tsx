"use client";

import { createContext, useContext } from "react";

import type {
  ConvertApiRequest,
  ConvertApiResponse,
  Heatmap,
  LedgerFilter,
  LedgerPage,
  RequestLogRecord,
  ServerConfig,
  StatusData,
  UsageQuery,
  UsageSummary,
} from "@any-converter/shared";

export interface ApiClient {
  convert(request: ConvertApiRequest): Promise<ConvertApiResponse>;
  getLogs(): Promise<RequestLogRecord[]>;
  getUsage(query: UsageQuery): Promise<UsageSummary>;
  getHeatmap(filter: LedgerFilter): Promise<Heatmap>;
  getLedger(filter: LedgerFilter, cursor?: number, limit?: number): Promise<LedgerPage>;
  getLedgerCsv(filter: LedgerFilter): Promise<string>;
  getConfig(): Promise<{ config: ServerConfig; raw: string }>;
  saveConfig(raw: string): Promise<void>;
  getStatus(): Promise<StatusData>;
}

const USAGE_PATH = "/api/usage";

/** The hook layer works in explicit ranges, so every usage call sends bounds. */
function rangeParams(range: { from: string; to: string }): URLSearchParams {
  const params = new URLSearchParams();
  params.set("from", range.from);
  params.set("to", range.to);
  return params;
}

function setOptional(params: URLSearchParams, key: string, value: string | undefined): void {
  if (value !== undefined && value !== "") {
    params.set(key, value);
  }
}

function usageParams(query: UsageQuery): URLSearchParams {
  const params = rangeParams(query);
  setOptional(params, "granularity", query.granularity);
  setOptional(params, "provider", query.provider);
  setOptional(params, "model", query.model);
  setOptional(params, "agent", query.agent);
  setOptional(params, "split_by", query.split_by);
  return params;
}

function ledgerParams(filter: LedgerFilter): URLSearchParams {
  const params = rangeParams(filter);
  setOptional(params, "day", filter.day);
  setOptional(params, "provider", filter.provider);
  setOptional(params, "model", filter.model);
  setOptional(params, "agent", filter.agent);
  setOptional(params, "account", filter.account);
  setOptional(params, "query", filter.query);
  if (filter.failed) {
    params.set("failed", "1");
  }
  return params;
}

function usageUrl(params: URLSearchParams): string {
  return `${USAGE_PATH}?${params.toString()}`;
}

const fetchApiClient: ApiClient = {
  async convert(request) {
    const res = await fetch("/api/convert", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
    const data = (await res.json()) as ConvertApiResponse;
    if (!res.ok || data.error) {
      throw new Error(data.error ?? "Conversion failed");
    }
    return data;
  },
  async getLogs() {
    const res = await fetch("/api/logs");
    if (!res.ok) {
      throw new Error(`Failed to load logs: ${res.statusText}`);
    }
    const data = (await res.json()) as { records: RequestLogRecord[] };
    return data.records;
  },
  async getUsage(query) {
    const res = await fetch(usageUrl(usageParams(query)));
    if (!res.ok) {
      throw new Error(`Failed to load usage: ${res.statusText}`);
    }
    const data = (await res.json()) as { summary: UsageSummary };
    return data.summary;
  },
  async getHeatmap(filter) {
    const params = ledgerParams(filter);
    params.set("heatmap", "1");
    const res = await fetch(usageUrl(params));
    if (!res.ok) {
      throw new Error(`Failed to load heatmap: ${res.statusText}`);
    }
    const data = (await res.json()) as { heatmap: Heatmap };
    return data.heatmap;
  },
  async getLedger(filter, cursor, limit) {
    const params = ledgerParams(filter);
    params.set("ledger", "1");
    if (cursor !== undefined) {
      params.set("cursor", String(cursor));
    }
    if (limit !== undefined) {
      params.set("limit", String(limit));
    }
    const res = await fetch(usageUrl(params));
    if (!res.ok) {
      throw new Error(`Failed to load ledger: ${res.statusText}`);
    }
    const data = (await res.json()) as { page: LedgerPage };
    return data.page;
  },
  async getLedgerCsv(filter) {
    const params = ledgerParams(filter);
    params.set("format", "csv");
    const res = await fetch(usageUrl(params));
    if (!res.ok) {
      throw new Error(`Failed to export ledger: ${res.statusText}`);
    }
    return res.text();
  },
  async getConfig() {
    const res = await fetch("/api/config");
    if (!res.ok) {
      throw new Error(`Failed to load config: ${res.statusText}`);
    }
    return (await res.json()) as { config: ServerConfig; raw: string };
  },
  async saveConfig(raw) {
    const res = await fetch("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ raw }),
    });
    if (!res.ok) {
      const data = (await res.json()) as { error?: string };
      throw new Error(data.error ?? "Failed to save config");
    }
  },
  async getStatus() {
    const res = await fetch("/api/status");
    if (!res.ok) {
      throw new Error(`Failed to load status: ${res.statusText}`);
    }
    return (await res.json()) as StatusData;
  },
};

const ApiClientContext = createContext<ApiClient>(fetchApiClient);

export function ApiClientProvider({ client, children }: { client: ApiClient; children: React.ReactNode }) {
  return <ApiClientContext.Provider value={client}>{children}</ApiClientContext.Provider>;
}

export function useApiClient() {
  return useContext(ApiClientContext);
}
