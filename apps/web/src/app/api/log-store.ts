import Database from "better-sqlite3";
import { join } from "node:path";

import type { AggregatedUsage, RequestLogRecord } from "@any-converter/shared";

interface RequestLogRow {
  record_json: string;
}

interface UsageRow {
  timestamp: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  request_count: number;
  status: number;
  avg_latency_ms: number;
  max_latency_ms: number;
  error_count: number;
  provider: string;
  client_model: string;
}

export function readRequestLogsFromSqlite(logDir: string, limit: number): RequestLogRecord[] | null {
  return withReadonlyDatabase(logDir, (db) => {
    const rows = db
      .prepare(
        `
        select record_json
        from request_logs
        order by timestamp desc, id desc
        limit ?
        `,
      )
      .all(limit) as RequestLogRow[];

    return rows.map((row) => JSON.parse(row.record_json) as RequestLogRecord);
  });
}

export function readUsageFromSqlite(logDir: string, limit: number): AggregatedUsage[] | null {
  return withReadonlyDatabase(logDir, (db) => {
    const rows = db
      .prepare(
        `
        select
          hour as timestamp,
          input_tokens,
          output_tokens,
          total_tokens,
          request_count,
          status,
          avg_latency_ms,
          max_latency_ms,
          error_count,
          provider,
          client_model
        from (
          select
            strftime('%Y-%m-%dT%H:00:00Z', timestamp) as hour,
            sum(input_tokens) as input_tokens,
            sum(output_tokens) as output_tokens,
            sum(total_tokens) as total_tokens,
            count(*) as request_count,
            max(response_status) as status,
            cast(round(avg(latency_ms)) as integer) as avg_latency_ms,
            max(latency_ms) as max_latency_ms,
            sum(case when response_status >= 400 then 1 else 0 end) as error_count,
            min(provider) as provider,
            min(client_model) as client_model
          from request_logs
          group by hour
          order by hour desc
          limit ?
        )
        order by timestamp asc
        `,
      )
      .all(limit) as UsageRow[];

    return rows.map((row) => ({
      timestamp: row.timestamp,
      input_tokens: row.input_tokens,
      output_tokens: row.output_tokens,
      total_tokens: row.total_tokens,
      request_count: row.request_count,
      status: row.status,
      latency_ms: row.avg_latency_ms,
      avg_latency_ms: row.avg_latency_ms,
      max_latency_ms: row.max_latency_ms,
      error_count: row.error_count,
      provider: row.provider,
      client_model: row.client_model,
    }));
  });
}

function withReadonlyDatabase<T>(logDir: string, read: (db: Database.Database) => T): T | null {
  let db: Database.Database | undefined;
  try {
    db = new Database(join(logDir, "any-converter.sqlite3"), {
      readonly: true,
      fileMustExist: true,
    });
    return read(db);
  } catch (error) {
    console.warn("sqlite log read failed, falling back to JSONL", error);
    return null;
  } finally {
    db?.close();
  }
}
