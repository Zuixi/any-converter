import Database from "better-sqlite3";
import { join } from "node:path";

import type {
  Granularity,
  Heatmap,
  LedgerFilter,
  LedgerPage,
  LedgerRow,
  RequestLogRecord,
  SplitDim,
  UsageGroup,
  UsageQuery,
  UsageSummary,
  UsageTotals,
} from "@any-converter/shared";

interface RequestLogRow {
  record_json: string;
}

interface RollupRow {
  bucket: string;
  provider: string;
  client_model: string;
  agent: string;
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
  /** Rows of this group whose model had a price when they were written. */
  priced_calls: number;
  retried_calls: number;
  loops: number;
}

type AggregateColumns = Omit<RollupRow, "bucket" | "provider" | "client_model" | "agent">;

type DimensionRow = AggregateColumns & {
  id: string;
  provider: string | null;
  model: string | null;
};

/** One (split value, bucket) group: the rollup's shape with a value column. */
type SplitRow = AggregateColumns & {
  value: string;
  bucket: string;
};

interface LedgerSqlRow {
  id: number;
  request_id: string;
  timestamp: string;
  provider: string;
  client_model: string;
  upstream_model: string;
  served_model: string | null;
  agent: string | null;
  account: string | null;
  session_id: string | null;
  status: number;
  attempt: number;
  streaming: number;
  latency_ms: number;
  ttft_ms: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  cost_usd: number | null;
}

interface LedgerTotalsRow {
  errors: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  cache_write_1h: number | null;
  cache_unknown_tokens: number | null;
  reasoning_tokens: number | null;
  latency_sum_ms: number | null;
  latency_max_ms: number | null;
  ttft_sum_ms: number | null;
  ttft_calls: number | null;
  cost_usd: number | null;
  priced_calls: number | null;
  retried: number | null;
  loops: number | null;
}

interface HeatRow {
  day: string;
  calls: number;
  tokens: number;
  cost_usd: number;
  unpriced: number;
}

type RawTotals = Omit<
  UsageTotals,
  "hit_rate" | "mean_latency_ms" | "mean_ttft_ms" | "retried" | "loops"
> & {
  retried: number;
  loops: number;
};

interface GroupAccumulator {
  id: string;
  provider?: string;
  model?: string;
  totals: RawTotals;
}

type SqlValue = string | number | null;

const DEFAULT_LEDGER_LIMIT = 100;
const MAX_LEDGER_LIMIT = 500;
/** How many split values one series point keeps; matches `SERIES_KEEP` in `storage/query.rs`. */
const SERIES_KEEP = 24;
/** Width of the heatmap calendar: 52 whole weeks behind this one, plus today's column. */
const HEATMAP_WEEKS = 53;

const LEDGER_CSV_HEADER =
  "timestamp,provider,client_model,upstream_model,served_model,agent,account," +
  "session_id,status,streaming,latency_ms,ttft_ms,input_tokens,output_tokens," +
  "cache_read_tokens,cache_write_tokens,reasoning_tokens,cost_usd";

const LEDGER_TOTALS_AGGREGATES = `
  sum(case when response_status >= 400 then 1 else 0 end) as errors,
  sum(input_tokens) as input_tokens,
  sum(output_tokens) as output_tokens,
  sum(coalesce(cache_read_tokens,0)) as cache_read_tokens,
  sum(coalesce(cache_write_tokens,0)) as cache_write_tokens,
  sum(cache_write_1h) as cache_write_1h,
  sum(case when cache_unknown = 1
           then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
           else 0 end) as cache_unknown_tokens,
  sum(coalesce(reasoning_tokens,0)) as reasoning_tokens,
  sum(latency_ms) as latency_sum_ms,
  max(latency_ms) as latency_max_ms,
  sum(ttft_ms) as ttft_sum_ms,
  sum(case when ttft_ms > 0 then 1 else 0 end) as ttft_calls,
  sum(coalesce(cost_usd,0)) as cost_usd,
  count(cost_usd) as priced_calls,
  sum(case when attempt > 0 and response_status < 400 then 1 else 0 end) as retried,
  sum(case when err_type = 'loop' then 1 else 0 end) as loops
`;

const DIMENSION_TOTALS_AGGREGATES = `
  sum(case when response_status >= 400 then 1 else 0 end) as errors,
  sum(input_tokens) as input_tokens,
  sum(output_tokens) as output_tokens,
  sum(coalesce(cache_read_tokens,0)) as cache_read_tokens,
  sum(coalesce(cache_write_tokens,0)) as cache_write_tokens,
  sum(cache_write_1h) as cache_write_1h,
  sum(case when cache_unknown = 1
           then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
           else 0 end) as cache_unknown_tokens,
  sum(coalesce(reasoning_tokens,0)) as reasoning_tokens,
  sum(latency_ms) as latency_sum_ms,
  max(latency_ms) as latency_max_ms,
  sum(ttft_ms) as ttft_sum_ms,
  sum(case when ttft_ms > 0 then 1 else 0 end) as ttft_calls,
  sum(coalesce(cost_usd,0)) as cost_usd,
  count(cost_usd) as priced_calls,
  sum(case when attempt > 0 and response_status < 400 then 1 else 0 end) as retried_calls,
  sum(case when err_type = 'loop' then 1 else 0 end) as loops
`;

