import { useEffect, useRef } from "react";
import { useApiClient, type ApiClient } from "@any-converter/core";
import type { UsageSummary } from "@any-converter/shared";

import { api } from "../lib/api";
import { management, type AgentSummary, type CatalogView } from "../lib/management";
import type { ServerStatus } from "../types";
import { usePolling } from "./usePolling";

/** The dashboard polls less often than the sidebar: it is one page of many. */
const DASHBOARD_INTERVAL_MS = 15000;
/** Usage window lengths behind the range pills, in days. */
export const RANGE_PRESETS = [
  { key: "today", days: 1 },
  { key: "week", days: 7 },
  { key: "month", days: 30 },
] as const;

export type RangeKey = (typeof RANGE_PRESETS)[number]["key"];

export interface AgentUsage {
  agent: string;
  calls: number;
  tokens: number;
}

/**
 * Everything the control-tower dashboard shows, already derived: health
 * signals, onboarding progress, and the usage numbers for the selected
 * range. Like the sidebar signals, every probe degrades to undefined
 * (unknown) instead of rejecting, so a stopped gateway dims the page
 * rather than erroring it.
 */
export interface DashboardStats {
  status: ServerStatus | undefined;
  /** Wiring counts behind onboarding and the health strip. */
  connectedAgents: number | undefined;
  driftedAgents: number | undefined;
  providerCount: number | undefined;
  /** Providers whose last model discovery failed, by name. */
  providerErrors: string[] | undefined;
  unroutedModels: number | undefined;
  routableModels: number | undefined;
  exposedModels: number | undefined;
  /** Usage totals for the selected window; undefined when unreachable. */
  calls: number | undefined;
  errors: number | undefined;
  costUsd: number | undefined;
  inputTokens: number | undefined;
  outputTokens: number | undefined;
  cacheReadTokens: number | undefined;
  reasoningTokens: number | undefined;
  /** Share of reads served from cache, 0–100; null when no cache data. */
  hitRatePct: number | null | undefined;
  meanLatencyMs: number | null | undefined;
  meanTtftMs: number | null | undefined;
  /** One point per day in the window, for the trend chart. */
  series: DayPoint[] | undefined;
  /** Distinct agents with traffic in the window (the headline number). */
  activeAgentCount: number | undefined;
  /** Agents with traffic, most calls first (top 5). */
  activeAgents: AgentUsage[] | undefined;
}

/** A day's calls, the chart's only payload. */
export interface DayPoint {
  day: string;
  calls: number;
}

const EMPTY_STATS: DashboardStats = {
  status: undefined,
  connectedAgents: undefined,
  driftedAgents: undefined,
  providerCount: undefined,
  providerErrors: undefined,
  unroutedModels: undefined,
  routableModels: undefined,
  exposedModels: undefined,
  calls: undefined,
  errors: undefined,
  costUsd: undefined,
  inputTokens: undefined,
  outputTokens: undefined,
  cacheReadTokens: undefined,
  reasoningTokens: undefined,
  hitRatePct: undefined,
  meanLatencyMs: undefined,
  meanTtftMs: undefined,
  series: undefined,
  activeAgentCount: undefined,
  activeAgents: undefined,
};

function isoSeconds(date: Date): string {
  return `${date.toISOString().slice(0, 19)}Z`;
}

async function fetchOverview(apiClient: ApiClient, rangeDays: number): Promise<DashboardStats> {
  const to = new Date();
  const from = new Date(to.getTime() - rangeDays * 24 * 60 * 60 * 1000);
  const [status, catalog, agents, usage] = await Promise.all([
    api.getServerStatus().catch(() => undefined),
    management.getCatalog().catch(() => undefined),
    management.listAgents().catch(() => undefined),
    apiClient
      .getUsage({ from: isoSeconds(from), to: isoSeconds(to), granularity: "day" })
      .catch(() => undefined),
  ]);
  return derive(status, catalog, agents, usage);
}

