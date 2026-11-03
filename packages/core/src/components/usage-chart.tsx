"use client";

import type { UsageBucket, UsageTotals } from "@any-converter/shared";
import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { Bar, BarChart, CartesianGrid, Legend, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

import { useI18n } from "../i18n";

export type UsageMetric = "tokens" | "cost" | "calls" | "latency";

export interface UsageChartProps {
  data: UsageBucket[];
  metric: UsageMetric;
  /** Asks for a stack; data without `by` still draws a single total bar. */
  stacked: boolean;
}

/** Ten hues, cycled when more split values survive than the palette has. */
const PALETTE = [
  "#2563eb",
  "#16a34a",
  "#f59e0b",
  "#dc2626",
  "#7c3aed",
  "#0891b2",
  "#db2777",
  "#65a30d",
  "#9333ea",
  "#ea580c",
] as const;

/** What a stack's tail folds into, so the bars still reach the point's total. */
const OTHER_COLOR = "#94a3b8";
const GRID_COLOR = "#e5e7eb";
const AXIS_COLOR = "#737373";
/** Below this, a gap is float noise rather than traffic the stack is missing. */
const GAP_EPSILON = 1e-9;

/**
 * The keys recharts plots. Split values become `s0`, `s1`, … because a value may
 * contain a dot, which `dataKey` would read as a path into a nested object.
 */
const TOTAL_KEY = "total";
const OTHER_KEY = "other";

const METRIC_LABEL_KEYS = {
  tokens: "usage.metric.tokens",
  cost: "usage.metric.cost",
  calls: "usage.metric.calls",
  latency: "usage.metric.latency",
} as const;

interface ChartRow {
  label: string;
  [key: string]: string | number;
}

interface ChartSeries {
  key: string;
  name: string;
  color: string;
}

interface ChartModel {
  rows: ChartRow[];
  series: ChartSeries[];
  stacked: boolean;
}

export function UsageChart({ data, metric, stacked }: UsageChartProps) {
  const { t } = useI18n();

  if (data.length === 0) {
    return (
      <div className="flex h-64 items-center justify-center rounded-md border text-muted-foreground">
        {t("usage.empty")}
      </div>
    );
  }

  const title = t(METRIC_LABEL_KEYS[metric]);
  const model = buildModel(data, metric, stacked, title, t("usage.other"));

  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="h-[320px] w-full">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={model.rows} margin={{ top: 8, right: 8, bottom: 0, left: 0 }} barCategoryGap="12%">
              <CartesianGrid strokeDasharray="3 3" stroke={GRID_COLOR} vertical={false} />
              <XAxis
                dataKey="label"
                tick={{ fontSize: 12, fill: AXIS_COLOR }}
                tickLine={false}
                axisLine={{ stroke: GRID_COLOR }}
                tickMargin={8}
                minTickGap={24}
                angle={0}
                interval="preserveStartEnd"
              />
              <YAxis
                width={64}
                tick={{ fontSize: 12, fill: AXIS_COLOR }}
                tickLine={false}
                axisLine={false}
                tickMargin={6}
                tickFormatter={(value: number | string) => formatTick(metric, numericValue(value))}
              />
              <Tooltip
                cursor={{ fill: "rgba(148,163,184,0.12)" }}
                formatter={(value, name) => [formatValue(metric, numericValue(value)), name]}
                contentStyle={{ borderRadius: 6, borderColor: GRID_COLOR, fontSize: 12 }}
              />
              {model.stacked && (
                <Legend iconType="square" iconSize={10} verticalAlign="bottom" wrapperStyle={{ fontSize: 12 }} />
              )}
              {model.series.map((item, index) => (
                <Bar
                  key={item.key}
                  dataKey={item.key}
                  name={item.name}
                  fill={item.color}
                  stackId={model.stacked ? "s" : undefined}
                  maxBarSize={72}
                  radius={model.stacked && index < model.series.length - 1 ? undefined : [3, 3, 0, 0]}
                />
              ))}
            </BarChart>
          </ResponsiveContainer>
        </div>
      </CardContent>
    </Card>
  );
}

/**
 * Turns the buckets into the rows recharts plots: one row per point, one series
 * per split value ranked by the tokens it moved over the whole range, and an
 * `other` series for whatever the kept values do not cover.
 */