export function readRequestLogsFromSqlite(logDir: string, limit: number): RequestLogRecord[] | null {
  return withReadonlyDatabase(logDir, (db) => {
    const rows = db
      .prepare(
        `
        select b.record_json
        from request_logs l
        join request_bodies b on b.request_id = l.request_id
        order by l.timestamp desc, l.id desc
        limit ?
        `,
      )
      .all(limit) as RequestLogRow[];

    return rows.map((row) => JSON.parse(row.record_json) as RequestLogRecord);
  });
}

/**
 * Folds `rollup_hourly` into the same buckets as the Rust `usage_summary`.
 *
 * The rollup is one row per (hour, provider, model, agent). A range bound inside
 * an hour is rounded down to that hour because the rollup cannot describe a
 * partial one; the echoed `from`/`to` stay exactly as the caller asked.
 */
export function usageSummaryFromSqlite(logDir: string, q: UsageQuery): UsageSummary | null {
  return withReadonlyDatabase(logDir, (db) => {
    const granularity = normalizeGranularity(q.granularity);
    // `rollup_hourly` has no account dimension, so an account filter reads the
    // detail rows instead: folding a whole rollup row would include every
    // account that shared the bucket.
    const rows =
      q.account === undefined
        ? (db
            .prepare(
              `
              select
                ${bucketExpression(granularity)} as bucket,
                provider,
                client_model,
                agent,
                sum(calls) as calls,
                sum(errors) as errors,
                sum(input_tokens) as input_tokens,
                sum(output_tokens) as output_tokens,
                sum(cache_read_tokens) as cache_read_tokens,
                sum(cache_write_tokens) as cache_write_tokens,
                sum(cache_write_1h) as cache_write_1h,
                sum(cache_unknown_tokens) as cache_unknown_tokens,
                sum(reasoning_tokens) as reasoning_tokens,
                sum(latency_sum_ms) as latency_sum_ms,
                max(latency_max_ms) as latency_max_ms,
                sum(ttft_sum_ms) as ttft_sum_ms,
                sum(ttft_calls) as ttft_calls,
                sum(cost_usd) as cost_usd,
                sum(priced_calls) as priced_calls,
                sum(retried_calls) as retried_calls,
                sum(loops) as loops
              from rollup_hourly
              where bucket >= ? and bucket < ?
                and (? is null or provider = ?)
                and (? is null or client_model = ?)
                and (? is null or agent = ?)
              group by 1, 2, 3, 4
              `,
            )
            .all(
              hourFloor(q.from),
              q.to,
              q.provider ?? null,
              q.provider ?? null,
              q.model ?? null,
              q.model ?? null,
              q.agent ?? null,
              q.agent ?? null,
            ) as RollupRow[])
        : (db
            .prepare(
              `
              select
                ${bucketExpression(granularity, "timestamp")} as bucket,
                provider,
                client_model,
                coalesce(client_id,'') as agent,
                count(*) as calls,
                sum(case when response_status >= 400 then 1 else 0 end) as errors,
                sum(input_tokens) as input_tokens,
                sum(output_tokens) as output_tokens,
                sum(coalesce(cache_read_tokens,0)) as cache_read_tokens,
                sum(coalesce(cache_write_tokens,0)) as cache_write_tokens,
                sum(cache_write_1h) as cache_write_1h,
                sum(case when cache_unknown = 1
                         then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
                         else 0 end) as cache_unknown_tokens,
                sum(coalesce(reasoning_tokens,0)) as reasoning_tokens,
                sum(latency_ms) as latency_sum_ms,
                max(latency_ms) as latency_max_ms,
                sum(ttft_ms) as ttft_sum_ms,
                sum(case when ttft_ms > 0 then 1 else 0 end) as ttft_calls,
                sum(coalesce(cost_usd,0)) as cost_usd,
                count(cost_usd) as priced_calls,
                sum(case when attempt > 0 and response_status < 400 then 1 else 0 end) as retried_calls,
                sum(case when err_type = 'loop' then 1 else 0 end) as loops
              from request_logs
              where timestamp >= ? and timestamp < ?
                and (? is null or provider = ?)
                and (? is null or client_model = ?)
                and (? is null or coalesce(client_id,'') = ?)
                and provider_account = ?
              group by 1, 2, 3, 4
              `,
            )
            .all(
              q.from,
              q.to,
              q.provider ?? null,
              q.provider ?? null,
              q.model ?? null,
              q.model ?? null,
              q.agent ?? null,
              q.agent ?? null,
              q.account,
            ) as RollupRow[]);

    const totals = emptyTotals();
    const series = new Map<string, RawTotals>();
    const byProvider = new Map<string, GroupAccumulator>();
    const byModel = new Map<string, GroupAccumulator>();
    const byAgent = new Map<string, GroupAccumulator>();

    for (const row of rows) {
      const rowTotals = rawTotalsFromRollup(row);
      addTotals(totals, rowTotals);
      addTotals(bucketFor(series, row.bucket), rowTotals);

      const providerGroup = groupFor(byProvider, row.provider);
      providerGroup.provider = row.provider;
      addTotals(providerGroup.totals, rowTotals);

      let modelGroup = byModel.get(row.client_model);
      if (!modelGroup) {
        modelGroup = {
          id: row.client_model,
          provider: row.provider,
          model: row.client_model,
          totals: emptyTotals(),
        };
        byModel.set(row.client_model, modelGroup);
      } else if (modelGroup.provider !== row.provider) {
        // A model served by several providers has no single provider to name.
        modelGroup.provider = undefined;
      }
      addTotals(modelGroup.totals, rowTotals);

      addTotals(groupFor(byAgent, row.agent).totals, rowTotals);
    }

    // Only a caller that asked for a stack pays for the second grouping.
    const split = q.split_by === undefined ? null : seriesSplit(db, q, granularity, q.split_by);

    return {
      from: q.from,
      to: q.to,
      granularity,
      totals: finalizeTotals(totals),
      series: Array.from(series.entries())
        .sort(([left], [right]) => compareText(left, right))
        .map(([timestamp, bucketTotals]) => ({
          label: timestamp,
          timestamp,
          totals: finalizeTotals(bucketTotals),
          ...(split === null ? {} : { by: split.get(timestamp) ?? {} }),
        })),
      by_provider: sortedGroups(byProvider),
      by_model: sortedGroups(byModel),
      by_agent: sortedGroups(byAgent),
      by_account: accountGroups(db, q),
      by_session: sessionGroups(db, q),
    };
  });
}

