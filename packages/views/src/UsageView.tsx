"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { UsageChart, useI18n, useUsage } from "@any-converter/core";

export function UsageView() {
  const { t } = useI18n();
  const { data, loading, error } = useUsage();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("usage.title")}</h1>
        <p className="text-muted-foreground">{t("usage.subtitle")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("usage.metrics")}</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">{t("usage.loading")}</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && <UsageChart data={data} />}
        </CardContent>
      </Card>
    </div>
  );
}
