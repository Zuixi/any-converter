export type AppPath =
  | "/dashboard"
  | "/providers"
  | "/routes"
  | "/playground"
  | "/logs"
  | "/usage"
  | "/settings";

export interface DesktopProvider {
  id: number;
  name: string;
  format: string;
  base_url: string;
  keychain_ref: string;
}

export interface DesktopRoute {
  id: number;
  pattern: string;
  providers: string[];
  upstream_model?: string;
  strategy: string;
}

export interface ServerStatus {
  state: "stopped" | "starting" | "running" | "error";
  host: string;
  port: number;
  last_error?: string;
}

export interface CreateProviderRequest {
  name: string;
  format: string;
  base_url: string;
  api_key: string;
}

export interface CreateModelRouteRequest {
  pattern: string;
  provider_ids: number[];
  upstream_model: string | null;
  strategy: string;
}

export interface UpdateSettingRequest {
  key: string;
  value: string;
}

export interface ConvertPayloadRequest {
  input: string;
  from: string;
  to: string;
  mode: string;
}

export interface ConvertPayloadResponse {
  output: string;
}