/**
 * The optional per-point split: bucket → value → that value's totals at it.
 *
 * Mirrors `series_split` in `crates/server/src/storage.rs`: the busiest
 * `SERIES_KEEP` values over the whole range survive, because ranking per bucket
 * would let one spike outrank a steady line. A value with no name is left out so
 * it stays in the point's own totals. `null` when the dimension cannot be split
 * (`account` is not on the rollup) or when nothing survived.
 */
function seriesSplit(
  db: Database.Database,
  q: UsageQuery,
  granularity: Granularity,
  dim: SplitDim,
): Map<string, Record<string, UsageTotals>> | null {
  const column = splitColumn(dim);
  if (column === null) {
    return null;
  }

  // An account filter reads `request_logs` in the summary (the rollup has no
  // account column), so the split must read the same rows or the selected
  // account would inherit every other account's traffic in the same bucket.
  const rows =
    q.account === undefined
      ? (db
          .prepare(
            `
            select
              ${column} as value,
              ${bucketExpression(granularity)} as bucket,
              sum(calls) as calls,
              sum(errors) as errors,
              sum(input_tokens) as input_tokens,
              sum(output_tokens) as output_tokens,
              sum(cache_read_tokens) as cache_read_tokens,
              sum(cache_write_tokens) as cache_write_tokens,
              sum(cache_write_1h) as cache_write_1h,
              sum(cache_unknown_tokens) as cache_unknown_tokens,
              sum(reasoning_tokens) as reasoning_tokens,
              sum(latency_sum_ms) as latency_sum_ms,
              max(latency_max_ms) as latency_max_ms,
              sum(ttft_sum_ms) as ttft_sum_ms,
              sum(ttft_calls) as ttft_calls,
              sum(cost_usd) as cost_usd,
              sum(priced_calls) as priced_calls,
              sum(retried_calls) as retried_calls,
              sum(loops) as loops
            from rollup_hourly
            where bucket >= ? and bucket < ?
              and (? is null or provider = ?)
              and (? is null or client_model = ?)
              and (? is null or agent = ?)
            group by 1, 2
            `,
          )
          .all(
            // The hour floor the main query applies, or a partial first hour
            // would be in `totals` but missing from `by`.
            hourFloor(q.from),
            q.to,
            q.provider ?? null,
            q.provider ?? null,
            q.model ?? null,
            q.model ?? null,
            q.agent ?? null,
            q.agent ?? null,
          ) as SplitRow[])
      : (db
          .prepare(
            `
            select
              ${column} as value,
              ${bucketExpression(granularity, "timestamp")} as bucket,
              count(*) as calls,
              sum(case when response_status >= 400 then 1 else 0 end) as errors,
              sum(input_tokens) as input_tokens,
              sum(output_tokens) as output_tokens,
              sum(coalesce(cache_read_tokens,0)) as cache_read_tokens,
              sum(coalesce(cache_write_tokens,0)) as cache_write_tokens,
              sum(cache_write_1h) as cache_write_1h,
              sum(case when cache_unknown = 1
                       then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
                       else 0 end) as cache_unknown_tokens,
              sum(coalesce(reasoning_tokens,0)) as reasoning_tokens,
              sum(latency_ms) as latency_sum_ms,
              max(latency_ms) as latency_max_ms,
              sum(ttft_ms) as ttft_sum_ms,
              sum(case when ttft_ms > 0 then 1 else 0 end) as ttft_calls,
              sum(coalesce(cost_usd,0)) as cost_usd,
              count(cost_usd) as priced_calls,
              sum(case when attempt > 0 and response_status < 400 then 1 else 0 end) as retried_calls,
              sum(case when err_type = 'loop' then 1 else 0 end) as loops
            from request_logs
            where timestamp >= ? and timestamp < ?
              and (? is null or provider = ?)
              and (? is null or client_model = ?)
              and (? is null or coalesce(client_id,'') = ?)
              and provider_account = ?
            group by 1, 2
            `,
          )
          .all(
            q.from,
            q.to,
            q.provider ?? null,
            q.provider ?? null,
            q.model ?? null,
            q.model ?? null,
            q.agent ?? null,
            q.agent ?? null,
            q.account,
          ) as SplitRow[]);

  const byBucket = new Map<string, Map<string, UsageTotals>>();
  const weight = new Map<string, number>();
  for (const row of rows) {
    // A value with no name has no line to draw, so it stays in the point total.
    if (row.value === "") {
      continue;
    }
    const rowTotals = finalizeTotals(rawTotalsFromAggregate(row));
    weight.set(row.value, (weight.get(row.value) ?? 0) + rowTotals.input_tokens + rowTotals.output_tokens);
    const values = bucketValuesFor(byBucket, row.bucket);
    values.set(row.value, rowTotals);
  }

  // Rank once over the whole range: a value that is busy in one bucket only is
  // still one line, so it must not outrank a steady one by a single spike.
  const kept = new Set(
    Array.from(weight.entries())
      .sort(
        ([leftValue, leftWeight], [rightValue, rightWeight]) =>
          rightWeight - leftWeight || compareText(leftValue, rightValue),
      )
      .slice(0, SERIES_KEEP)
      .map(([value]) => value),
  );
  if (kept.size === 0) {
    return null;
  }

  const split = new Map<string, Record<string, UsageTotals>>();
  for (const [bucket, values] of byBucket) {
    const filtered: Record<string, UsageTotals> = {};
    for (const [value, totals] of values) {
      if (kept.has(value)) {
        filtered[value] = totals;
      }
    }
    if (Object.keys(filtered).length > 0) {
      split.set(bucket, filtered);
    }
  }
  return split;
}