function buildModel(
  data: UsageBucket[],
  metric: UsageMetric,
  requestedStack: boolean,
  totalName: string,
  otherName: string,
): ChartModel {
  const hasSplit = requestedStack && data.some((bucket) => Object.keys(bucket.by ?? {}).length > 0);

  if (!hasSplit) {
    return {
      rows: data.map((bucket) => ({ label: bucket.label, [TOTAL_KEY]: metricValue(bucket.totals, metric) })),
      series: [{ key: TOTAL_KEY, name: totalName, color: PALETTE[0] }],
      stacked: false,
    };
  }

  // Rank over the whole range: a value busy in one bucket only is still one
  // line, so it must not outrank a steady one by a single spike.
  const tokensByValue = new Map<string, number>();
  for (const bucket of data) {
    for (const [value, totals] of Object.entries(bucket.by ?? {})) {
      tokensByValue.set(value, (tokensByValue.get(value) ?? 0) + metricValue(totals, "tokens"));
    }
  }
  const values = Array.from(tokensByValue.entries())
    .sort(
      ([leftName, leftTokens], [rightName, rightTokens]) =>
        rightTokens - leftTokens || compareText(leftName, rightName),
    )
    .map(([value]) => value);

  const points = data.map((bucket) => {
    const parts = values.map((value) => {
      const totals = bucket.by?.[value];
      return totals === undefined ? 0 : metricValue(totals, metric);
    });
    return {
      label: bucket.label,
      total: metricValue(bucket.totals, metric),
      parts,
      covered: parts.reduce((sum, part) => sum + part, 0),
    };
  });
  // The server keeps only the busiest values; the rest still happened, so the
  // stack needs a grey tail to add up to the point.
  const hasOther = points.some((point) => point.total - point.covered > GAP_EPSILON);

  const rows = points.map((point) => {
    const row: ChartRow = { label: point.label };
    point.parts.forEach((part, seriesIndex) => {
      row[`s${seriesIndex}`] = part;
    });
    if (hasOther) {
      row[OTHER_KEY] = Math.max(0, point.total - point.covered);
    }
    return row;
  });

  const series: ChartSeries[] = values.map((value, index) => ({
    key: `s${index}`,
    name: value,
    color: PALETTE[index % PALETTE.length] ?? PALETTE[0],
  }));
  if (hasOther) {
    series.push({ key: OTHER_KEY, name: otherName, color: OTHER_COLOR });
  }

  return { rows, series, stacked: true };
}

function metricValue(totals: UsageTotals, metric: UsageMetric): number {
  switch (metric) {
    case "tokens":
      return totals.input_tokens + totals.output_tokens + totals.cache_read_tokens + totals.cache_write_tokens;
    case "cost":
      return totals.cost_usd;
    case "calls":
      return totals.calls;
    case "latency":
      return totals.mean_latency_ms ?? 0;
  }
}

function numericValue(value: unknown): number {
  if (typeof value === "number") {
    return Number.isFinite(value) ? value : 0;
  }
  if (typeof value === "string") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

function compareText(left: string, right: string): number {
  if (left < right) {
    return -1;
  }
  return left > right ? 1 : 0;
}

/** Axis ticks: compact enough for `1.2k`/`3.4M`, precise enough for small costs. */
function formatTick(metric: UsageMetric, value: number): string {
  switch (metric) {
    case "cost":
      return formatCost(value);
    case "latency":
      return `${formatCompact(value)}ms`;
    default:
      return formatCompact(value);
  }
}

function formatCompact(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1_000_000) {
    return `${formatFraction(value / 1_000_000)}M`;
  }
  if (abs >= 1_000) {
    return `${formatFraction(value / 1_000)}k`;
  }
  if (abs > 0 && abs < 0.01) {
    return value.toPrecision(1);
  }
  return formatFraction(value);
}

/** At most one decimal: `3.4`, `3`, `0.25`. */
function formatFraction(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value);
}

function formatValue(metric: UsageMetric, value: number): string {
  switch (metric) {
    case "cost":
      return formatCost(value);
    case "latency":
      return `${formatNumber(value)} ms`;
    default:
      return formatNumber(value);
  }
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(Math.round(value));
}

function formatCost(value: number): string {
  const abs = Math.abs(value);
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 0,
    maximumFractionDigits: abs >= 100 ? 0 : abs >= 1 ? 2 : 4,
  }).format(value);
}
