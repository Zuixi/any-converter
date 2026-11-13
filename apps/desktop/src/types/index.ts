export type AppPath =
  "/dashboard" | "/providers" | "/routes" | "/agents" | "/playground" | "/logs" | "/usage" | "/settings";

export interface ServerStatus {
  state: "stopped" | "starting" | "running" | "stopping" | "failed" | "attached";
  host: string;
  port: number;
  last_error?: string;
}

/** One management call forwarded to the embedded server over IPC. */
export interface ServerProxyRequest {
  method: "GET" | "POST" | "PATCH" | "PUT" | "DELETE";
  path: string;
  body?: string;
  ifMatch?: string;
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
