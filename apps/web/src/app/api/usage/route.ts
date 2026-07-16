import { NextResponse } from "next/server";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

import { parseJsonl } from "@any-converter/shared";
import type { AggregatedUsage, RequestLogRecord, ResponseBodyKind, UsageRecord } from "@any-converter/shared";

import { readUsageFromSqlite } from "../log-store";

export const runtime = "nodejs";

interface UsageBucket extends AggregatedUsage {
  latency_total_ms: number;
}

function getLogDir(): string {
  const dir = process.env.LOG_DIR;
  if (!dir) {
    throw new Error("LOG_DIR environment variable is not set");
  }
  return dir;
}

export async function GET() {
  try {
    const logDir = getLogDir();
    const sqliteRecords = readUsageFromSqlite(logDir, 50);
    if (sqliteRecords && sqliteRecords.length > 0) {
      return NextResponse.json({ records: sqliteRecords });
    }

    const files = await readdir(logDir);
    const requestRecords = await readRecentRequestRecords(logDir, files);
    const records =
      requestRecords.length > 0
        ? aggregateRequestRecords(requestRecords)
        : aggregateUsageRecords(await readRecentUsageRecords(logDir, files));

    return NextResponse.json({ records: records.slice(-50) });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ records: [], error: message }, { status: 500 });
  }
}

async function readRecentRequestRecords(logDir: string, files: string[]): Promise<RequestLogRecord[]> {
  const requestFiles = files
    .filter((name) => name.startsWith("requests.") && name.endsWith(".jsonl"))
    .sort()
    .reverse();
  let records: RequestLogRecord[] = [];
  for (const file of requestFiles.slice(0, 7)) {
    const text = await readFile(join(logDir, file), "utf-8");
    records = records.concat(parseJsonl<RequestLogRecord>(text));
  }
  return records;
}

async function readRecentUsageRecords(logDir: string, files: string[]): Promise<AggregatedUsage[]> {
  const usageFiles = files
    .filter((name) => name.startsWith("usage.") && name.endsWith(".jsonl"))
    .sort()
    .reverse();
  let records: AggregatedUsage[] = [];
  for (const file of usageFiles.slice(0, 7)) {
    const text = await readFile(join(logDir, file), "utf-8");
    records = records.concat(parseJsonl<AggregatedUsage>(text));
  }
  return records;
}

function aggregateRequestRecords(records: RequestLogRecord[]): AggregatedUsage[] {
  const bucketed = new Map<string, UsageBucket>();
  for (const record of records) {
    const usage = effectiveUsage(record);
    const hour = record.timestamp.slice(0, 13);
    const existing = bucketed.get(hour);
    const totalTokens = usage.input_tokens + usage.output_tokens;
    if (existing) {
      existing.input_tokens += usage.input_tokens;
      existing.output_tokens += usage.output_tokens;
      existing.total_tokens += totalTokens;
      existing.request_count += 1;
      existing.latency_total_ms += record.latency_ms;
      existing.avg_latency_ms = Math.round(existing.latency_total_ms / existing.request_count);
      existing.max_latency_ms = Math.max(existing.max_latency_ms ?? 0, record.latency_ms);
      existing.latency_ms = existing.avg_latency_ms;
      existing.error_count = (existing.error_count ?? 0) + (record.response_status >= 400 ? 1 : 0);
    } else {
      bucketed.set(hour, {
        timestamp: `${hour}:00`,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: totalTokens,
        request_count: 1,
        status: record.response_status,
        latency_ms: record.latency_ms,
        avg_latency_ms: record.latency_ms,
        max_latency_ms: record.latency_ms,
        latency_total_ms: record.latency_ms,
        error_count: record.response_status >= 400 ? 1 : 0,
        provider: record.provider,
        client_model: record.client_model,
      });
    }
  }
  return finalizeBuckets(bucketed);
}

function aggregateUsageRecords(records: AggregatedUsage[]): AggregatedUsage[] {
  const bucketed = new Map<string, UsageBucket>();
  for (const record of records) {
    const hour = record.timestamp.slice(0, 13);
    const requestCount = record.request_count || 1;
    const latency = record.avg_latency_ms ?? record.latency_ms ?? 0;
    const existing = bucketed.get(hour);
    if (existing) {
      existing.input_tokens += record.input_tokens;
      existing.output_tokens += record.output_tokens;
      existing.total_tokens += record.total_tokens ?? record.input_tokens + record.output_tokens;
      existing.request_count += requestCount;
      existing.latency_total_ms += latency * requestCount;
      existing.avg_latency_ms = Math.round(existing.latency_total_ms / existing.request_count);
      existing.max_latency_ms = Math.max(existing.max_latency_ms ?? 0, record.max_latency_ms ?? record.latency_ms ?? 0);
      existing.latency_ms = existing.avg_latency_ms;
      existing.error_count = (existing.error_count ?? 0) + (record.status >= 400 ? requestCount : 0);
    } else {
      bucketed.set(hour, {
        ...record,
        timestamp: `${hour}:00`,
        total_tokens: record.total_tokens ?? record.input_tokens + record.output_tokens,
        request_count: requestCount,
        latency_ms: latency,
        avg_latency_ms: latency,
        max_latency_ms: record.max_latency_ms ?? record.latency_ms ?? latency,
        latency_total_ms: latency * requestCount,
        error_count: record.status >= 400 ? requestCount : 0,
      });
    }
  }
  return finalizeBuckets(bucketed);
}

function finalizeBuckets(bucketed: Map<string, UsageBucket>): AggregatedUsage[] {
  return Array.from(bucketed.values())
    .map(({ latency_total_ms: _latencyTotal, ...bucket }) => bucket)
    .sort((a, b) => a.timestamp.localeCompare(b.timestamp));
}

function effectiveUsage(record: RequestLogRecord): UsageRecord {
  const streamed = extractUsageFromResponseBody(record.response_body);
  if (streamed.input_tokens > 0 || streamed.output_tokens > 0) {
    return streamed;
  }
  return record.usage;
}

function extractUsageFromResponseBody(body: ResponseBodyKind): UsageRecord {
  if ("text" in body) {
    const parsed = safeParseObject(body.text);
    return usageFromUnknown(parsed?.usage);
  }

  let latest: UsageRecord = { input_tokens: 0, output_tokens: 0 };
  for (const block of body.lines) {
    for (const payload of extractSseDataPayloads(block)) {
      if (payload === "[DONE]") {
        continue;
      }
      const parsed = safeParseObject(payload);
      if (parsed?.type === "response.completed") {
        const response = parsed.response;
        if (response && typeof response === "object") {
          const usage = usageFromUnknown((response as Record<string, unknown>).usage);
          if (usage.input_tokens > 0 || usage.output_tokens > 0) {
            latest = usage;
          }
        }
      }
      if (parsed?.type === "message_delta") {
        const usage = usageFromUnknown(parsed.usage);
        if (usage.input_tokens > 0 || usage.output_tokens > 0) {
          latest = usage;
        }
      }
    }
  }
  return latest;
}

function usageFromUnknown(value: unknown): UsageRecord {
  if (!value || typeof value !== "object") {
    return { input_tokens: 0, output_tokens: 0 };
  }
  const usage = value as Record<string, unknown>;
  return {
    input_tokens: numberField(usage.input_tokens) ?? numberField(usage.prompt_tokens) ?? 0,
    output_tokens: numberField(usage.output_tokens) ?? numberField(usage.completion_tokens) ?? 0,
  };
}

function extractSseDataPayloads(block: string): string[] {
  return block
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .filter(Boolean);
}

function safeParseObject(text: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(text);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function numberField(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
