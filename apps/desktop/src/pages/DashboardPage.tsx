import { errorMessage, useI18n } from "@any-converter/core";
import { Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle } from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { useAsyncState } from "../hooks/useAsyncState";
import { api } from "../lib/api";
import { isDesktopApp } from "../lib/platform";
import { management, type CatalogView } from "../lib/management";
import type { ServerStatus } from "../types";

const statusBadgeVariant: Record<ServerStatus["state"], "default" | "secondary" | "destructive" | "outline"> = {
  running: "default",
  starting: "secondary",
  stopped: "outline",
  stopping: "outline",
  failed: "destructive",
  attached: "secondary",
};

export function DashboardPage() {
  const { t } = useI18n();
  const [status, setStatus, error, setError] = useAsyncState<ServerStatus>(api.getServerStatus);
  // The catalog is optional here: a stopped or legacy gateway must not turn
  // the dashboard into an error page.
  const [catalog] = useAsyncState<CatalogView | undefined>(() =>
    management.getCatalog().catch(() => undefined),
  );
  const canManageLifecycle = isDesktopApp() && status?.state !== "attached";

  const exposed = catalog?.models.filter((model) => model.exposed).length ?? 0;
  const routable = catalog?.models.filter((model) => model.exposed && model.routed).length ?? 0;
  const unrouted = exposed - routable;

  const run = async (action: () => Promise<ServerStatus>) => {
    try {
      setStatus(await action());
      setError(undefined);
    } catch (cause) {
      setError(errorMessage(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.dashboard")} subtitle={t("desktop.dashboard.subtitle")} />
      {error && <ErrorBanner message={error} />}
      <div className="grid gap-4 md:grid-cols-2">
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
              <Button size="sm" variant="outline" onClick={() => void run(api.getServerStatus)}>
                {t("desktop.dashboard.refresh")}
              </Button>
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.nextSteps")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground">{t("desktop.dashboard.nextStepsBody")}</p>
          </CardContent>
        </Card>
      </div>
      {catalog && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.catalog")}</CardTitle>
            <CardDescription>{t("desktop.dashboard.catalogDescription")}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.providerCount")}</p>
                <p className="text-2xl font-semibold">{catalog.providers.length}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.exposedCount")}</p>
                <p className="text-2xl font-semibold">{exposed}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.routableCount")}</p>
                <p className="text-2xl font-semibold">{routable}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{t("desktop.dashboard.unroutedCount")}</p>
                <p
                  className={
                    unrouted > 0 ? "text-2xl font-semibold text-amber-600 dark:text-amber-400" : "text-2xl font-semibold"
                  }
                >
                  {unrouted}
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
