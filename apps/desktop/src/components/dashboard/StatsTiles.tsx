import { useI18n } from "@any-converter/core";

import type { DashboardStats } from "../../hooks/useDashboardOverview";

const compact = new Intl.NumberFormat(undefined, { notation: "compact" });
const plain = new Intl.NumberFormat();

/**
 * The KPI strip: ONE card, four hairline-divided cells — a 20px number,
 * an 11px uppercase label, and a quiet sub-line each. Cells read tokens,
 * cache read, reasoning, calls last.
 * Em-dash is the honest value for "no data": a stopped gateway shows
 * dashes, not zeros.
 */
export function StatsTiles({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();

  const tokens =
    stats.inputTokens !== undefined || stats.outputTokens !== undefined
      ? compact.format((stats.inputTokens ?? 0) + (stats.outputTokens ?? 0))
      : "—";
  const inOut =
    stats.inputTokens !== undefined && stats.outputTokens !== undefined
      ? t("desktop.dashboard.kpi.inOut")
          .replace("{in}", compact.format(stats.inputTokens))
          .replace("{out}", compact.format(stats.outputTokens))
      : "";
  const errorsSub =
    stats.errors === undefined
      ? ""
      : stats.errors > 0
        ? t("desktop.dashboard.kpi.errors").replace("{count}", plain.format(stats.errors))
        : t("desktop.dashboard.kpi.noErrors");
  const cacheSub =
    stats.hitRatePct === undefined || stats.hitRatePct === null
      ? ""
      : t("desktop.dashboard.kpi.hitRate").replace("{rate}", stats.hitRatePct.toFixed(0));

  const cells = [
    {
      key: "tokens",
      label: t("desktop.dashboard.kpi.tokens"),
      value: tokens,
      sub: inOut,
      subAttn: false,
    },
    {
      key: "cache",
      label: t("desktop.dashboard.kpi.cache"),
      value: stats.cacheReadTokens !== undefined ? compact.format(stats.cacheReadTokens) : "—",
      sub: cacheSub,
      subAttn: false,
    },
    {
      key: "reasoning",
      label: t("desktop.dashboard.kpi.reasoning"),
      value: stats.reasoningTokens !== undefined ? compact.format(stats.reasoningTokens) : "—",
      sub: "",
      subAttn: false,
    },
    {
      key: "calls",
      label: t("desktop.dashboard.kpi.calls"),
      value: stats.calls !== undefined ? plain.format(stats.calls) : "—",
      sub: errorsSub,
      subAttn: (stats.errors ?? 0) > 0,
    },
  ];

  return (
    <div className="stats">
      {cells.map((cell) => (
        <div key={cell.key} className="kpi">
          <b>{cell.value}</b>
          <span>{cell.label}</span>
          {cell.sub && <small className={cell.subAttn ? "attn" : undefined}>{cell.sub}</small>}
        </div>
      ))}
    </div>
  );
}

/**
 * Daily calls over the window: one flat-accent bar per day, the y-max as
 * a quiet label. Hidden when the window has no days with
 * traffic — an empty chart reads as broken, missing reads as calm.
 */
export function TrendChart({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();
  const points = (stats.series ?? []).filter((point) => point.calls > 0);
  if (points.length === 0) {
    return null;
  }
  const max = Math.max(...points.map((point) => point.calls));

  return (
    <div className="chart-card">
      <div className="chart-top">
        <span className="section-label" style={{ marginBottom: 0 }}>
          {t("desktop.dashboard.chart.calls")}
        </span>
        <span className="max">{compact.format(max)}</span>
      </div>
      <div className="chart" role="img" aria-label={t("desktop.dashboard.chart.calls")}>
        {(stats.series ?? []).map((point, index) => (
          <div
            key={`${point.day}-${index}`}
            className="bar"
            style={{ height: `${Math.max(2, (point.calls / max) * 100)}%` }}
            title={`${point.day}: ${plain.format(point.calls)}`}
          />
        ))}
      </div>
    </div>
  );
}

/** The leaderboard under the tiles: which agents actually used the gateway. */
export function AgentUsageList({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();
  const agents = stats.activeAgents ?? [];

  return (
    <div className="list-card">
      <div className="lc-head">
        {t("desktop.dashboard.byAgent")}
        {stats.activeAgentCount !== undefined && (
          <span className="count">
            {t("desktop.dashboard.byAgentCount").replace(
              "{count}",
              plain.format(stats.activeAgentCount),
            )}
          </span>
        )}
      </div>
      {agents.length === 0 ? (
        <p className="lc-empty">
          {stats.activeAgents === undefined ? "—" : t("desktop.dashboard.byAgent.empty")}
        </p>
      ) : (
        <ul>
          {agents.map((agent) => (
            <li key={agent.agent} className="lc-row">
              <span className="lc-title min-w-0 flex-1">{agent.agent}</span>
              <span className="text-xs tabular-nums text-muted-foreground">
                {plain.format(agent.calls)} · {compact.format(agent.tokens)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
