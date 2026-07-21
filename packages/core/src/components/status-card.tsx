"use client";

import { Card, CardContent, CardHeader, CardTitle, Badge } from "@any-converter/ui";
import { formatBytes } from "@any-converter/shared";
import type { StatusData } from "@any-converter/shared";
import { useI18n } from "../i18n";

interface StatusCardProps {
  status: StatusData;
}

export function StatusCard({ status }: StatusCardProps) {
  const { t } = useI18n();
  const isHealthy = status.health.status === "ok";

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader>
          <CardTitle>{t("status.health")}</CardTitle>
        </CardHeader>
        <CardContent>
          <Badge variant={isHealthy ? "default" : "destructive"}>{isHealthy ? t("status.healthy") : t("status.unhealthy")}</Badge>
          {status.health.error && <p className="mt-2 text-sm text-destructive">{status.health.error}</p>}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("status.disk")}</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm">
            {t("status.used")}: {formatBytes(status.disk.used_bytes)}
            {status.disk.max_bytes && (
              <>
                {" "}
                / {formatBytes(status.disk.max_bytes)} ({status.disk.percent?.toFixed(1)}%)
              </>
            )}
          </p>
        </CardContent>
      </Card>

      {status.recentErrors.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>{t("status.errors")}</CardTitle>
          </CardHeader>
          <CardContent>
            <ul className="grid gap-2 text-sm">
              {status.recentErrors.map((err, i) => (
                <li key={i} className="text-destructive">
                  {err}
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
