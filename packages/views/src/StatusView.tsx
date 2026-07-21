"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { StatusCard, useI18n, useStatus } from "@any-converter/core";

export function StatusView() {
  const { t } = useI18n();
  const { status, loading, error } = useStatus();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("status.title")}</h1>
        <p className="text-muted-foreground">{t("status.subtitle")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("status.card")}</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">{t("status.loading")}</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && status && <StatusCard status={status} />}
        </CardContent>
      </Card>
    </div>
  );
}
