import type { RequestLogRecord, ResponseBodyKind, TraceMessage, UsageRecord } from "@any-converter/shared";

export type ConversationEntryKind = "system" | "user" | "assistant" | "tool_call" | "tool_result" | "response";

export interface ConversationEntry {
  id: string;
  kind: ConversationEntryKind;
  title: string;
  content: string;
  /** "left" = AI/system/tool, "right" = user */
  side: "left" | "right";
  /** Whether the entry should render collapsed by default */
  collapsible?: boolean;
  /** Short summary shown when collapsed */
  collapsedTitle?: string;
}

export interface ConversationRequestSummary {
  record: RequestLogRecord;
  title: string;
  subtitle: string;
  newItemCount: number;
}

export interface ConversationDetail {
  timelineEntries: ConversationEntry[];
}

export interface SessionSummary {
  key: string;
  clientId: string;
  sessionId?: string;
  title: string;
  subtitle: string;
  requestCount: number;
  latestTimestamp: string;
  records: RequestLogRecord[];
}

export function sortRecordsAscending(records: RequestLogRecord[]): RequestLogRecord[] {
  return [...records].sort((a, b) => a.timestamp.localeCompare(b.timestamp));
}

/**
 * Group request records into sessions.
 *
 * Explicit `(client_id, session_id)` pairs are authoritative. Without a
 * `session_id`, records merge only when one uniquely longest prior history is
 * a strict prefix; uncertain records remain single-request sessions.
 */
export function buildSessionSummaries(records: RequestLogRecord[]): SessionSummary[] {
  const sorted = sortRecordsAscending(records);
  const sessions = new Map<string, RequestLogRecord[]>();
  const inferredKeys = new Map<string, string[]>();

  for (const record of sorted) {
    const clientId = record.client_id ?? record.client_format;
    let key = record.session_id ? `${clientId}:${record.session_id}` : `${clientId}:__single_${record.request_id}`;

    if (!record.session_id && record.client_id && conversationHistorySize(record) > 0) {
      const clientKeys = inferredKeys.get(clientId) ?? [];
      // ponytail: this scans the small recent-log set; index histories only if log volume makes it measurable.
      const candidates = clientKeys.flatMap((candidateKey) => {
        const candidateRecords = sessions.get(candidateKey);
        const latest = candidateRecords?.[candidateRecords.length - 1];
        return latest && strictlyExtendsConversation(latest, record)
          ? [{ key: candidateKey, historySize: conversationHistorySize(latest) }]
          : [];
      });
      const longestHistory = Math.max(0, ...candidates.map((candidate) => candidate.historySize));
      const longest = candidates.filter((candidate) => candidate.historySize === longestHistory);
      const match = longest.length === 1 ? longest[0]?.key : undefined;

      if (match) {
        key = match;
      } else {
        key = `${clientId}:__inferred_${record.request_id}`;
        clientKeys.push(key);
        inferredKeys.set(clientId, clientKeys);
      }
    }

    const list = sessions.get(key) ?? [];
    list.push(record);
    sessions.set(key, list);
  }

  const summaries: SessionSummary[] = [];
  for (const [key, sessionRecords] of sessions) {
    const first = sessionRecords[0];
    const latest = sessionRecords[sessionRecords.length - 1];
    if (!first || !latest) {
      continue;
    }
    const clientId = first.client_id ?? first.client_format;

    // Find the first user message across all requests in the session for the title.
    let title = "";
    for (const record of sessionRecords) {
      const detail = buildConversationDetail(record, undefined);
      const userEntry = detail.timelineEntries.find((e) => e.kind === "user");
      if (userEntry) {
        title = compactText(userEntry.content, 80);
        break;
      }
    }
    if (!title) {
      title = first.path || first.request_id;
    }

    const totalTokens = sessionRecords.reduce((sum, r) => {
      const usage = effectiveUsage(r);
      return sum + usage.input_tokens + usage.output_tokens;
    }, 0);

    summaries.push({
      key,
      clientId,
      sessionId: first.session_id,
      title,
      subtitle: `${clientId} · ${sessionRecords.length} requests · ${totalTokens.toLocaleString()} tokens`,
      requestCount: sessionRecords.length,
      latestTimestamp: latest.timestamp,
      records: sessionRecords,
    });
  }

  // Sort by latest activity descending.
  summaries.sort((a, b) => b.latestTimestamp.localeCompare(a.latestTimestamp));
  return summaries;
}