/** The `rollup_hourly` column behind a split dimension, or null when it has none. */
function splitColumn(dim: SplitDim): string | null {
  switch (dim) {
    case "provider":
      return "provider";
    case "agent":
      return "agent";
    case "model":
      return "client_model";
    // An account is a high-cardinality detail-only label: adding it to the
    // hourly rollup would turn a cache into a per-subscription table.
    case "account":
      return null;
  }
}

function bucketValuesFor(
  buckets: Map<string, Map<string, UsageTotals>>,
  key: string,
): Map<string, UsageTotals> {
  let values = buckets.get(key);
  if (!values) {
    values = new Map();
    buckets.set(key, values);
  }
  return values;
}

/**
 * The last 53 weeks of calls the filter keeps, a day each: one cell is one
 * `substr(bucket,1,10)` group of `rollup_hourly`.
 *
 * The span is the heatmap's own — `filter.from`/`filter.to` are ignored, because a
 * calendar whose first column moved with the ledger's range would stop being a
 * calendar. `provider`/`model`/`agent` still narrow the cells; `account` has no
 * column in the rollup, and `day`/`failed`/`query` only describe detail rows.
 */
export function heatmapFromSqlite(logDir: string, filter: LedgerFilter): Heatmap | null {
  return withReadonlyDatabase(logDir, (db) => {
    // One instant for both ends, so a clock crossing midnight cannot make the
    // window end on a day its own start never covered.
    const now = new Date();
    const today = startOfUtcDay(now);
    // 52 whole weeks behind this week's Monday, so the current week keeps its own
    // column and the calendar is exactly `HEATMAP_WEEKS` columns wide.
    const from = addMilliseconds(mondayOf(today), -(HEATMAP_WEEKS - 1) * 7 * DAY_MS);

    const rows = db
      .prepare(
        `
        select
          substr(bucket,1,10) as day,
          sum(calls) as calls,
          sum(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens) as tokens,
          sum(cost_usd) as cost_usd,
          sum(calls - priced_calls) as unpriced
        from rollup_hourly
        where bucket >= ? and bucket <= ?
          and (? is null or provider = ?)
          and (? is null or client_model = ?)
          and (? is null or agent = ?)
        group by 1
        having sum(calls) > 0
        order by 1 asc
        `,
      )
      .all(
        utcDayString(from),
        // `bucket` is `YYYY-MM-DDTHH:00:00Z`, so it sorts against a
        // second-precision instant; the hour in progress is included, later hours
        // of today are not.
        formatUtc(now),
        filter.provider ?? null,
        filter.provider ?? null,
        filter.model ?? null,
        filter.model ?? null,
        filter.agent ?? null,
        filter.agent ?? null,
      ) as HeatRow[];

    return {
      from: utcDayString(from),
      to: utcDayString(today),
      weeks: HEATMAP_WEEKS,
      days: rows.map((row) => ({
        date: row.day,
        calls: nonNegative(row.calls),
        tokens: nonNegative(row.tokens),
        cost_usd: row.cost_usd,
        unpriced: nonNegative(row.unpriced),
      })),
    };
  });
}

