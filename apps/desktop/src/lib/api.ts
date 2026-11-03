import { invoke } from "@tauri-apps/api/core";
import type {
  Heatmap,
  LedgerFilter,
  LedgerPage,
  RequestLogRecord,
  UsageSummary,
} from "@any-converter/shared";

import type {
  ConvertPayloadRequest,
  ConvertPayloadResponse,
  CreateModelRouteRequest,
  CreateProviderRequest,
  DesktopProvider,
  DesktopRoute,
  ServerStatus,
  UpdateSettingRequest,
} from "../types";

export const api = {
  getSettings(): Promise<Record<string, string>> {
    return invoke("get_settings");
  },

  updateSetting(request: UpdateSettingRequest): Promise<Record<string, string>> {
    return invoke("update_setting", { request });
  },

  listProviders(): Promise<DesktopProvider[]> {
    return invoke("list_providers");
  },

  createProvider(request: CreateProviderRequest): Promise<DesktopProvider[]> {
    return invoke("create_provider", { request });
  },

  deleteProvider(id: number): Promise<DesktopProvider[]> {
    return invoke("delete_provider", { id });
  },

  listModelRoutes(): Promise<DesktopRoute[]> {
    return invoke("list_model_routes");
  },

  createModelRoute(request: CreateModelRouteRequest): Promise<DesktopRoute[]> {
    return invoke("create_model_route", { request });
  },

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
