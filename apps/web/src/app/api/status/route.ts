import { NextResponse } from "next/server";
import { readdir, readFile, stat } from "node:fs/promises";
import { join } from "node:path";

import { parseJsonl } from "@any-converter/shared";
import type { HealthStatus, StatusData } from "@any-converter/shared";

function getLogDir(): string {
  const dir = process.env.LOG_DIR;
  if (!dir) {
    throw new Error("LOG_DIR environment variable is not set");
  }
  return dir;
}

function getServerUrl(): string {
  return process.env.SERVER_URL ?? "http://127.0.0.1:8080";
}

async function fetchHealth(): Promise<HealthStatus> {
  try {
    const res = await fetch(`${getServerUrl()}/health`, { cache: "no-store" });
    if (!res.ok) {
      return { status: "error", error: `HTTP ${res.status}` };
    }
    const data = (await res.json()) as { status?: string };
    return { status: data.status === "ok" ? "ok" : "error" };
  } catch (error) {
    return {
      status: "error",
      error: error instanceof Error ? error.message : "Unknown error",
    };
  }
}

async function getDiskUsage(logDir: string): Promise<{
  used_bytes: number;
  max_bytes: number | null;
  percent: number | null;
}> {
  let usedBytes = 0;
  const files = await readdir(logDir);
  for (const file of files) {
    try {
      const info = await stat(join(logDir, file));
      if (info.isFile()) {
        usedBytes += info.size;
      }
    } catch {
      // ignore
    }
  }

  const maxMb = process.env.LOG_MAX_DISK_MB ? Number(process.env.LOG_MAX_DISK_MB) : null;
  const maxBytes = maxMb ? maxMb * 1024 * 1024 : null;
  const percent = maxBytes ? (usedBytes / maxBytes) * 100 : null;

  return { used_bytes: usedBytes, max_bytes: maxBytes, percent };
}

async function getRecentErrors(logDir: string): Promise<string[]> {
  try {
    const files = await readdir(logDir);
    const appLogFiles = files
      .filter((name) => name.endsWith(".jsonl") && !name.startsWith("requests.") && !name.startsWith("usage."))
      .sort()
      .reverse();

    const errors: string[] = [];
    for (const file of appLogFiles.slice(0, 2)) {
      const text = await readFile(join(logDir, file), "utf-8");
      const lines = parseJsonl<Record<string, unknown>>(text);
      for (const line of lines) {
        if (line.level === "ERROR" || line.level === "WARN") {
          errors.push(`${line.timestamp} [${line.level}] ${String(line.message ?? line.msg ?? "")}`);
          if (errors.length >= 10) break;
        }
      }
      if (errors.length >= 10) break;
    }
    return errors;
  } catch {
    return [];
  }
}

export async function GET() {
  try {
    const logDir = getLogDir();
    const [health, disk, recentErrors] = await Promise.all([
      fetchHealth(),
      getDiskUsage(logDir),
      getRecentErrors(logDir),
    ]);

    const status: StatusData = { health, disk, recentErrors };
    return NextResponse.json(status);
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    const status: StatusData = {
      health: { status: "error", error: message },
      disk: { used_bytes: 0, max_bytes: null, percent: null },
      recentErrors: [message],
    };
    return NextResponse.json(status, { status: 500 });
  }
}
