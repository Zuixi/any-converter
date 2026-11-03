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

export type Granularity = "hour" | "day" | "week" | "month";

export interface UsageTotals {
  calls: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  cache_write_1h: number;
  cache_unknown_tokens: number;
  reasoning_tokens: number;
  latency_sum_ms: number;
  latency_max_ms: number;
  ttft_sum_ms: number;
  ttft_calls: number;
  cost_usd: number;
  /** Calls with tokens but no price in the table, so `cost_usd` excludes them. */
  unpriced: number;
  /** Requests answered after a try failed first: a success with `attempt > 0`. */
  retried: number;
  /** Of `errors`, those the gateway ended for looping. */
  loops: number;
  /** null when no vendor counted a cache for any call in the range. */
  hit_rate: number | null;
  mean_latency_ms: number | null;
  mean_ttft_ms: number | null;
}

/** The dimension a series point can be told apart by. */
export type SplitDim = "provider" | "agent" | "model" | "account";

export interface UsageBucket {
  label: string;
  timestamp: string;
  totals: UsageTotals;
  /**
   * The point split by `UsageQuery.split_by`: value → that value's totals here.
   * Absent unless a split was asked for, and absent entirely when no split value
   * survived; a value with no calls in a bucket has no entry, which reads as zero.
   */
  by?: Record<string, UsageTotals>;
}

export interface UsageGroup {
  id: string;
  provider?: string;
  model?: string;
  totals: UsageTotals;
}

export interface UsageSummary {
  from: string;
  to: string;
  granularity: Granularity;
  totals: UsageTotals;
  series: UsageBucket[];
  by_provider: UsageGroup[];
  by_model: UsageGroup[];
  by_agent: UsageGroup[];
  by_account: UsageGroup[];
  by_session: UsageGroup[];
}

export interface UsageQuery {
  from: string;
  to: string;
  granularity?: Granularity;
  provider?: string;
  model?: string;
  agent?: string;
  account?: string;
  /** Adds `UsageBucket.by`, one entry per split value. `account` is not splittable on the rollup. */
  split_by?: SplitDim;
}

/** One day of the heatmap calendar. Only days with calls are listed. */
export interface HeatDay {
  /** `YYYY-MM-DD`, UTC. */
  date: string;
  calls: number;
  /** Input + output + cache traffic, as the chart counts it. */
  tokens: number;
  cost_usd: number;
  /** Calls inside this day with no price, so `cost_usd` covers the rest. */
  unpriced: number;
}

/** The last 53 weeks, a day each, oldest first. */
export interface Heatmap {
  /** `YYYY-MM-DD` of the first Monday shown. */
  from: string;
  /** `YYYY-MM-DD` of today. */
  to: string;
  /** 53: 52 whole weeks behind this one, plus the current partial column. */
  weeks: number;
  days: HeatDay[];
}

export interface LedgerRow {
  request_id: string;
  timestamp: string;
  provider: string;
  client_model: string;
  upstream_model: string;
  served_model?: string;
  agent?: string;
  account?: string;
  session_id?: string;
  status: number;
  /** Zero-based index of the try that answered: `n` means `n` failovers. */
  attempt: number;
  streaming: boolean;
  latency_ms: number;
  ttft_ms: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  cost_usd?: number;
}

export interface LedgerPage {
  rows: LedgerRow[];
  total: number;
  next_cursor?: number;
  totals: UsageTotals;
}

export interface LedgerFilter {
  from: string;
  to: string;
  day?: string;
  provider?: string;
  model?: string;
  agent?: string;
  account?: string;
  failed?: boolean;
  /** Only calls answered after an earlier try failed (`attempt > 0`). */
  retried?: boolean;
  query?: string;
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
