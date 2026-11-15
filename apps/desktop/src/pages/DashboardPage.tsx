import { useState } from "react";
import { errorMessage, useI18n } from "@any-converter/core";
import { Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle } from "@any-converter/ui";

import { AgentUsageList, StatsTiles } from "../components/dashboard/StatsTiles";
import { HealthStrip } from "../components/dashboard/HealthStrip";
import { OnboardingChecklist } from "../components/dashboard/OnboardingChecklist";
import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { useDashboardOverview } from "../hooks/useDashboardOverview";
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

/**
 * The control tower: what needs attention, how far setup has come, how the
 * gateway is being used — then the lifecycle and catalog detail underneath.
 */
export function DashboardPage() {
  const { t } = useI18n();
  const overview = useDashboardOverview();
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

  return (
    <section className="grid gap-6">
      <Header title={t("nav.dashboard")} subtitle={t("desktop.dashboard.subtitle")} />
      {actionError && <ErrorBanner message={actionError} />}
      <HealthStrip stats={overview} />
      <OnboardingChecklist stats={overview} />
      <StatsTiles stats={overview} />
      <div className="grid gap-4 md:grid-cols-2">
        <AgentUsageList stats={overview} />
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.server")}</CardTitle>
            <CardDescription>{status ? `${status.host}:${status.port}` : "Loading…"}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <Badge variant={status ? statusBadgeVariant[status.state] : "secondary"} className="w-fit capitalize">
              {status?.state ?? "loading"}
            </Badge>
            {status?.host === "0.0.0.0" && (
              <p className="text-sm text-muted-foreground">{t("desktop.dashboard.lanHint")}</p>
            )}
            {status?.last_error && <p className="text-sm text-destructive">{status.last_error}</p>}
            {status?.state === "attached" && (
              <p className="text-sm text-muted-foreground">{t("desktop.dashboard.externalHint")}</p>
            )}
            {!isDesktopApp() && (
              <p className="text-sm text-muted-foreground">{t("desktop.dashboard.externalHintBrowser")}</p>
            )}
            <div className="flex flex-wrap gap-2">
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
          </CardContent>
        </Card>
      </div>
      {overview.providerCount !== undefined && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.catalog")}</CardTitle>
            <CardDescription>{t("desktop.dashboard.catalogDescription")}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.providerCount")}</p>
                <p className="text-2xl font-semibold">{overview.providerCount}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.exposedCount")}</p>
                <p className="text-2xl font-semibold">{overview.exposedModels ?? 0}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.routableCount")}</p>
                <p className="text-2xl font-semibold">{overview.routableModels ?? 0}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.unroutedCount")}</p>
                <p
                  className={
                    (overview.unroutedModels ?? 0) > 0
                      ? "text-2xl font-semibold text-amber-600 dark:text-amber-400"
                      : "text-2xl font-semibold"
                  }
                >
                  {overview.unroutedModels ?? 0}
                </p>
              </div>
            </div>
            <p className="text-sm text-muted-foreground">{t("desktop.dashboard.catalogHint")}</p>
          </CardContent>
        </Card>
      )}
    </section>
  );
}