/**
 * One page of detail rows, newest first, with the filtered set's size and totals.
 *
 * Paging is keyset: `cursor` is the `id` of the previous page's last row, so a
 * write landing between two page loads cannot shift a row across the boundary.
 */
export function ledgerPageFromSqlite(
  logDir: string,
  filter: LedgerFilter,
  cursor: number | undefined,
  limit: number,
): LedgerPage | null {
  return withReadonlyDatabase(logDir, (db) => {
    const effectiveLimit = Math.min(
      Number.isFinite(limit) && limit > 0 ? Math.trunc(limit) : DEFAULT_LEDGER_LIMIT,
      MAX_LEDGER_LIMIT,
    );
    const { sql: predicate, params } = ledgerPredicate(filter);

    const rows = db
      .prepare(
        `
        select
          id, request_id, timestamp, provider, client_model, upstream_model,
          served_model, client_id as agent, provider_account as account, session_id,
          response_status as status, streaming, latency_ms, ttft_ms,
          input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
          reasoning_tokens, cost_usd, attempt
        from request_logs
        where ${predicate}
          and (? is null or id < ?)
        order by id desc
        limit ?
        `,
      )
      .all(...params, cursor ?? null, cursor ?? null, effectiveLimit) as LedgerSqlRow[];

    const countRow = db
      .prepare(`select count(*) as total from request_logs where ${predicate}`)
      .get(...params) as { total: number } | undefined;
    const total = countRow?.total ?? 0;

    const aggregate = db
      .prepare(`select ${LEDGER_TOTALS_AGGREGATES} from request_logs where ${predicate}`)
      .get(...params) as LedgerTotalsRow | undefined;

    const last = rows[rows.length - 1];
    return {
      rows: rows.map(ledgerRowFromSql),
      total,
      next_cursor: rows.length === effectiveLimit && last ? last.id : undefined,
      totals: finalizeTotals(rawTotalsFromLedger(aggregate, total)),
    };
  });
}

/** RFC 4180 CSV of the ledger rows, header included. */
export function ledgerCsv(rows: LedgerRow[]): string {
  const lines = [LEDGER_CSV_HEADER];
  for (const row of rows) {
    lines.push(
      [
        row.timestamp,
        row.provider,
        row.client_model,
        row.upstream_model,
        row.served_model ?? "",
        row.agent ?? "",
        row.account ?? "",
        row.session_id ?? "",
        String(row.status),
        row.streaming ? "true" : "false",
        String(row.latency_ms),
        String(row.ttft_ms),
        String(row.input_tokens),
        String(row.output_tokens),
        String(row.cache_read_tokens),
        String(row.cache_write_tokens),
        String(row.reasoning_tokens),
        row.cost_usd === undefined ? "" : row.cost_usd.toFixed(6),
      ]
        .map(csvField)
        .join(","),
    );
  }
  return lines.join("\n");
}

/**
 * Resolves a preset, a rolling window, or a picked day range into UTC bounds.
 *
 * Mirrors `crates/server/src/period.rs`:
 * - `today`  : this UTC day, midnight to midnight
 * - `<n>d`   : the last `n` days, ending now, for any `n` from 1 to 3650
 *   (so `1d`, `3d`, `7d`, and `30d` are the same kind of window)
 * - `all`    : from 1970-01-01T00:00:00Z to now
 * - `last:24h`, `last:90m`, `last:2d` : that long before now
 * - `2026-01-01..2026-01-31` : both days included, so `to` is the midnight after the last
 */
export function resolvePeriod(period: string, now: Date = new Date()): { from: string; to: string } {
  if (period === "today") {
    const start = startOfUtcDay(now);
    return { from: formatUtc(start), to: formatUtc(addMilliseconds(start, DAY_MS)) };
  }
  if (period === "all") {
    return { from: formatUtc(new Date(0)), to: formatUtc(now) };
  }
  if (period.startsWith("last:")) {
    return rollingWindow(now, lastDuration(period), period);
  }
  if (period.includes("..")) {
    return resolveDays(period);
  }
  const days = dayPreset(period);
  if (days === null) {
    throw new Error(`unknown period: ${period}`);
  }
  return rollingWindow(now, days * DAY_MS, period);
}