function conversationHistorySize(record: RequestLogRecord): number {
  const trace = record.trace?.client;
  return trace ? trace.messages.length + trace.tool_calls.length + trace.tool_results.length : 0;
}

function strictlyExtendsConversation(previous: RequestLogRecord, current: RequestLogRecord): boolean {
  const left = previous.trace?.client;
  const right = current.trace?.client;
  if (!left || !right || conversationHistorySize(current) <= conversationHistorySize(previous)) {
    return false;
  }

  return (
    sharedPrefixLength(left.messages, right.messages, sameTraceMessage) === left.messages.length &&
    sharedPrefixLength(left.tool_calls.map(stableToolId), right.tool_calls.map(stableToolId), Object.is) ===
      left.tool_calls.length &&
    sharedPrefixLength(
      left.tool_results.map((item) => item.id ?? item.content_preview),
      right.tool_results.map((item) => item.id ?? item.content_preview),
      Object.is,
    ) === left.tool_results.length
  );
}

export function buildRequestSummaries(records: RequestLogRecord[]): ConversationRequestSummary[] {
  const sorted = sortRecordsAscending(records);
  return sorted.map((record, index) => {
    const previous = sorted[index - 1];
    const detail = buildConversationDetail(record, previous);
    const latestUser = [...detail.timelineEntries].reverse().find((entry) => entry.kind === "user");
    const fallback = record.path || record.request_id;
    return {
      record,
      title: latestUser ? compactText(latestUser.content, 80) : fallback,
      subtitle: `${record.provider} · ${record.upstream_model} · ${record.response_status}`,
      newItemCount: detail.timelineEntries.length,
    };
  });
}

export function buildConversationDetail(record: RequestLogRecord, previous?: RequestLogRecord): ConversationDetail {
  const contextEntries = buildContextEntries(record, previous);
  const responseEntries = buildResponseEntries(record);

  // Merge into a single chronological timeline.
  const timeline: ConversationEntry[] = [];
  const systemEntries = contextEntries.filter((e) => e.kind === "system");
  const nonSystemContext = contextEntries.filter((e) => e.kind !== "system");

  if (systemEntries.length > 0) {
    timeline.push(...systemEntries);
  }
  timeline.push(...nonSystemContext);
  timeline.push(...responseEntries);

  return { timelineEntries: timeline };
}

export function responseBodyToText(body: ResponseBodyKind): string {
  if ("lines" in body) {
    return body.lines.join("\n");
  }
  return body.text;
}

export function effectiveUsage(record: RequestLogRecord): UsageRecord {
  const streamUsage = extractUsageFromResponseBody(record.response_body);
  if (hasUsage(streamUsage)) {
    return streamUsage;
  }
  return record.usage;
}

export function totalUsage(records: RequestLogRecord[]): UsageRecord {
  const initial: UsageRecord = { input_tokens: 0, output_tokens: 0 };
  return records.reduce(
    (total, record) => {
      const usage = effectiveUsage(record);
      return {
        input_tokens: total.input_tokens + usage.input_tokens,
        output_tokens: total.output_tokens + usage.output_tokens,
        cache_read_tokens: addOptional(total.cache_read_tokens, usage.cache_read_tokens),
        cache_write_tokens: addOptional(total.cache_write_tokens, usage.cache_write_tokens),
        reasoning_tokens: addOptional(total.reasoning_tokens, usage.reasoning_tokens),
      };
    },
    initial,
  );
}

