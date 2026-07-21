"use client";

import type { AggregatedUsage } from "@any-converter/shared";
import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { useI18n } from "../i18n";

interface UsageChartProps {
  data: AggregatedUsage[];
}

type ChartPoint = AggregatedUsage & {
  label: string;
  avg_latency_ms: number;
  max_latency_ms: number;
  error_count: number;
};

type LineSeries = {
  key: keyof ChartPoint;
  name: string;
  color: string;
  dashed?: boolean;
};

type BarSeries = {
  key: keyof ChartPoint;
  name: string;
  color: string;
};

const COLORS = {
  input: "#2563eb",
  output: "#16a34a",
  total: "#7c3aed",
  requests: "#0f766e",
  latency: "#ea580c",
  maxLatency: "#f59e0b",
  errors: "#dc2626",
  grid: "#e5e7eb",
  axis: "#737373",
};

const CHART = {
  width: 960,
  height: 260,
  left: 72,
  right: 24,
  top: 20,
  bottom: 48,
};

export function UsageChart({ data }: UsageChartProps) {
  const { t } = useI18n();

  if (data.length === 0) {
    return (
      <div className="flex h-64 items-center justify-center rounded-md border text-muted-foreground">
        {t("usage.empty")}
      </div>
    );
  }

  const chartData = data.map((record) => ({
    ...record,
    label: formatHour(record.timestamp),
    avg_latency_ms: record.avg_latency_ms ?? record.latency_ms,
    max_latency_ms: record.max_latency_ms ?? record.latency_ms,
    error_count: record.error_count ?? 0,
  }));
  const summary = summarize(chartData);

  return (
    <div className="grid gap-4">
      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-5">
        <SummaryCard title={t("usage.totalTokens")} value={formatNumber(summary.totalTokens)} accent={COLORS.total} />
        <SummaryCard title={t("usage.inputTokens")} value={formatNumber(summary.inputTokens)} accent={COLORS.input} />
        <SummaryCard title={t("usage.outputTokens")} value={formatNumber(summary.outputTokens)} accent={COLORS.output} />
        <SummaryCard title={t("usage.requests")} value={formatNumber(summary.requests)} accent={COLORS.requests} />
        <SummaryCard title={t("usage.avgLatency")} value={`${formatNumber(summary.avgLatency)} ms`} accent={COLORS.latency} />
      </div>

      <ChartCard title={t("usage.tokenTrend")}>
        <LineChart
          data={chartData}
          maxValue={maxOf(chartData, ["total_tokens"])}
          valueFormatter={formatCompact}
          series={[
            { key: "input_tokens", name: t("usage.inputTokens"), color: COLORS.input },
            { key: "output_tokens", name: t("usage.outputTokens"), color: COLORS.output },
            { key: "total_tokens", name: t("usage.totalTokens"), color: COLORS.total },
          ]}
        />
      </ChartCard>

      <div className="grid gap-4 xl:grid-cols-2">
        <ChartCard title={t("usage.requestVolume")}>
          <BarChart
            data={chartData}
            maxValue={maxOf(chartData, ["request_count", "error_count"])}
            valueFormatter={formatNumber}
            series={[
              { key: "request_count", name: t("usage.requests"), color: COLORS.requests },
              { key: "error_count", name: t("usage.errors"), color: COLORS.errors },
            ]}
          />
        </ChartCard>

        <ChartCard title={t("usage.latency")}>
          <LineChart
            data={chartData}
            maxValue={maxOf(chartData, ["avg_latency_ms", "max_latency_ms"])}
            valueFormatter={formatLatencyTick}
            series={[
              { key: "avg_latency_ms", name: t("usage.avgLatency"), color: COLORS.latency },
              { key: "max_latency_ms", name: t("usage.maxLatency"), color: COLORS.maxLatency, dashed: true },
            ]}
          />
        </ChartCard>
      </div>
    </div>
  );
}

function SummaryCard({ title, value, accent }: { title: string; value: string; accent: string }) {
  return (
    <div className="rounded-md border bg-background p-4">
      <div className="mb-3 h-1 w-10 rounded-full" style={{ backgroundColor: accent }} />
      <div className="text-sm text-muted-foreground">{title}</div>
      <div className="mt-1 text-2xl font-semibold">{value}</div>
    </div>
  );
}

function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

function LineChart({
  data,
  maxValue,
  valueFormatter,
  series,
}: {
  data: ChartPoint[];
  maxValue: number;
  valueFormatter: (value: number) => string;
  series: LineSeries[];
}) {
  const yMax = niceMax(maxValue);
  const ticks = makeTicks(yMax, 4);
  return (
    <ChartSurface>
      <Grid ticks={ticks} yMax={yMax} valueFormatter={valueFormatter} />
      <TimeAxis data={data} />
      {series.map((item) => (
        <g key={item.name}>
          <path
            d={makeLinePath(data, item.key, yMax)}
            fill="none"
            stroke={item.color}
            strokeWidth="3"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeDasharray={item.dashed ? "7 6" : undefined}
          />
          {data.map((point, index) => {
            const value = numericValue(point[item.key]);
            return <circle key={`${item.name}-${point.label}`} cx={xFor(index, data.length)} cy={yFor(value, yMax)} r="3.5" fill="#fff" stroke={item.color} strokeWidth="2" />;
          })}
        </g>
      ))}
      <Legend items={series} />
    </ChartSurface>
  );
}