/** `<n>d` as whole days in `1..=MAX_PRESET_DAYS`, or null when the period is not one. */
function dayPreset(period: string): number | null {
  const match = /^(\d+)d$/.exec(period);
  if (!match) {
    return null;
  }
  const days = Number(match[1]);
  if (!Number.isSafeInteger(days) || days < 1 || days > MAX_PRESET_DAYS) {
    throw new Error(`invalid range: ${period}`);
  }
  return days;
}

function bucketExpression(granularity: Granularity, column = "bucket"): string {
  switch (granularity) {
    case "hour":
      return `substr(${column},1,13) || ':00:00Z'`;
    case "day":
      return `substr(${column},1,10)`;
    case "week":
      return `date(${column},'weekday 1','-7 days')`;
    case "month":
      return `substr(${column},1,7)`;
  }
}

function hourFloor(timestamp: string): string {
  return timestamp.length >= 13 ? `${timestamp.slice(0, 13)}:00:00Z` : timestamp;
}

function normalizeGranularity(value: Granularity | undefined): Granularity {
  return value === "day" || value === "week" || value === "month" || value === "hour"
    ? value
    : "hour";
}

function rawTotalsFromRollup(row: RollupRow): RawTotals {
  return rawTotalsFromAggregate(row);
}

function rawTotalsFromAggregate(row: AggregateColumns): RawTotals {
  return {
    calls: row.calls,
    errors: row.errors,
    input_tokens: row.input_tokens,
    output_tokens: row.output_tokens,
    cache_read_tokens: row.cache_read_tokens,
    cache_write_tokens: row.cache_write_tokens,
    cache_write_1h: row.cache_write_1h,
    cache_unknown_tokens: row.cache_unknown_tokens,
    reasoning_tokens: row.reasoning_tokens,
    latency_sum_ms: row.latency_sum_ms,
    latency_max_ms: row.latency_max_ms,
    ttft_sum_ms: row.ttft_sum_ms,
    ttft_calls: row.ttft_calls,
    cost_usd: row.cost_usd,
    // `cost_usd` covers only the priced calls, so the rest are reported apart
    // instead of silently reading as free traffic.
    unpriced: Math.max(0, row.calls - row.priced_calls),
    retried: row.retried_calls,
    loops: row.loops,
  };
}

function rawTotalsFromLedger(row: LedgerTotalsRow | undefined, calls: number): RawTotals {
  return {
    calls,
    errors: nonNegative(row?.errors),
    input_tokens: nonNegative(row?.input_tokens),
    output_tokens: nonNegative(row?.output_tokens),
    cache_read_tokens: nonNegative(row?.cache_read_tokens),
    cache_write_tokens: nonNegative(row?.cache_write_tokens),
    cache_write_1h: nonNegative(row?.cache_write_1h),
    cache_unknown_tokens: nonNegative(row?.cache_unknown_tokens),
    reasoning_tokens: nonNegative(row?.reasoning_tokens),
    latency_sum_ms: nonNegative(row?.latency_sum_ms),
    latency_max_ms: nonNegative(row?.latency_max_ms),
    ttft_sum_ms: nonNegative(row?.ttft_sum_ms),
    ttft_calls: nonNegative(row?.ttft_calls),
    cost_usd: row?.cost_usd ?? 0,
    // The ledger counts the rows that carry no price against its own call count.
    unpriced: Math.max(0, calls - nonNegative(row?.priced_calls)),
    retried: nonNegative(row?.retried),
    loops: nonNegative(row?.loops),
  };
}

function emptyTotals(): RawTotals {
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
  };
}

function addTotals(into: RawTotals, from: RawTotals): void {
  into.calls += from.calls;
  into.errors += from.errors;
  into.input_tokens += from.input_tokens;
  into.output_tokens += from.output_tokens;
  into.cache_read_tokens += from.cache_read_tokens;
  into.cache_write_tokens += from.cache_write_tokens;
  into.cache_write_1h += from.cache_write_1h;
  into.cache_unknown_tokens += from.cache_unknown_tokens;
  into.reasoning_tokens += from.reasoning_tokens;
  into.latency_sum_ms += from.latency_sum_ms;
  into.latency_max_ms = Math.max(into.latency_max_ms, from.latency_max_ms);
  into.ttft_sum_ms += from.ttft_sum_ms;
  into.ttft_calls += from.ttft_calls;
  into.cost_usd += from.cost_usd;
  into.unpriced += from.unpriced;
  into.retried += from.retried;
  into.loops += from.loops;
}

