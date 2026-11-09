import type { ApiClient } from "@any-converter/core";
import type { StatusData } from "@any-converter/shared";

import { api } from "./api";
import { management, type AdminConfigView } from "./management";

export function createDesktopApiClient(): ApiClient {
  return {
    async convert(request) {
      const result = await api.convertPayload(request);
      return { output: result.output };
    },
    async getLogs() {
      return api.listRequestLogs(500);
    },
    async getUsage(query) {
      return api.getUsageSummary("7d", query.granularity);
    },
    async getLedger(filter, cursor, limit) {
      return api.getLedgerPage(filter, cursor, limit);
    },
    async getHeatmap(filter) {
      return api.getHeatmap(filter);
    },
    async getLedgerCsv() {
      throw new Error("Ledger CSV export is not available in the desktop app");
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
    async getStatus() {
      const status = await api.getServerStatus();
      const data: StatusData = {
        health: {
          status: status.state === "running" ? "ok" : "error",
          error: status.last_error ?? (status.state === "running" ? undefined : status.state),
        },
        disk: { used_bytes: 0, max_bytes: null, percent: null },
        recentErrors: status.last_error ? [status.last_error] : [],
      };
      return data;
    },
  };
}
