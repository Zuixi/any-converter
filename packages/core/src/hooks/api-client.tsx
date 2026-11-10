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
  getLogs(limit?: number): Promise<RequestLogRecord[]>;
  getUsage(query: UsageQuery): Promise<UsageSummary>;
  getHeatmap(filter: LedgerFilter): Promise<Heatmap>;
  getLedger(filter: LedgerFilter, cursor?: number, limit?: number): Promise<LedgerPage>;
  getLedgerCsv(filter: LedgerFilter): Promise<string>;
  getConfig(): Promise<{ config: ServerConfig; raw: string }>;
  saveConfig(raw: string): Promise<void>;
  getStatus(): Promise<StatusData>;
}

const ApiClientContext = createContext<ApiClient | null>(null);

export function ApiClientProvider({ client, children }: { client: ApiClient; children: React.ReactNode }) {
  return <ApiClientContext.Provider value={client}>{children}</ApiClientContext.Provider>;
}

export function useApiClient(): ApiClient {
  const client = useContext(ApiClientContext);
  if (client === null) {
    throw new Error("useApiClient must be used inside <ApiClientProvider>");
  }
  return client;
}