function buildContextEntries(record: RequestLogRecord, previous?: RequestLogRecord): ConversationEntry[] {
  const current = record.trace?.client;
  if (!current) {
    return [];
  }

  const previousMessages = previous?.trace?.client.messages ?? [];
  const previousToolCallIds = new Set((previous?.trace?.client.tool_calls ?? []).map(stableToolId));
  const previousToolResultIds = new Set((previous?.trace?.client.tool_results ?? []).map((item) => item.id ?? item.content_preview));
  const entries: ConversationEntry[] = [];

  const messageStart = sharedPrefixLength(previousMessages, current.messages, sameTraceMessage);
  current.messages.slice(messageStart).forEach((message, index) => {
    if (!message.content_preview.trim()) {
      return;
    }
    const kind = messageKind(message.role);
    const content = normalizeContentPreview(message.content_preview);
    entries.push({
      id: `${record.request_id}-message-${messageStart + index}`,
      kind,
      title: roleTitle(message.role),
      content,
      side: kind === "user" ? "right" : "left",
      collapsible: kind === "system" && content.length > 200,
      collapsedTitle: kind === "system" ? `System prompt · ${content.length} chars` : undefined,
    });
  });

  current.tool_calls.forEach((tool, index) => {
    const stableId = stableToolId(tool);
    if (previousToolCallIds.has(stableId)) {
      return;
    }
    const name = tool.namespace ? `${tool.namespace}.${tool.name}` : tool.name;
    entries.push({
      id: `${record.request_id}-tool-call-${index}`,
      kind: "tool_call",
      title: `Tool call · ${name}`,
      content: tool.arguments_preview || "{}",
      side: "left",
    });
  });

  current.tool_results.forEach((result, index) => {
    const stableId = result.id ?? result.content_preview;
    if (previousToolResultIds.has(stableId)) {
      return;
    }
    entries.push({
      id: `${record.request_id}-tool-result-${index}`,
      kind: "tool_result",
      title: result.id ? `Tool result · ${result.id}` : "Tool result",
      content: result.content_preview,
      side: "left",
      collapsible: true,
      collapsedTitle: result.id ? `Tool result · ${result.id}` : "Tool result",
    });
  });

  if (entries.length === 0 && current.messages.length > 0) {
    const latest = current.messages[current.messages.length - 1];
    if (latest) {
      const kind = messageKind(latest.role);
      const content = normalizeContentPreview(latest.content_preview);
      entries.push({
        id: `${record.request_id}-latest-message`,
        kind,
        title: `Latest ${roleTitle(latest.role)}`,
        content,
        side: kind === "user" ? "right" : "left",
        collapsible: kind === "system" && content.length > 200,
        collapsedTitle: kind === "system" ? `System prompt · ${content.length} chars` : undefined,
      });
    }
  }

  return entries;
}

function buildResponseEntries(record: RequestLogRecord): ConversationEntry[] {
  const entries: ConversationEntry[] = [];
  const text = extractAssistantTextFromResponseBody(record.response_body);
  if (text.trim()) {
    entries.push({
      id: `${record.request_id}-response-text`,
      kind: "response",
      title: "Assistant",
      content: text,
      side: "left",
    });
  }

  const responseTrace = record.trace?.response;
  if (responseTrace) {
    responseTrace.tool_calls.forEach((tool, index) => {
      const name = tool.namespace ? `${tool.namespace}.${tool.name}` : tool.name;
      entries.push({
        id: `${record.request_id}-response-tool-call-${index}`,
        kind: "tool_call",
        title: `Tool call · ${name}`,
        content: tool.arguments_preview || "{}",
        side: "left",
      });
    });

    if (!text.trim()) {
      responseTrace.messages
        .filter((message) => message.content_preview.trim())
        .forEach((message, index) => {
          entries.push({
            id: `${record.request_id}-response-message-${index}`,
            kind: "response",
            title: "Assistant",
            content: normalizeContentPreview(message.content_preview),
            side: "left",
          });
        });
    }
  }

  return entries;
}

function extractAssistantTextFromResponseBody(body: ResponseBodyKind): string {
  if ("text" in body) {
    return extractAssistantTextFromJsonText(body.text) ?? body.text;
  }

  const chunks: string[] = [];
  for (const line of body.lines) {
    for (const data of extractSseDataPayloads(line)) {
      if (data === "[DONE]") {
        continue;
      }
      const parsed = safeParseObject(data);
      const eventType = typeof parsed?.type === "string" ? parsed.type : "";
      if (eventType === "response.output_text.delta" && typeof parsed?.delta === "string") {
        chunks.push(parsed.delta);
      }
      if (eventType === "content_block_delta" && parsed?.delta && typeof parsed.delta === "object") {
        const delta = parsed.delta as Record<string, unknown>;
        if (typeof delta.text === "string") {
          chunks.push(delta.text);
        }
      }
    }
  }

  return chunks.join("");
}

