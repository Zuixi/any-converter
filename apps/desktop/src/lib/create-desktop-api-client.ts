import type { ApiClient } from "@any-converter/core";
import { ledgerParams, usageParams } from "@any-converter/core";
import type {
  ConvertApiResponse,
  Heatmap,
  LedgerPage,
  RequestLogRecord,
  StatusData,
  UsageSummary,
} from "@any-converter/shared";

import { management, type AdminConfigView } from "./management";
import { requestServer } from "./transport";

// The single `ApiClient` for both hosts; the transport decides the wire.
export function createDesktopApiClient(): ApiClient {
  return {
    async convert(request) {
      return requestServer<ConvertApiResponse>({
        method: "POST",
        path: "/v1/admin/convert",
        body: JSON.stringify(request),
      });
    },
    async getLogs(limit?: number) {
      // The server clamps limit to 1..=1000; only callers that need fewer
      // rows (Settings reads exactly one record) should send one.
      const suffix = limit !== undefined && limit > 0 ? `?limit=${limit}` : "";
      const data = await requestServer<{ records: RequestLogRecord[] }>({
        method: "GET",
        path: `/v1/admin/logs${suffix}`,
      });
      return data.records;
    },
    async getUsage(query) {
      const data = await requestServer<{ summary: UsageSummary }>({
        method: "GET",
        path: `/v1/admin/usage?${usageParams(query)}`,
      });
      return data.summary;
    },
    async getHeatmap(filter) {
      const params = ledgerParams(filter);
      params.set("heatmap", "1");
      const data = await requestServer<{ heatmap: Heatmap }>({
        method: "GET",
        path: `/v1/admin/usage?${params}`,
      });
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
      const data = await requestServer<{ page: LedgerPage }>({
        method: "GET",
        path: `/v1/admin/usage?${params}`,
      });
      return data.page;
    },
    async getLedgerCsv(filter) {
      const params = ledgerParams(filter);
      params.set("format", "csv");
      return requestServer<string>({
        method: "GET",
        path: `/v1/admin/usage?${params}`,
      });
    },
    async getStatus() {
      return requestServer<StatusData>({ method: "GET", path: "/v1/admin/status" });
    },
    async getConfig() {
      const config = await management.getConfig();
      return {
        config: {
          server: {
            host: config.server.host,
            port: config.server.port,
          },
        },
        raw: JSON.stringify(config, null, 2),
      };
    },
    async saveConfig(raw: string) {
      const view = JSON.parse(raw) as AdminConfigView;
      await management.patchConfig(view.revision, {
        server: { host: view.server.host, port: view.server.port },
        logging: {
          dir: view.logging.dir ?? null,
          max_disk_mb: view.logging.max_disk_mb,
          request_log_enabled: view.logging.request_log_enabled,
        },
      });
    },
  };
}
