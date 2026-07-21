"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { LogTable, useI18n, useLogs } from "@any-converter/core";

export function LogsView() {
  const { t } = useI18n();
  const { records, loading, error } = useLogs();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("logs.title")}</h1>
        <p className="text-muted-foreground">{t("logs.subtitle")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("logs.timeline")}</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">{t("logs.loading")}</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && <LogTable records={records} />}
        </CardContent>
      </Card>
    </div>
  );
}
