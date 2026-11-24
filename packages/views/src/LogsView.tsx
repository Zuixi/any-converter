"use client";

import { SessionList, useI18n } from "@any-converter/core";

export function LogsView() {
  const { t } = useI18n();

  return (
    <div className="grid min-w-0 gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("logs.title")}</h1>
        <p className="text-muted-foreground">{t("logs.subtitle")}</p>
      </div>
      <SessionList />
    </div>
  );
}
