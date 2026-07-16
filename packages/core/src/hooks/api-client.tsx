"use client";

import { createContext, useContext } from "react";

import type {
  AggregatedUsage,
  ConvertApiRequest,
  ConvertApiResponse,
  RequestLogRecord,
  ServerConfig,
  StatusData,
} from "@any-converter/shared";

export interface ApiClient {
  convert(request: ConvertApiRequest): Promise<ConvertApiResponse>;
  getLogs(): Promise<RequestLogRecord[]>;
  getUsage(): Promise<AggregatedUsage[]>;
  getConfig(): Promise<{ config: ServerConfig; raw: string }>;
  saveConfig(raw: string): Promise<void>;
  getStatus(): Promise<StatusData>;
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
  async getUsage() {
    const res = await fetch("/api/usage");
    if (!res.ok) {
      throw new Error(`Failed to load usage: ${res.statusText}`);
    }
    const data = (await res.json()) as { records: AggregatedUsage[] };
    return data.records;
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