function derive(
  status: ServerStatus | undefined,
  catalog: CatalogView | undefined,
  agents: AgentSummary[] | undefined,
  usage: UsageSummary | undefined,
): DashboardStats {
  const providerErrors = catalog?.providers
    .filter((provider) => provider.status === "error")
    .map((provider) => provider.name);
  const exposed = catalog?.models.filter((model) => model.exposed);
  const totals = usage?.totals;
  const withTraffic = usage?.by_agent.filter((group) => group.totals.calls > 0) ?? [];
  const activeAgents = [...withTraffic]
    .sort((a, b) => b.totals.calls - a.totals.calls)
    .slice(0, 5)
    .map((group) => ({
      agent: group.id,
      calls: group.totals.calls,
      tokens:
        group.totals.input_tokens +
        group.totals.output_tokens +
        group.totals.cache_read_tokens +
        group.totals.cache_write_tokens,
    }));
  return {
    status,
    connectedAgents: countBy(agents, "connected"),
    driftedAgents: countBy(agents, "drifted"),
    providerCount: catalog ? catalog.providers.length : undefined,
    providerErrors,
    unroutedModels: exposed ? exposed.filter((model) => !model.routed).length : undefined,
    routableModels: exposed ? exposed.filter((model) => model.routed).length : undefined,
    exposedModels: exposed ? exposed.length : undefined,
    calls: totals?.calls,
    errors: totals?.errors,
    costUsd: totals?.cost_usd,
    inputTokens: totals?.input_tokens,
    outputTokens: totals?.output_tokens,
    cacheReadTokens: totals?.cache_read_tokens,
    reasoningTokens: totals?.reasoning_tokens,
    hitRatePct: totals ? (totals.hit_rate === null ? null : totals.hit_rate * 100) : undefined,
    meanLatencyMs: totals ? totals.mean_latency_ms : undefined,
    meanTtftMs: totals ? totals.mean_ttft_ms : undefined,
    series: usage?.series.map((bucket) => ({
      day: bucket.label,
      calls: bucket.totals.calls,
    })),
    activeAgentCount: usage ? withTraffic.length : undefined,
    activeAgents,
  };
}

function countBy(agents: AgentSummary[] | undefined, status: AgentStatus): number | undefined {
  return agents ? agents.filter((agent) => agent.status === status).length : undefined;
}

type AgentStatus = AgentSummary["status"];

function statsEqual(a: DashboardStats, b: DashboardStats): boolean {
  return (
    a.status === b.status &&
    a.connectedAgents === b.connectedAgents &&
    a.driftedAgents === b.driftedAgents &&
    a.providerCount === b.providerCount &&
    a.providerErrors?.join(",") === b.providerErrors?.join(",") &&
    a.unroutedModels === b.unroutedModels &&
    a.routableModels === b.routableModels &&
    a.exposedModels === b.exposedModels &&
    a.calls === b.calls &&
    a.errors === b.errors &&
    a.costUsd === b.costUsd &&
    a.inputTokens === b.inputTokens &&
    a.outputTokens === b.outputTokens &&
    a.cacheReadTokens === b.cacheReadTokens &&
    a.reasoningTokens === b.reasoningTokens &&
    a.hitRatePct === b.hitRatePct &&
    a.meanLatencyMs === b.meanLatencyMs &&
    a.meanTtftMs === b.meanTtftMs &&
    JSON.stringify(a.series) === JSON.stringify(b.series) &&
    a.activeAgentCount === b.activeAgentCount &&
    JSON.stringify(a.activeAgents) === JSON.stringify(b.activeAgents)
  );
}

/**
 * The dashboard's single data source; `refresh` follows lifecycle actions.
 * Changing `rangeDays` refetches immediately — the pills must feel instant.
 */
export function useDashboardOverview(rangeDays: number): DashboardStats & { refresh: () => void } {
  const apiClient = useApiClient();
  const { data, refresh } = usePolling(
    () => fetchOverview(apiClient, rangeDays),
    statsEqual,
    DASHBOARD_INTERVAL_MS,
  );
  // Refetch when the range changes (not on mount — the poll loop already
  // starts with a round). fetchRef inside usePolling always runs the latest
  // closure, so this pull always uses the new range.
  const firstRun = useRef(true);
  useEffect(() => {
    if (firstRun.current) {
      firstRun.current = false;
      return;
    }
    refresh();
  }, [rangeDays]); // eslint-disable-line react-hooks/exhaustive-deps
  return { ...(data ?? EMPTY_STATS), refresh };
}
