"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { ConfigEditor, useConfig, useI18n } from "@any-converter/core";

export function ConfigView() {
  const { t } = useI18n();
  const { config, raw, loading, error, saved, save } = useConfig();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">{t("config.title")}</h1>
        <p className="text-muted-foreground">{t("config.subtitle")}</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("config.card")}</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">{t("config.loading")}</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && config && (
            <ConfigEditor config={config} raw={raw} loading={loading} saved={saved} onSave={save} />
          )}
        </CardContent>
      </Card>
    </div>
  );
}