function finalizeTotals(raw: RawTotals): UsageTotals {
  const prompt =
    raw.input_tokens + raw.cache_read_tokens + raw.cache_write_tokens - raw.cache_unknown_tokens;
  return {
    ...raw,
    hit_rate: prompt > 0 ? raw.cache_read_tokens / prompt : null,
    mean_latency_ms: raw.calls > 0 ? Math.floor(raw.latency_sum_ms / raw.calls) : null,
    mean_ttft_ms: raw.ttft_calls > 0 ? Math.floor(raw.ttft_sum_ms / raw.ttft_calls) : null,
  };
}

function bucketFor(buckets: Map<string, RawTotals>, key: string): RawTotals {
  let totals = buckets.get(key);
  if (!totals) {
    totals = emptyTotals();
    buckets.set(key, totals);
  }
  return totals;
}

function groupFor(groups: Map<string, GroupAccumulator>, key: string): GroupAccumulator {
  let group = groups.get(key);
  if (!group) {
    group = { id: key, totals: emptyTotals() };
    groups.set(key, group);
  }
  return group;
}

function sortedGroups(groups: Map<string, GroupAccumulator>): UsageGroup[] {
  return Array.from(groups.values())
    .sort(
      (left, right) =>
        right.totals.calls - left.totals.calls || compareText(left.id, right.id),
    )
    .map((group) => ({
      id: group.id,
      provider: group.provider,
      model: group.model,
      totals: finalizeTotals(group.totals),
    }));
}

/** Accounts rank from `request_logs`: the rollup has no account dimension. */
function accountGroups(db: Database.Database, q: UsageQuery): UsageGroup[] {
  const rows = db
    .prepare(
      `
      select coalesce(provider_account,'') as id, provider, null as model, count(*) as calls,
        ${DIMENSION_TOTALS_AGGREGATES}
      from request_logs
      where timestamp >= ? and timestamp < ?
        and (? is null or provider = ?)
        and (? is null or client_model = ?)
        and (? is null or coalesce(client_id,'') = ?)
        and provider_account is not null and provider_account <> ''
        and (? is null or provider_account = ?)
      group by 1, 2
      order by sum(input_tokens + output_tokens) desc, 1 asc, 2 asc
      limit 50
      `,
    )
    .all(
      q.from,
      q.to,
      q.provider ?? null,
      q.provider ?? null,
      q.model ?? null,
      q.model ?? null,
      q.agent ?? null,
      q.agent ?? null,
      q.account ?? null,
      q.account ?? null,
    ) as DimensionRow[];
  return dimensionGroups(rows);
}

/** Sessions rank from `request_logs`; they are far too many for the rollup. */
function sessionGroups(db: Database.Database, q: UsageQuery): UsageGroup[] {
  const rows = db
    .prepare(
      `
      select session_id as id, coalesce(client_id,'') as model, null as provider, count(*) as calls,
        ${DIMENSION_TOTALS_AGGREGATES}
      from request_logs
      where timestamp >= ? and timestamp < ?
        and (? is null or provider = ?)
        and (? is null or client_model = ?)
        and (? is null or coalesce(client_id,'') = ?)
        and session_id is not null and session_id <> ''
        and (? is null or provider_account = ?)
      group by 1, 2
      order by sum(input_tokens + output_tokens) desc, 1 asc, 2 asc
      limit 50
      `,
    )
    .all(
      q.from,
      q.to,
      q.provider ?? null,
      q.provider ?? null,
      q.model ?? null,
      q.model ?? null,
      q.agent ?? null,
      q.agent ?? null,
      q.account ?? null,
      q.account ?? null,
    ) as DimensionRow[];
  return dimensionGroups(rows);
}

function dimensionGroups(rows: DimensionRow[]): UsageGroup[] {
  return rows.map((row) => ({
    id: row.id,
    provider: row.provider ?? undefined,
    model: row.model ?? undefined,
    totals: finalizeTotals(rawTotalsFromAggregate(row)),
  }));
}

function ledgerPredicate(filter: LedgerFilter): { sql: string; params: SqlValue[] } {
  const clauses = ["timestamp >= ?", "timestamp < ?"];
  const params: SqlValue[] = [filter.from, filter.to];

  if (filter.day !== undefined) {
    clauses.push("substr(timestamp,1,10) = ?");
    params.push(filter.day);
  }
  for (const [column, value] of [
    ["provider", filter.provider],
    ["client_model", filter.model],
    ["client_id", filter.agent],
    ["provider_account", filter.account],
  ] as const) {
    if (value !== undefined) {
      clauses.push(`${column} = ?`);
      params.push(value);
    }
  }
  if (filter.failed) {
    clauses.push("response_status >= 400");
  }
  if (filter.retried) {
    clauses.push("attempt > 0");
  }
  if (filter.query !== undefined) {
    clauses.push(
      "(lower(client_model) like ? or lower(provider) like ? or " +
        "lower(coalesce(served_model,'')) like ? or lower(coalesce(session_id,'')) like ?)",
    );
    const needle = `%${filter.query.toLowerCase()}%`;
    params.push(needle, needle, needle, needle);
  }

  return { sql: clauses.join(" and "), params };
}

