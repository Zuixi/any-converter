import { useState } from "react";
import { useI18n } from "@any-converter/core";
import { Button, Card, CardContent, Input } from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { Table } from "../components/layout/Table";
import { useAsyncState } from "../hooks/useAsyncState";
import { api } from "../lib/api";

export function SettingsPage() {
  const { t } = useI18n();
  const [settings, setSettings, error, setError] = useAsyncState<Record<string, string>>(api.getSettings, {});
  const [key, setKey] = useState("server.port");
  const [value, setValue] = useState("");

  const save = async () => {
    try {
      setSettings(await api.updateSetting({ key, value }));
      setValue("");
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.settings")} subtitle={t("desktop.settings.subtitle")} />
      {error && <ErrorBanner message={error} />}
      <Card>
        <CardContent className="flex flex-wrap items-center gap-3 pt-6">
          <Input className="max-w-xs" value={key} onChange={(event) => setKey(event.target.value)} placeholder={t("desktop.settings.key")} />
          <Input className="max-w-xs" value={value} onChange={(event) => setValue(event.target.value)} placeholder={t("desktop.settings.value")} />
          <Button onClick={() => void save()}>{t("desktop.settings.save")}</Button>
        </CardContent>
      </Card>
      <Table headers={[t("desktop.settings.key"), t("desktop.settings.value")]} rows={Object.entries(settings ?? {})} />
    </section>
  );
}
