"use client";

import { LogTable, useI18n, useLogs } from "@any-converter/core";

export function LogsView() {
  const { t } = useI18n();
  const { records, loading, error } = useLogs();

  return (
    <div className="grid min-w-0 gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("logs.title")}</h1>
        <p className="text-muted-foreground">{t("logs.subtitle")}</p>
      </div>

      <section className="grid min-w-0 gap-4">
        <h2 className="text-xl font-semibold">{t("logs.timeline")}</h2>
        {loading && <p className="text-muted-foreground">{t("logs.loading")}</p>}
        {error && <p className="text-destructive">{error}</p>}
        {!loading && !error && <LogTable records={records} />}
      </section>
    </div>
  );
}
