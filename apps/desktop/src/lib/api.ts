import { invoke } from "@tauri-apps/api/core";
import type { Heatmap, LedgerFilter, LedgerPage, RequestLogRecord, UsageSummary } from "@any-converter/shared";

import type { ConvertPayloadRequest, ConvertPayloadResponse, ServerProxyRequest, ServerStatus } from "../types";

export const api = {
  convertPayload(request: ConvertPayloadRequest): Promise<ConvertPayloadResponse> {
    return invoke("convert_payload", { request });
  },

  listRequestLogs(limit = 500): Promise<RequestLogRecord[]> {
    return invoke("list_request_logs", { limit });
  },

  getUsageSummary(preset = "7d", granularity = "day"): Promise<UsageSummary> {
    return invoke("get_usage_summary", { preset, granularity });
  },

  getHeatmap(filter: LedgerFilter): Promise<Heatmap> {
    return invoke("get_heatmap", { filter });
  },
  getLedgerPage(filter: LedgerFilter, cursor?: number, limit?: number): Promise<LedgerPage> {
    return invoke("list_ledger_page", { filter, cursor, limit });
  },

  getServerStatus(): Promise<ServerStatus> {
    return invoke("get_server_status");
  },

  serverRequest(request: ServerProxyRequest): Promise<unknown> {
    return invoke("server_request", { args: request });
  },

  startServer(): Promise<ServerStatus> {
    return invoke("start_server");
  },

  stopServer(): Promise<ServerStatus> {
    return invoke("stop_server");
  },

  restartServer(): Promise<ServerStatus> {
    return invoke("restart_server");
  },
};
