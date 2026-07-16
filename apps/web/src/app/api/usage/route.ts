import { NextResponse } from "next/server";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

import { parseJsonl } from "@any-converter/shared";
import type { AggregatedUsage } from "@any-converter/shared";

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
    const files = await readdir(logDir);
    const usageFiles = files
      .filter((name) => name.startsWith("usage.") && name.endsWith(".jsonl"))
      .sort()
      .reverse();

    let allRecords: AggregatedUsage[] = [];
    for (const file of usageFiles.slice(0, 7)) {
      const text = await readFile(join(logDir, file), "utf-8");
      const records = parseJsonl<AggregatedUsage>(text);
      allRecords = allRecords.concat(records);
    }

    const bucketed = new Map<string, AggregatedUsage>();
    for (const record of allRecords) {
      const hour = record.timestamp.slice(0, 13);
      const existing = bucketed.get(hour);
      if (existing) {
        bucketed.set(hour, {
          ...existing,
          input_tokens: existing.input_tokens + record.input_tokens,
          output_tokens: existing.output_tokens + record.output_tokens,
          total_tokens:
            (existing.total_tokens ?? existing.input_tokens + existing.output_tokens) +
            (record.total_tokens ?? record.input_tokens + record.output_tokens),
          request_count: existing.request_count + 1,
          latency_ms: Math.max(existing.latency_ms, record.latency_ms),
        });
      } else {
        bucketed.set(hour, {
          ...record,
          timestamp: `${hour}:00`,
          total_tokens: record.total_tokens ?? record.input_tokens + record.output_tokens,
          request_count: 1,
        });
      }
    }

    const sorted = Array.from(bucketed.values()).sort((a, b) => a.timestamp.localeCompare(b.timestamp));
    return NextResponse.json({ records: sorted.slice(-50) });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ records: [], error: message }, { status: 500 });
  }
}
