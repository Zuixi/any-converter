import type { RequestLogRecord, ResponseBodyKind, TraceMessage, UsageRecord } from "@any-converter/shared";

export type ConversationEntryKind = "system" | "user" | "assistant" | "tool_call" | "tool_result" | "response";

export interface ConversationEntry {
  id: string;
  kind: ConversationEntryKind;
  title: string;
  content: string;
}

export interface ConversationRequestSummary {
  record: RequestLogRecord;
  title: string;
  subtitle: string;
  newItemCount: number;
}

export interface ConversationDetail {
  contextEntries: ConversationEntry[];
  responseEntries: ConversationEntry[];
}

export function sortRecordsAscending(records: RequestLogRecord[]): RequestLogRecord[] {
  return [...records].sort((a, b) => a.timestamp.localeCompare(b.timestamp));
}

export function buildRequestSummaries(records: RequestLogRecord[]): ConversationRequestSummary[] {
  const sorted = sortRecordsAscending(records);
  return sorted.map((record, index) => {
    const previous = sorted[index - 1];
    const detail = buildConversationDetail(record, previous);
    const latestUser = [...detail.contextEntries].reverse().find((entry) => entry.kind === "user");
    const fallback = record.path || record.request_id;
    return {
      record,
      title: latestUser ? compactText(latestUser.content, 80) : fallback,
      subtitle: `${record.provider} · ${record.upstream_model} · ${record.response_status}`,
      newItemCount: detail.contextEntries.length + detail.responseEntries.length,
    };
  });
}

export function buildConversationDetail(record: RequestLogRecord, previous?: RequestLogRecord): ConversationDetail {
  const contextEntries = buildContextEntries(record, previous);
  const responseEntries = buildResponseEntries(record);
  return { contextEntries, responseEntries };
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
    entries.push({
      id: `${record.request_id}-message-${messageStart + index}`,
      kind: messageKind(message.role),
      title: roleTitle(message.role),
      content: normalizeContentPreview(message.content_preview),
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
    });
  });

  if (entries.length === 0 && current.messages.length > 0) {
    const latest = current.messages[current.messages.length - 1];
    if (latest) {
      entries.push({
        id: `${record.request_id}-latest-message`,
        kind: messageKind(latest.role),
        title: `Latest ${roleTitle(latest.role)}`,
        content: normalizeContentPreview(latest.content_preview),
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
      title: "LLM response",
      content: text,
    });
  }

  const responseTrace = record.trace?.response;
  if (responseTrace) {
    responseTrace.tool_calls.forEach((tool, index) => {
      const name = tool.namespace ? `${tool.namespace}.${tool.name}` : tool.name;
      entries.push({
        id: `${record.request_id}-response-tool-call-${index}`,
        kind: "tool_call",
        title: `LLM tool call · ${name}`,
        content: tool.arguments_preview || "{}",
      });
    });

    if (!text.trim()) {
      responseTrace.messages
        .filter((message) => message.content_preview.trim())
        .forEach((message, index) => {
          entries.push({
            id: `${record.request_id}-response-message-${index}`,
            kind: "response",
            title: "LLM response",
            content: normalizeContentPreview(message.content_preview),
          });
        });
    }
  }

  return entries;
}

function extractAssistantTextFromResponseBody(body: ResponseBodyKind): string {
  if ("text" in body) {
    return extractAssistantTextFromJsonText(body.text) || body.text;
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

function extractAssistantTextFromJsonText(text: string): string {
  const parsed = safeParseObject(text);
  if (!parsed) {
    return "";
  }
  const output = Array.isArray(parsed.output) ? parsed.output : [];
  const chunks: string[] = [];
  for (const item of output) {
    if (!item || typeof item !== "object") {
      continue;
    }
    const content = Array.isArray((item as Record<string, unknown>).content)
      ? ((item as Record<string, unknown>).content as unknown[])
      : [];
    for (const part of content) {
      if (part && typeof part === "object") {
        const textPart = (part as Record<string, unknown>).text;
        if (typeof textPart === "string") {
          chunks.push(textPart);
        }
      }
    }
  }
  return chunks.join("");
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
