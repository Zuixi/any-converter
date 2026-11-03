import { NextResponse } from "next/server";

import type {
  Granularity,
  LedgerFilter,
  LedgerPage,
  SplitDim,
  UsageQuery,
  UsageSummary,
  UsageTotals,
} from "@any-converter/shared";

import {
  heatmapFromSqlite,
  ledgerCsv,
  ledgerPageFromSqlite,
  resolvePeriod,
  usageSummaryFromSqlite,
} from "../log-store";

export const runtime = "nodejs";

const DEFAULT_PRESET = "7d";
const DEFAULT_GRANULARITY: Granularity = "day";

interface ResolvedRange {
  from: string;
  to: string;
}

function getLogDir(): string {
  const dir = process.env.LOG_DIR;
  if (!dir) {
    throw new Error("LOG_DIR environment variable is not set");
  }
  return dir;
}

export async function GET(request: Request) {
  const params = new URL(request.url).searchParams;
  const granularity = granularityFrom(params.get("granularity"));
  const wantsLedger = params.get("ledger") === "1" || params.get("format") === "csv";
  const wantsHeatmap = params.get("heatmap") === "1";

  let range: ResolvedRange;
  try {
    // An explicit from/to pair wins over the preset: the hook layer works in
    // concrete ranges and must not silently fall back to a default window.
    const from = params.get("from");
    const to = params.get("to");
    range = from && to ? { from, to } : resolvePeriod(params.get("preset") ?? DEFAULT_PRESET);
  } catch (error) {
    return NextResponse.json({ error: errorMessage(error) }, { status: 400 });
  }

  try {
    const logDir = getLogDir();

    if (wantsHeatmap) {
      // The calendar has its own 53-week span, so the resolved range only has to
      // be a valid one for the request to reach this branch.
      const heatmap = heatmapFromSqlite(logDir, ledgerFilter(params, range));
      if (heatmap === null) {
        return NextResponse.json({ heatmap: null }, { status: 500 });
      }
      return NextResponse.json({ heatmap });
    }

    if (wantsLedger) {
      const page =
        ledgerPageFromSqlite(
          logDir,
          ledgerFilter(params, range),
          parseInteger(params.get("cursor")),
          parseInteger(params.get("limit")) ?? 0,
        ) ?? emptyLedgerPage();

      if (params.get("format") === "csv") {
        return new NextResponse(ledgerCsv(page.rows), {
          headers: { "content-type": "text/csv; charset=utf-8" },
        });
      }
      return NextResponse.json({ page });
    }

    const query: UsageQuery = {
      ...range,
      granularity,
      provider: optionalParam(params, "provider"),
      model: optionalParam(params, "model"),
      agent: optionalParam(params, "agent"),
      split_by: splitDimFrom(params.get("split_by")),
    };
    const summary = usageSummaryFromSqlite(logDir, query) ?? emptySummary(range, granularity);

    return NextResponse.json({ summary });
  } catch (error) {
    const message = errorMessage(error);
    if (wantsHeatmap) {
      return NextResponse.json({ heatmap: null, error: message }, { status: 500 });
    }
    if (wantsLedger) {
      return NextResponse.json({ page: emptyLedgerPage(), error: message }, { status: 500 });
    }
    return NextResponse.json(
      { summary: emptySummary(range, granularity), error: message },
      { status: 500 },
    );
  }
}

function ledgerFilter(params: URLSearchParams, range: ResolvedRange): LedgerFilter {
  const failed = params.get("failed");
  return {
    ...range,
    day: optionalParam(params, "day"),
    provider: optionalParam(params, "provider"),
    model: optionalParam(params, "model"),
    agent: optionalParam(params, "agent"),
    account: optionalParam(params, "account"),
    failed: failed === "1" || failed === "true",
    query: optionalParam(params, "query"),
  };
}

function granularityFrom(value: string | null): Granularity {
  switch (value) {
    case "hour":
    case "day":
    case "week":
    case "month":
      return value;
    default:
      return DEFAULT_GRANULARITY;
  }
}

function optionalParam(params: URLSearchParams, name: string): string | undefined {
  return params.get(name) ?? undefined;
}

/** `account` is accepted; the split then comes back without `by`, as the rollup has no account column. */
function splitDimFrom(value: string | null): SplitDim | undefined {
  switch (value) {
    case "provider":
    case "agent":
    case "model":
    case "account":
      return value;
    default:
      return undefined;
  }
}

function parseInteger(value: string | null): number | undefined {
  if (value === null || value.trim() === "") {
    return undefined;
  }
  const parsed = Number(value);
  return Number.isInteger(parsed) ? parsed : undefined;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Unknown error";
}

function emptySummary(range: ResolvedRange, granularity: Granularity): UsageSummary {
  return {
    from: range.from,
    to: range.to,
    granularity,
    totals: zeroTotals(),
    series: [],
    by_provider: [],
    by_model: [],
    by_agent: [],
    by_account: [],
    by_session: [],
  };
}

function emptyLedgerPage(): LedgerPage {
  return {
    rows: [],
    total: 0,
    totals: zeroTotals(),
  };
}

function zeroTotals(): UsageTotals {
  return {
    calls: 0,
    errors: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cache_write_1h: 0,
    cache_unknown_tokens: 0,
    reasoning_tokens: 0,
    latency_sum_ms: 0,
    latency_max_ms: 0,
    ttft_sum_ms: 0,
    ttft_calls: 0,
    cost_usd: 0,
    unpriced: 0,
    retried: 0,
    loops: 0,
    hit_rate: null,
    mean_latency_ms: null,
    mean_ttft_ms: null,
  };
}
