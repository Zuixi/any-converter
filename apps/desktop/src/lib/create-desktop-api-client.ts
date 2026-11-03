import type { ApiClient } from "@any-converter/core";
import type { StatusData } from "@any-converter/shared";

import { api } from "./api";

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
      const settings = await api.getSettings();
      return {
        config: {
          server: {
            host: settings["server.host"],
            port: Number(settings["server.port"] ?? 1228),
          },
        },
        raw: JSON.stringify(settings, null, 2),
      };
    },
    async saveConfig(raw: string) {
      const settings = JSON.parse(raw) as Record<string, string>;
      for (const [key, value] of Object.entries(settings)) {
        await api.updateSetting({ key, value: String(value) });
      }
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
