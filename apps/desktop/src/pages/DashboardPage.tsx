import { useState } from "react";
import { errorMessage, useI18n } from "@any-converter/core";
import { Badge, Button } from "@any-converter/ui";

import { AgentUsageList, StatsTiles, TrendChart } from "../components/dashboard/StatsTiles";
import { HealthStrip } from "../components/dashboard/HealthStrip";
import { OnboardingChecklist } from "../components/dashboard/OnboardingChecklist";
import { ErrorBanner } from "../components/layout/ErrorBanner";
import { RANGE_PRESETS, useDashboardOverview, type RangeKey } from "../hooks/useDashboardOverview";
import { api } from "../lib/api";
import { isDesktopApp } from "../lib/platform";
import type { ServerStatus } from "../types";

const statusBadgeVariant: Record<ServerStatus["state"], "default" | "secondary" | "destructive" | "outline"> = {
  running: "default",
  starting: "secondary",
  stopped: "outline",
  stopping: "outline",
  failed: "destructive",
  attached: "secondary",
};

function formatCost(usd: number | undefined): string {
  if (usd === undefined) {
    return "—";
  }
  return usd >= 1 ? `$${usd.toFixed(2)}` : `$${usd.toFixed(4)}`;
}

/** "823 ms" / "1.4 s"; null means the window had no timed calls. */
function formatLatency(ms: number | null | undefined): string {
  if (ms === undefined || ms === null) {
    return "—";
  }
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms)} ms`;
}

/**
 * The control tower, in magpie's page language: one quiet intro sentence,
 * chips for what needs attention, a single KPI card over the selected
 * range, and list cards for the rest.
 */
export function DashboardPage() {
  const { t } = useI18n();
  const [rangeKey, setRangeKey] = useState<RangeKey>("week");
  const rangeDays = RANGE_PRESETS.find((preset) => preset.key === rangeKey)?.days ?? 7;
  const overview = useDashboardOverview(rangeDays);
  const [actionError, setActionError] = useState<string>();
  const status = overview.status;
  const canManageLifecycle = isDesktopApp() && status?.state !== "attached";

  const run = async (action: () => Promise<ServerStatus>) => {
    try {
      await action();
      setActionError(undefined);
      overview.refresh();
    } catch (cause) {
      setActionError(errorMessage(cause));
    }
  };

  const serverHints = [
    status?.host === "0.0.0.0" ? t("desktop.dashboard.lanHint") : undefined,
    status?.state === "attached" ? t("desktop.dashboard.externalHint") : undefined,
    !isDesktopApp() ? t("desktop.dashboard.externalHintBrowser") : undefined,
  ].filter(Boolean);

  return (
    <section className="grid gap-5">
      <p className="page-intro">
        <b>{t("desktop.dashboard.intro.lead")}</b> {t("desktop.dashboard.intro.rest")}
      </p>
      {actionError && <ErrorBanner message={actionError} />}
      <HealthStrip stats={overview} />
      <OnboardingChecklist stats={overview} />

      <div className="usage-head">
        <div className="range-group">
          {RANGE_PRESETS.map((preset) => (
            <button
              key={preset.key}
              type="button"
              className={`range-opt${preset.key === rangeKey ? " on" : ""}`}
              aria-pressed={preset.key === rangeKey}
              onClick={() => setRangeKey(preset.key)}
            >
              {t(`desktop.dashboard.range.${preset.key}`)}
            </button>
          ))}
        </div>
        <span className="usage-cost">
          ≈<b>{formatCost(overview.costUsd)}</b> {t("desktop.dashboard.costSuffix")}
        </span>
      </div>
      <StatsTiles stats={overview} />
      <TrendChart stats={overview} />

      <div className="grid gap-4 md:grid-cols-2">
        <AgentUsageList stats={overview} />
        <div className="list-card">
          <div className="lc-head">
            {t("desktop.dashboard.server")}
            <Badge
              variant={status ? statusBadgeVariant[status.state] : "secondary"}
              className="ml-auto capitalize"
            >
              {status?.state ?? "loading"}
            </Badge>
          </div>
          <div className="lc-row">
            <div className="min-w-0">
              <span className="lc-title">{status ? `${status.host}:${status.port}` : "…"}</span>
              {overview.meanLatencyMs !== undefined && overview.meanLatencyMs !== null && (
                <span className="lc-sub">
                  {t("desktop.dashboard.serverPerf")
                    .replace("{lat}", formatLatency(overview.meanLatencyMs))
                    .replace("{ttft}", formatLatency(overview.meanTtftMs))}
                </span>
              )}
              {status?.last_error && (
                <span className="lc-sub" style={{ color: "var(--drift)" }}>
                  {status.last_error}
                </span>
              )}
              {serverHints.map((hint) => (
                <span key={hint} className="lc-sub">
                  {hint}
                </span>
              ))}
            </div>
            <div className="ml-auto flex flex-wrap justify-end gap-2">
              {canManageLifecycle && (
                <>
                  <Button size="sm" onClick={() => void run(api.startServer)}>
                    {t("desktop.dashboard.start")}
                  </Button>
                  <Button size="sm" variant="secondary" onClick={() => void run(api.stopServer)}>
                    {t("desktop.dashboard.stop")}
                  </Button>
                  <Button size="sm" variant="secondary" onClick={() => void run(api.restartServer)}>
                    {t("desktop.dashboard.restart")}
                  </Button>
                </>
              )}
              <Button size="sm" variant="outline" onClick={() => overview.refresh()}>
                {t("desktop.dashboard.refresh")}
              </Button>
            </div>
          </div>
        </div>
      </div>

      {overview.providerCount !== undefined && (
        <div>
          <div className="section-label">{t("desktop.dashboard.catalog")}</div>
          <div className="stats">
            <div className="kpi">
              <b>{overview.providerCount}</b>
              <span>{t("desktop.dashboard.providerCount")}</span>
            </div>
            <div className="kpi">
              <b>{overview.exposedModels ?? 0}</b>
              <span>{t("desktop.dashboard.exposedCount")}</span>
            </div>
            <div className="kpi">
              <b>{overview.routableModels ?? 0}</b>
              <span>{t("desktop.dashboard.routableCount")}</span>
            </div>
            <div className="kpi">
              <b
                style={
                  (overview.unroutedModels ?? 0) > 0 ? { color: "var(--warn)" } : undefined
                }
              >
                {overview.unroutedModels ?? 0}
              </b>
              <span>{t("desktop.dashboard.unroutedCount")}</span>
            </div>
          </div>
          <p className="mt-2 text-xs text-muted-foreground">{t("desktop.dashboard.catalogHint")}</p>
        </div>
      )}
    </section>
  );
}
