"use client";

import { useMemo, useState } from "react";

import type { Granularity, SplitDim, UsageGroup } from "@any-converter/shared";
import { formatDuration } from "@any-converter/shared";
import { Button, Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { UsageChart, useHeatmap, useI18n, useUsage } from "@any-converter/core";
import type { UsageMetric } from "@any-converter/core";
// The heatmap is imported from its own module: the core barrel is not part of
// this change's surface.
import { UsageHeatmap } from "@any-converter/core";

type Preset = "today" | "7d" | "30d" | "all";

const PRESETS = [
  { value: "today", key: "usage.period.today" },
  { value: "7d", key: "usage.period.7d" },
  { value: "30d", key: "usage.period.30d" },
  { value: "all", key: "usage.period.all" },
] as const;

const GRANULARITIES = [
  { value: "hour", key: "usage.granularity.hour" },
  { value: "day", key: "usage.granularity.day" },
  { value: "week", key: "usage.granularity.week" },
  { value: "month", key: "usage.granularity.month" },
] as const;

const DEFAULT_GRANULARITY: Record<Preset, Granularity> = {
  today: "hour",
  "7d": "day",
  "30d": "day",
  all: "week",
};

const METRICS = [
  { value: "tokens", key: "usage.metric.tokens" },
  { value: "cost", key: "usage.metric.cost" },
  { value: "calls", key: "usage.metric.calls" },
  { value: "latency", key: "usage.metric.latency" },
] as const;

type Split = "none" | SplitDim;

const SPLITS = [
  { value: "none", key: "usage.split.none" },
  { value: "provider", key: "usage.split.provider" },
  { value: "model", key: "usage.split.model" },
  { value: "agent", key: "usage.split.agent" },
] as const;

const DAY_MS = 24 * 60 * 60 * 1000;

/** UTC query bounds for a period preset, truncated to whole seconds. */
function presetRange(preset: Preset): { from: string; to: string } {
  const now = new Date();
  const start = new Date(now.getTime());
  if (preset === "today") {
    start.setUTCHours(0, 0, 0, 0);
  } else if (preset === "7d") {
    start.setTime(now.getTime() - 7 * DAY_MS);
  } else if (preset === "30d") {
    start.setTime(now.getTime() - 30 * DAY_MS);
  } else {
    start.setTime(0);
  }
  return { from: toIsoSeconds(start), to: toIsoSeconds(now) };
}

function toIsoSeconds(date: Date): string {
  return date.toISOString().slice(0, 19) + "Z";
}

export function UsageView() {
  const { t } = useI18n();
  const [preset, setPreset] = useState<Preset>("7d");
  const [granularity, setGranularity] = useState<Granularity>(DEFAULT_GRANULARITY["7d"]);
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const [split, setSplit] = useState<Split>("none");

  // Anchor the range per period change: recomputing "now" on every render would
  // hand useUsage a new query key and refetch in a loop. The split is optional,
  // so `none` leaves it out rather than sending an empty value.
  const query = useMemo(
    () => ({
      ...presetRange(preset),
      granularity,
      ...(split === "none" ? {} : { split_by: split }),
    }),
    [preset, granularity, split],
  );
  const { summary, loading, error } = useUsage(query);
  // The heatmap runs its own 53-week span; the range here only satisfies the
  // filter shape the request carries, so it refetches with the preset.
  const { heatmap } = useHeatmap({ from: query.from, to: query.to });

  const choosePreset = (next: Preset) => {
    setPreset(next);
    setGranularity(DEFAULT_GRANULARITY[next]);
  };

  const ready = !loading && !error && summary !== null;
  const isEmpty =
    summary !== null &&
    summary.series.length === 0 &&
    summary.by_provider.length === 0 &&
    summary.by_model.length === 0 &&
    (summary.by_account?.length ?? 0) === 0;
  const totals = summary?.totals;
  const notAvailable = t("usage.notAvailable");

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("usage.title")}</h1>
        <p className="text-muted-foreground">{t("usage.subtitle")}</p>
      </div>

      <div className="flex flex-wrap items-end gap-6">
        <div className="grid gap-2">
          <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{t("usage.period")}</span>
          <div className="flex flex-wrap gap-2">
            {PRESETS.map((item) => (
              <Button
                key={item.value}
                type="button"
                size="sm"
                variant={preset === item.value ? "default" : "outline"}
                aria-pressed={preset === item.value}
                onClick={() => choosePreset(item.value)}
              >
                {t(item.key)}
              </Button>
            ))}
          </div>
        </div>

        <div className="grid gap-2">
          <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            {t("usage.granularity")}
          </span>
          <div className="flex flex-wrap gap-2">
            {GRANULARITIES.map((item) => (
              <Button
                key={item.value}
                type="button"
                size="sm"
                variant={granularity === item.value ? "default" : "outline"}
                aria-pressed={granularity === item.value}
                onClick={() => setGranularity(item.value)}
              >
                {t(item.key)}
              </Button>
            ))}
          </div>
        </div>

        <div className="grid gap-2">
          <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            {t("usage.metric")}
          </span>
          <div className="flex flex-wrap gap-2">
            {METRICS.map((item) => (
              <Button
                key={item.value}
                type="button"
                size="sm"
                variant={metric === item.value ? "default" : "outline"}
                aria-pressed={metric === item.value}
                onClick={() => setMetric(item.value)}
              >
                {t(item.key)}
              </Button>
            ))}
          </div>
        </div>

        <div className="grid gap-2">
          <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            {t("usage.split")}
          </span>
          <div className="flex flex-wrap gap-2">
            {SPLITS.map((item) => (
              <Button
                key={item.value}
                type="button"
                size="sm"
                variant={split === item.value ? "default" : "outline"}
                aria-pressed={split === item.value}
                onClick={() => setSplit(item.value)}
              >
                {t(item.key)}
              </Button>
            ))}
          </div>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("usage.metrics")}</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">{t("usage.loading")}</p>}
          {error && <p className="text-destructive">{error}</p>}
          {ready && isEmpty && <p className="text-muted-foreground">{t("usage.empty")}</p>}
          {ready && !isEmpty && totals !== undefined && (
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-7">
              <MetricCard title={t("usage.totalTokens")} value={formatNumber(totals.input_tokens + totals.output_tokens)} />
              <MetricCard title={t("usage.hitRate")} value={formatHitRate(totals.hit_rate, notAvailable)} />
              <MetricCard
                title={t("usage.cost")}
                value={formatCost(totals.cost_usd)}
                detail={
                  totals.unpriced > 0
                    ? `${formatNumber(totals.unpriced)} ${t("usage.unpricedCalls")}`
                    : undefined
                }
              />
              <MetricCard title={t("usage.avgLatency")} value={formatLatency(totals.mean_latency_ms, notAvailable)} />
              <MetricCard title={t("usage.maxLatency")} value={formatLatency(totals.latency_max_ms, notAvailable)} />
              <MetricCard title={t("usage.errors")} value={formatNumber(totals.errors)} />
              <MetricCard title={t("usage.retried")} value={formatNumber(totals.retried ?? 0)} />
            </div>
          )}
        </CardContent>
      </Card>

      {ready && !isEmpty && summary !== null && (
        <>
          <UsageChart data={summary.series} metric={metric} stacked={split !== "none"} />
          {/* The Ledger tab owns day drill-down, so this click is a no-op for now. */}
          {heatmap !== null && <UsageHeatmap data={heatmap} onSelectDay={() => undefined} />}
          <RankingTable idLabel={t("usage.byProvider")} title={t("usage.byProvider")} groups={summary.by_provider} />
          <RankingTable idLabel={t("usage.byModel")} title={t("usage.byModel")} groups={summary.by_model} showProvider />
          {summary.by_account !== undefined && summary.by_account.length > 0 && (
            <RankingTable
              idLabel={t("usage.account")}
              title={t("usage.byAccount")}
              groups={summary.by_account}
              showProvider
            />
          )}
        </>
      )}
    </div>
  );
}

