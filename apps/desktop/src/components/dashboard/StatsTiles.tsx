import { useI18n } from "@any-converter/core";

import type { DashboardStats } from "../../hooks/useDashboardOverview";

const compact = new Intl.NumberFormat(undefined, { notation: "compact" });
const plain = new Intl.NumberFormat();

/** "823 ms" / "1.4 s"; null means the window had no timed calls. */
function formatLatency(ms: number | null | undefined): string {
  if (ms === undefined || ms === null) {
    return "—";
  }
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms)} ms`;
}

function formatCost(usd: number | undefined): string {
  if (usd === undefined) {
    return "—";
  }
  return usd >= 1 ? `$${usd.toFixed(2)}` : `$${usd.toFixed(4)}`;
}

/**
 * The live overview: five numbers over the last 7 days. Em-dash is the
 * honest value for "no data" — a stopped gateway shows dashes, not zeros.
 */
export function StatsTiles({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();

  const calls = stats.calls;
  const errorRate =
    calls !== undefined && calls > 0 && stats.errors !== undefined
      ? `${((stats.errors / calls) * 100).toFixed(1)}%`
      : "—";

  const tiles = [
    { key: "calls", label: t("usage.requests"), value: calls !== undefined ? plain.format(calls) : "—" },
    { key: "errors", label: t("desktop.dashboard.stats.errorRate"), value: errorRate },
    { key: "latency", label: t("usage.avgLatency"), value: formatLatency(stats.meanLatencyMs) },
    { key: "cost", label: t("desktop.dashboard.stats.cost"), value: formatCost(stats.costUsd) },
    {
      key: "agents",
      label: t("desktop.dashboard.stats.activeAgents"),
      value:
        stats.activeAgentCount !== undefined ? plain.format(stats.activeAgentCount) : "—",
    },
  ];

  return (
    <section aria-label={t("desktop.dashboard.stats.window")}>
      <div className="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {t("desktop.dashboard.stats.window")}
      </div>
      <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
        {tiles.map((tile) => (
          <div key={tile.key} className="rounded-lg border bg-card p-4">
            <div className="truncate text-xs text-muted-foreground">{tile.label}</div>
            <div className="mt-1 text-2xl font-semibold tabular-nums">{tile.value}</div>
          </div>
        ))}
      </div>
    </section>
  );
}

/** The leaderboard under the tiles: which agents actually used the gateway. */
export function AgentUsageList({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();
  const agents = stats.activeAgents ?? [];

  return (
    <div className="rounded-lg border bg-card">
      <div className="border-b px-4 py-3 text-sm font-semibold">
        {t("desktop.dashboard.byAgent")}
      </div>
      {agents.length === 0 ? (
        <p className="px-4 py-3 text-sm text-muted-foreground">
          {stats.activeAgents === undefined ? "—" : t("desktop.dashboard.byAgent.empty")}
        </p>
      ) : (
        <ul className="divide-y">
          {agents.map((agent) => (
            <li key={agent.agent} className="flex items-center gap-3 px-4 py-2.5 text-sm">
              <code className="rounded bg-muted px-2 py-0.5 text-xs font-medium">{agent.agent}</code>
              <span className="ml-auto tabular-nums text-muted-foreground">
                {plain.format(agent.calls)} · {compact.format(agent.tokens)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