function extractUsageFromResponseBody(body: ResponseBodyKind): UsageRecord {
  if ("text" in body) {
    const parsed = safeParseObject(body.text);
    return usageFromObject(parsed?.usage);
  }

  let latest: UsageRecord = { input_tokens: 0, output_tokens: 0 };
  for (const line of body.lines) {
    for (const data of extractSseDataPayloads(line)) {
      if (data === "[DONE]") {
        continue;
      }
      const parsed = safeParseObject(data);
      const eventType = typeof parsed?.type === "string" ? parsed.type : "";
      if (eventType === "response.completed") {
        const response = parsed?.response;
        if (response && typeof response === "object") {
          const usage = usageFromObject((response as Record<string, unknown>).usage);
          if (hasUsage(usage)) {
            latest = usage;
          }
        }
      }
      if (eventType === "message_delta") {
        const usage = usageFromObject(parsed?.usage);
        if (hasUsage(usage)) {
          latest = usage;
        }
      }
    }
  }
  return latest;
}

function usageFromObject(value: unknown): UsageRecord {
  if (!value || typeof value !== "object") {
    return { input_tokens: 0, output_tokens: 0 };
  }
  const usage = value as Record<string, unknown>;
  return {
    input_tokens: numberField(usage.input_tokens) ?? numberField(usage.prompt_tokens) ?? 0,
    output_tokens: numberField(usage.output_tokens) ?? numberField(usage.completion_tokens) ?? 0,
    cache_read_tokens: numberField(usage.cache_read_tokens) ?? numberField(usage.cache_read_input_tokens),
    cache_write_tokens: numberField(usage.cache_write_tokens) ?? numberField(usage.cache_creation_input_tokens),
    reasoning_tokens: numberField(usage.reasoning_tokens),
  };
}

function hasUsage(usage: UsageRecord): boolean {
  return (
    usage.input_tokens > 0 ||
    usage.output_tokens > 0 ||
    usage.cache_read_tokens !== undefined ||
    usage.cache_write_tokens !== undefined ||
    usage.reasoning_tokens !== undefined
  );
}

function numberField(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function addOptional(left: number | undefined, right: number | undefined): number | undefined {
  if (left === undefined && right === undefined) {
    return undefined;
  }
  return (left ?? 0) + (right ?? 0);
}

function extractAssistantTextFromJsonText(text: string): string | undefined {
  const parsed = safeParse(text);
  if (parsed === undefined) {
    return undefined;
  }
  if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const output = (parsed as Record<string, unknown>).output;
    if (Array.isArray(output)) {
      return extractTextFromContentValue(output);
    }
  }
  return extractTextFromContentValue(parsed);
}

function normalizeContentPreview(preview: string): string {
  const parsed = safeParse(preview);
  const text = extractTextFromContentValue(parsed);
  if (text.trim()) {
    return text;
  }
  return preview;
}

function extractTextFromContentValue(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map(extractTextFromContentValue).filter(Boolean).join("\n");
  }
  if (!value || typeof value !== "object") {
    return "";
  }
  const object = value as Record<string, unknown>;
  if (typeof object.text === "string") {
    return object.text;
  }
  if (typeof object.content === "string") {
    return object.content;
  }
  if (Array.isArray(object.content)) {
    return extractTextFromContentValue(object.content);
  }
  return "";
}

function safeParse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

function extractSseDataPayloads(block: string): string[] {
  return block
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .filter(Boolean);
}

function sharedPrefixLength<T>(left: T[], right: T[], equal: (a: T, b: T) => boolean): number {
  let index = 0;
  while (index < left.length && index < right.length) {
    const leftItem = left[index];
    const rightItem = right[index];
    if (leftItem === undefined || rightItem === undefined || !equal(leftItem, rightItem)) {
      break;
    }
    index += 1;
  }
  return index;
}

function sameTraceMessage(left: TraceMessage, right: TraceMessage): boolean {
  return left.role === right.role && left.content_preview === right.content_preview;
}

function messageKind(role: string): ConversationEntryKind {
  if (role === "system" || role === "developer") {
    return "system";
  }
  if (role === "assistant") {
    return "assistant";
  }
  return "user";
}

function roleTitle(role: string): string {
  if (role === "developer") {
    return "Developer";
  }
  return role.charAt(0).toUpperCase() + role.slice(1);
}

function stableToolId(tool: { id?: string; name: string; namespace?: string; arguments_preview: string }): string {
  return [tool.id ?? "", tool.namespace ?? "", tool.name, tool.arguments_preview].join("|");
}

function safeParseObject(text: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(text);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function compactText(text: string, maxLength: number): string {
  const normalized = text.replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) {
    return normalized;
  }
  return `${normalized.slice(0, maxLength - 1)}...`;
}
