"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { ConversionPlayground, useI18n } from "@any-converter/core";

export function PlaygroundView() {
  const { t } = useI18n();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("playground.title")}</h1>
        <p className="text-muted-foreground">{t("playground.subtitle")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("playground.card")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ConversionPlayground />
        </CardContent>
      </Card>
    </div>
  );
}
