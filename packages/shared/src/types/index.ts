export type Format = "openai_chat" | "openai_responses" | "claude" | "gemini";

export type ConversionMode = "request" | "response";

export interface ConvertApiRequest {
  input: string;
  from: Format;
  to: Format;
  mode: ConversionMode;
}

export interface ConvertApiResponse {
  output: string;
  error?: string;
}

export interface SanitizedBody {
  text: string;
  truncated: boolean;
}

export type ResponseBodyKind =
  | { text: string }
  | { lines: string[] }
  | { type: "Json"; text: string }
  | { type: "SseLines"; lines: string[] };

export interface RequestLogRecord {
  request_id: string;
  timestamp: string;
  client_format: string;
  client_id?: string;
  session_id?: string;
  provider: string;
  client_model: string;
  upstream_model: string;
  streaming: boolean;
  method: string;
  path: string;
  request_body: SanitizedBody | null;
  upstream_request_body: SanitizedBody | null;
  response_status: number;
  response_body: ResponseBodyKind;
  latency_ms: number;
  usage: UsageRecord;
  trace?: RequestTraceSummary;
  truncated: boolean;
}

export interface RequestTraceSummary {
  client: TraceBodySummary;
  upstream: TraceBodySummary;
  response: TraceBodySummary;
}

export interface TraceBodySummary {
  messages: TraceMessage[];
  tool_definitions: TraceToolDefinition[];
  tool_calls: TraceToolCall[];
  tool_results: TraceToolResult[];
}

export interface TraceMessage {
  role: string;
  content_preview: string;
}

export interface TraceToolDefinition {
  name: string;
  namespace?: string;
}

export interface TraceToolCall {
  id?: string;
  name: string;
  namespace?: string;
  arguments_preview: string;
}

export interface TraceToolResult {
  id?: string;
  content_preview: string;
}

export interface UsageRecord {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  reasoning_tokens?: number;
}

export interface AggregatedUsage {
  timestamp: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  request_count: number;
  status: number;
  latency_ms: number;
  avg_latency_ms?: number;
  max_latency_ms?: number;
  error_count?: number;
  provider: string;
  client_model: string;
}

export interface ServerSettings {
  host?: string;
  port?: number;
  api_key?: string;
}

export interface ProviderConfig {
  name: string;
  format: string;
  base_url: string;
  api_key: string;
  model_map?: Record<string, string>;
}

export interface ModelRoute {
  models: string[];
  providers: string[];
  strategy?: string;
  upstream_model?: string;
}

export interface LegacyRoute {
  client_format: string;
  provider: string;
}

export interface LoggingConfig {
  level?: string;
  dir?: string;
  max_disk_mb?: number;
  request_log?: {
    enabled: boolean;
    max_capture_bytes?: number;
    trace_enabled?: boolean;
    trace_max_preview_bytes?: number;
  };
}

export interface ServerConfig {
  server?: ServerSettings;
  providers?: ProviderConfig[];
  model_routes?: ModelRoute[];
  routes?: LegacyRoute[];
  logging?: LoggingConfig;
}

export interface HealthStatus {
  status: "ok" | "error";
  error?: string;
}

export interface StatusData {
  health: HealthStatus;
  disk: {
    used_bytes: number;
    max_bytes: number | null;
    percent: number | null;
  };
  recentErrors: string[];
}