function MetricCard({ title, value, detail }: { title: string; value: string; detail?: string }) {
  return (
    <div className="rounded-md border bg-background p-4">
      <div className="text-sm text-muted-foreground">{title}</div>
      <div className="mt-1 text-2xl font-semibold tabular-nums">{value}</div>
      {detail !== undefined && <div className="text-xs text-muted-foreground tabular-nums">{detail}</div>}
    </div>
  );
}

function RankingTable({
  title,
  idLabel,
  groups,
  showProvider = false,
}: {
  title: string;
  idLabel: string;
  groups: UsageGroup[];
  showProvider?: boolean;
}) {
  const { t } = useI18n();
  const notAvailable = t("usage.notAvailable");

  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>
        {groups.length === 0 ? (
          <p className="text-muted-foreground">{t("usage.empty")}</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b text-left text-xs uppercase tracking-wide text-muted-foreground">
                  <th className="py-2 pr-4 font-medium">{idLabel}</th>
                  <th className="py-2 pr-4 text-right font-medium">{t("usage.callCount")}</th>
                  <th className="py-2 pr-4 text-right font-medium">{t("usage.tokens")}</th>
                  <th className="py-2 pr-4 text-right font-medium">{t("usage.hitRate")}</th>
                  <th className="py-2 text-right font-medium">{t("usage.avgLatency")}</th>
                </tr>
              </thead>
              <tbody>
                {groups.map((group) => (
                  <tr key={`${group.id}:${group.provider ?? ""}:${group.model ?? ""}`} className="border-b last:border-b-0">
                    <td className="py-2 pr-4">
                      <div className="font-medium">{group.id}</div>
                      {showProvider && group.provider !== undefined && group.provider !== "" && (
                        <div className="text-xs text-muted-foreground">{group.provider}</div>
                      )}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">{formatNumber(group.totals.calls)}</td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(group.totals.input_tokens + group.totals.output_tokens)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatHitRate(group.totals.hit_rate, notAvailable)}
                    </td>
                    <td className="py-2 text-right tabular-nums">
                      {formatLatency(group.totals.mean_latency_ms, notAvailable)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(Math.round(value));
}

function formatHitRate(value: number | null, notAvailable: string): string {
  return value === null ? notAvailable : `${(value * 100).toFixed(1)}%`;
}

function formatLatency(value: number | null, notAvailable: string): string {
  return value === null ? notAvailable : formatDuration(Math.round(value));
}

function formatCost(value: number): string {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 4,
  }).format(value);
}
