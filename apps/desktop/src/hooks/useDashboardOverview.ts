import { useApiClient, type ApiClient } from "@any-converter/core";
import type { UsageSummary } from "@any-converter/shared";

import { api } from "../lib/api";
import { management, type AgentSummary, type CatalogView } from "../lib/management";
import type { ServerStatus } from "../types";
import { usePolling } from "./usePolling";

/** The dashboard polls less often than the sidebar: it is one page of many. */
const DASHBOARD_INTERVAL_MS = 15000;
/** Usage window behind the stat tiles and the agent leaderboard. */
const USAGE_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

export interface AgentUsage {
  agent: string;
  calls: number;
  tokens: number;
}

/**
 * Everything the control-tower dashboard shows, already derived: health
 * signals, onboarding progress, and the 7-day usage numbers. Like the
 * sidebar signals, every probe degrades to undefined (unknown) instead of
 * rejecting, so a stopped gateway dims the page rather than erroring it.
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
  /** 7-day usage totals; undefined when the usage API is unreachable. */
  calls: number | undefined;
  errors: number | undefined;
  costUsd: number | undefined;
  meanLatencyMs: number | null | undefined;
  /** Distinct agents with traffic this week (the tile's number). */
  activeAgentCount: number | undefined;
  /** Agents with traffic this week, most calls first (top 5). */
  activeAgents: AgentUsage[] | undefined;
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
  meanLatencyMs: undefined,
  activeAgentCount: undefined,
  activeAgents: undefined,
};

function isoSeconds(date: Date): string {
  return `${date.toISOString().slice(0, 19)}Z`;
}

async function fetchOverview(apiClient: ApiClient): Promise<DashboardStats> {
  const to = new Date();
  const from = new Date(to.getTime() - USAGE_WINDOW_MS);
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
    meanLatencyMs: totals ? totals.mean_latency_ms : undefined,
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
    a.meanLatencyMs === b.meanLatencyMs &&
    a.activeAgentCount === b.activeAgentCount &&
    JSON.stringify(a.activeAgents) === JSON.stringify(b.activeAgents)
  );
}

/** The dashboard's single data source; `refresh` follows lifecycle actions. */
export function useDashboardOverview(): DashboardStats & { refresh: () => void } {
  const apiClient = useApiClient();
  const { data, refresh } = usePolling(
    () => fetchOverview(apiClient),
    statsEqual,
    DASHBOARD_INTERVAL_MS,
  );
  return { ...(data ?? EMPTY_STATS), refresh };
}
