"use client";

import { useState } from "react";

import type { ServerConfig } from "@any-converter/shared";
import { redactJson } from "@any-converter/shared";
import { Button, Card, CardContent, CardHeader, CardTitle, Textarea } from "@any-converter/ui";
import { useI18n } from "../i18n";

interface ConfigEditorProps {
  config: ServerConfig;
  raw: string;
  loading: boolean;
  saved: boolean;
  onSave: (raw: string) => void;
}

export function ConfigEditor({ config, raw, loading, saved, onSave }: ConfigEditorProps) {
  const { t } = useI18n();
  const [editRaw, setEditRaw] = useState(raw);

  return (
    <div className="grid gap-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("config.structured")}</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Section title={t("config.server")} data={config.server ?? {}} />
          <Section title={t("config.providers")} data={redactJson(config.providers ?? [])} />
          <Section title={t("config.modelRoutes")} data={config.model_routes ?? []} />
          <Section title={t("config.legacyRoutes")} data={config.routes ?? []} />
          <Section title={t("config.logging")} data={redactJson(config.logging ?? {})} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("config.editToml")}</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Textarea value={editRaw} onChange={(e) => setEditRaw(e.target.value)} className="min-h-[400px] font-mono" />
          <div className="flex items-center gap-4">
            <Button onClick={() => onSave(editRaw)} disabled={loading}>
              {loading ? t("config.saving") : t("config.save")}
            </Button>
            {saved && <p className="text-sm text-green-600">{t("config.saved")}</p>}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function Section({ title, data }: { title: string; data: unknown }) {
  return (
    <div className="grid gap-2">
      <h3 className="font-semibold">{title}</h3>
      <pre className="overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(data, null, 2)}</pre>
    </div>
  );
}
