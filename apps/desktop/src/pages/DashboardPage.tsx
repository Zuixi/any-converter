import { useI18n } from "@any-converter/core";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { useAsyncState } from "../hooks/useAsyncState";
import { api } from "../lib/api";
import type { ServerStatus } from "../types";

const statusBadgeVariant: Record<ServerStatus["state"], "default" | "secondary" | "destructive" | "outline"> = {
  running: "default",
  starting: "secondary",
  stopped: "outline",
  error: "destructive",
};

export function DashboardPage() {
  const { t } = useI18n();
  const [status, setStatus, error, setError] = useAsyncState<ServerStatus>(api.getServerStatus);

  const run = async (action: () => Promise<ServerStatus>) => {
    try {
      setStatus(await action());
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
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
            <CardDescription>
              {status ? `${status.host}:${status.port}` : "Loading…"}
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <Badge variant={status ? statusBadgeVariant[status.state] : "secondary"} className="w-fit capitalize">
              {status?.state ?? "loading"}
            </Badge>
            {status?.last_error && <p className="text-sm text-destructive">{status.last_error}</p>}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" onClick={() => void run(api.startServer)}>{t("desktop.dashboard.start")}</Button>
              <Button size="sm" variant="secondary" onClick={() => void run(api.stopServer)}>{t("desktop.dashboard.stop")}</Button>
              <Button size="sm" variant="secondary" onClick={() => void run(api.restartServer)}>{t("desktop.dashboard.restart")}</Button>
              <Button size="sm" variant="outline" onClick={() => void run(api.getServerStatus)}>{t("desktop.dashboard.refresh")}</Button>
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.nextSteps")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground">
              {t("desktop.dashboard.nextStepsBody")}
            </p>
          </CardContent>
        </Card>
      </div>
    </section>
  );
}