function ledgerRowFromSql(row: LedgerSqlRow): LedgerRow {
  return {
    request_id: row.request_id,
    timestamp: row.timestamp,
    provider: row.provider,
    client_model: row.client_model,
    upstream_model: row.upstream_model,
    served_model: row.served_model ?? undefined,
    agent: row.agent ?? undefined,
    account: row.account ?? undefined,
    session_id: row.session_id ?? undefined,
    status: row.status,
    attempt: nonNegative(row.attempt),
    streaming: row.streaming !== 0,
    latency_ms: nonNegative(row.latency_ms),
    ttft_ms: nonNegative(row.ttft_ms),
    input_tokens: nonNegative(row.input_tokens),
    output_tokens: nonNegative(row.output_tokens),
    cache_read_tokens: nonNegative(row.cache_read_tokens),
    cache_write_tokens: nonNegative(row.cache_write_tokens),
    reasoning_tokens: nonNegative(row.reasoning_tokens),
    cost_usd: row.cost_usd ?? undefined,
  };
}

function nonNegative(value: number | null | undefined): number {
  return Math.max(0, Math.trunc(value ?? 0));
}

function csvField(field: string): string {
  if (!/[",\r\n]/.test(field)) {
    return field;
  }
  return `"${field.replace(/"/g, '""')}"`;
}

function compareText(left: string, right: string): number {
  if (left < right) {
    return -1;
  }
  return left > right ? 1 : 0;
}

const DAY_MS = 24 * 60 * 60 * 1000;
/** The widest accepted `<n>d` window; matches `MAX_PRESET_DAYS` in `period.rs`. */
const MAX_PRESET_DAYS = 3650;

function rollingWindow(now: Date, spanMs: number, period: string): { from: string; to: string } {
  const from = addMilliseconds(now, -spanMs);
  if (Number.isNaN(from.getTime())) {
    throw new Error(`invalid range: ${period}`);
  }
  return { from: formatUtc(from), to: formatUtc(now) };
}

function lastDuration(period: string): number {
  const match = /^last:([+-]?\d+)([mhd])$/.exec(period);
  if (!match) {
    throw new Error(`invalid range: ${period}`);
  }
  const amount = Number(match[1]);
  if (!Number.isSafeInteger(amount) || amount <= 0) {
    throw new Error(`invalid range: ${period}`);
  }
  const unit = match[2];
  const unitMs = unit === "m" ? 60_000 : unit === "h" ? 3_600_000 : DAY_MS;
  return amount * unitMs;
}

function resolveDays(period: string): { from: string; to: string } {
  const parts = period.split("..");
  if (parts.length !== 2) {
    throw new Error(`invalid range: ${period}`);
  }
  const first = parseUtcDay(parts[0], period);
  const last = parseUtcDay(parts[1], period);
  const from = utcDay(first.year, first.month, first.day);
  const lastDay = utcDay(last.year, last.month, last.day);
  if (from.getTime() > lastDay.getTime()) {
    throw new Error(`invalid range: ${period}`);
  }
  return { from: formatUtc(from), to: formatUtc(addMilliseconds(lastDay, DAY_MS)) };
}

function parseUtcDay(value: string | undefined, period: string): { year: number; month: number; day: number } {
  const match = value === undefined ? null : /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(value);
  if (!match) {
    throw new Error(`invalid range: ${period}`);
  }
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const parsed = utcDay(year, month, day);
  if (
    parsed.getUTCFullYear() !== year ||
    parsed.getUTCMonth() !== month - 1 ||
    parsed.getUTCDate() !== day
  ) {
    throw new Error(`invalid range: ${period}`);
  }
  return { year, month, day };
}

function utcDay(year: number, month: number, day: number): Date {
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  date.setUTCHours(0, 0, 0, 0);
  return date;
}

function startOfUtcDay(now: Date): Date {
  return utcDay(now.getUTCFullYear(), now.getUTCMonth() + 1, now.getUTCDate());
}

/** The Monday that starts `date`'s UTC week; a Sunday belongs to the week six days back. */
function mondayOf(date: Date): Date {
  const start = startOfUtcDay(date);
  // `getUTCDay()` is 0 for Sunday, which closes a Monday-first week.
  return addMilliseconds(start, -((start.getUTCDay() + 6) % 7) * DAY_MS);
}

/** `YYYY-MM-DD`: the width of one day group of `rollup_hourly`. */
function utcDayString(date: Date): string {
  return date.toISOString().slice(0, 10);
}

function addMilliseconds(date: Date, milliseconds: number): Date {
  return new Date(date.getTime() + milliseconds);
}

function formatUtc(date: Date): string {
  return `${date.toISOString().slice(0, 19)}Z`;
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
