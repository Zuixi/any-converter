import { NextResponse } from "next/server";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

import { parseJsonl } from "@any-converter/shared";
import type { RequestLogRecord } from "@any-converter/shared";

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
    const requestFiles = files
      .filter((name) => name.startsWith("requests.") && name.endsWith(".jsonl"))
      .sort()
      .reverse();

    let allRecords: RequestLogRecord[] = [];
    for (const file of requestFiles.slice(0, 3)) {
      const text = await readFile(join(logDir, file), "utf-8");
      const records = parseJsonl<RequestLogRecord>(text);
      allRecords = allRecords.concat(records);
    }

    allRecords.sort((a, b) => b.timestamp.localeCompare(a.timestamp));
    return NextResponse.json({ records: allRecords.slice(0, 500) });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ records: [], error: message }, { status: 500 });
  }
}