function BarChart({
  data,
  maxValue,
  valueFormatter,
  series,
}: {
  data: ChartPoint[];
  maxValue: number;
  valueFormatter: (value: number) => string;
  series: BarSeries[];
}) {
  const yMax = niceMax(maxValue);
  const ticks = makeTicks(yMax, 4);
  const groupWidth = Math.min(60, plotWidth() / Math.max(data.length, 1) / 2);
  const barWidth = groupWidth / Math.max(series.length, 1);
  return (
    <ChartSurface>
      <Grid ticks={ticks} yMax={yMax} valueFormatter={valueFormatter} />
      <TimeAxis data={data} />
      {data.map((point, pointIndex) => {
        const groupLeft = xFor(pointIndex, data.length) - groupWidth / 2;
        return series.map((item, seriesIndex) => {
          const value = numericValue(point[item.key]);
          const y = yFor(value, yMax);
          return (
            <rect
              key={`${item.name}-${point.label}`}
              x={groupLeft + seriesIndex * barWidth}
              y={y}
              width={Math.max(4, barWidth - 3)}
              height={CHART.height - CHART.bottom - y}
              rx="3"
              fill={item.color}
            />
          );
        });
      })}
      <Legend items={series} />
    </ChartSurface>
  );
}

function ChartSurface({ children }: { children: React.ReactNode }) {
  return (
    <div className="w-full">
      <svg className="h-[280px] w-full overflow-visible" viewBox={`0 0 ${CHART.width} ${CHART.height}`} role="img">
        {children}
      </svg>
    </div>
  );
}

function Grid({ ticks, yMax, valueFormatter }: { ticks: number[]; yMax: number; valueFormatter: (value: number) => string }) {
  return (
    <g>
      {ticks.map((tick) => {
        const y = yFor(tick, yMax);
        return (
          <g key={tick}>
            <line x1={CHART.left} x2={CHART.width - CHART.right} y1={y} y2={y} stroke={COLORS.grid} strokeDasharray="4 4" />
            <text x={CHART.left - 12} y={y + 4} textAnchor="end" className="fill-muted-foreground text-xs">
              {valueFormatter(tick)}
            </text>
          </g>
        );
      })}
      <line x1={CHART.left} x2={CHART.left} y1={CHART.top} y2={CHART.height - CHART.bottom} stroke={COLORS.axis} />
      <line x1={CHART.left} x2={CHART.width - CHART.right} y1={CHART.height - CHART.bottom} y2={CHART.height - CHART.bottom} stroke={COLORS.axis} />
    </g>
  );
}

function TimeAxis({ data }: { data: ChartPoint[] }) {
  return (
    <g>
      {data.map((point, index) => (
        <text key={point.label} x={xFor(index, data.length)} y={CHART.height - CHART.bottom + 24} textAnchor="middle" className="fill-muted-foreground text-xs">
          {point.label}
        </text>
      ))}
    </g>
  );
}

function Legend({ items }: { items: Array<LineSeries | BarSeries> }) {
  const itemWidth = 150;
  const startX = CHART.left + (plotWidth() - items.length * itemWidth) / 2;
  return (
    <g>
      {items.map((item, index) => (
        <g key={item.name} transform={`translate(${startX + index * itemWidth}, ${CHART.height - 8})`}>
          <rect x="0" y="-10" width="12" height="12" rx="2" fill={item.color} />
          <text x="18" y="0" className="fill-muted-foreground text-xs">
            {item.name}
          </text>
        </g>
      ))}
    </g>
  );
}

function summarize(data: ChartPoint[]) {
  const inputTokens = data.reduce((sum, item) => sum + item.input_tokens, 0);
  const outputTokens = data.reduce((sum, item) => sum + item.output_tokens, 0);
  const requests = data.reduce((sum, item) => sum + item.request_count, 0);
  const weightedLatency = data.reduce((sum, item) => sum + item.avg_latency_ms * item.request_count, 0);
  return {
    inputTokens,
    outputTokens,
    totalTokens: inputTokens + outputTokens,
    requests,
    avgLatency: requests > 0 ? Math.round(weightedLatency / requests) : 0,
  };
}

function makeLinePath(data: ChartPoint[], key: keyof ChartPoint, yMax: number): string {
  return data
    .map((point, index) => {
      const command = index === 0 ? "M" : "L";
      return `${command} ${xFor(index, data.length)} ${yFor(numericValue(point[key]), yMax)}`;
    })
    .join(" ");
}

function xFor(index: number, total: number): number {
  if (total <= 1) {
    return CHART.left + plotWidth() / 2;
  }
  return CHART.left + (index / (total - 1)) * plotWidth();
}

function yFor(value: number, maxValue: number): number {
  const plotHeight = CHART.height - CHART.top - CHART.bottom;
  const ratio = maxValue > 0 ? value / maxValue : 0;
  return CHART.height - CHART.bottom - ratio * plotHeight;
}

function plotWidth(): number {
  return CHART.width - CHART.left - CHART.right;
}

function maxOf(data: ChartPoint[], keys: Array<keyof ChartPoint>): number {
  return data.reduce((max, point) => Math.max(max, ...keys.map((key) => numericValue(point[key]))), 0);
}

function numericValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function niceMax(value: number): number {
  if (value <= 0) {
    return 1;
  }
  const magnitude = 10 ** Math.floor(Math.log10(value));
  return Math.ceil(value / magnitude) * magnitude;
}

function makeTicks(maxValue: number, count: number): number[] {
  return Array.from({ length: count + 1 }, (_, index) => Math.round((maxValue / count) * index));
}

function formatHour(timestamp: string): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return timestamp.slice(5, 16).replace("T", " ");
  }
  return new Intl.DateTimeFormat(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(Math.round(value));
}

function formatCompact(value: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

function formatLatencyTick(value: number): string {
  if (value >= 1000) {
    return `${(value / 1000).toFixed(value >= 10_000 ? 0 : 1)}s`;
  }
  return `${value}ms`;
}
