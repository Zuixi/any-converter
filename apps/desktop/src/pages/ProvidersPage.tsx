import { useState } from "react";
import { useI18n } from "@any-converter/core";
import {
  Button,
  Card,
  CardContent,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Field } from "../components/layout/Field";
import { Header } from "../components/layout/Header";
import { Table } from "../components/layout/Table";
import { useAsyncState } from "../hooks/useAsyncState";
import { api } from "../lib/api";
import { PROVIDER_FORMATS, PROVIDER_PRESETS } from "../lib/constants";
import type { DesktopProvider } from "../types";

export function ProvidersPage() {
  const { t } = useI18n();
  const [providers, setProviders, error, setError] = useAsyncState<DesktopProvider[]>(api.listProviders, []);
  const [form, setForm] = useState({ name: "", format: "openai_responses", base_url: "", api_key: "" });
  const [preset, setPreset] = useState("custom");

  const applyPreset = (id: string) => {
    setPreset(id);
    const next = PROVIDER_PRESETS.find((item) => item.id === id);
    if (!next || next.id === "custom") {
      return;
    }
    setForm((current) => ({
      ...current,
      name: next.name,
      format: next.format,
      base_url: next.base_url,
    }));
  };

  const create = async () => {
    try {
      setProviders(await api.createProvider(form));
      setForm({ name: "", format: "openai_responses", base_url: "", api_key: "" });
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const remove = async (id: number) => {
    try {
      setProviders(await api.deleteProvider(id));
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.providers")} subtitle={t("desktop.providers.subtitle")} />
      {error && <ErrorBanner message={error} />}
      <Card>
        <CardContent className="grid gap-4 pt-6">
          <Field label={t("desktop.providers.preset")} help={t("desktop.providers.empty")}>
            <Select value={preset} onValueChange={applyPreset}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PROVIDER_PRESETS.map((item) => (
                  <SelectItem key={item.id} value={item.id}>
                    {item.id === "custom" ? t("desktop.providers.custom") : item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          <div className="grid gap-4 md:grid-cols-2">
            <Field label={t("desktop.providers.name")} help={t("desktop.providers.nameHelp")}>
              <Input value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} />
            </Field>
            <Field label={t("desktop.providers.format")} help={t("desktop.providers.formatHelp")}>
              <Select value={form.format} onValueChange={(format) => setForm({ ...form, format })}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PROVIDER_FORMATS.map((format) => (
                    <SelectItem key={format.value} value={format.value}>{format.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={t("desktop.providers.baseUrl")} help={t("desktop.providers.baseUrlHelp")}>
              <Input value={form.base_url} onChange={(event) => setForm({ ...form, base_url: event.target.value })} />
            </Field>
            <Field label={t("desktop.providers.apiKey")} help={t("desktop.providers.apiKeyHelp")}>
              <Input type="password" value={form.api_key} onChange={(event) => setForm({ ...form, api_key: event.target.value })} />
            </Field>
          </div>
          <Button className="w-fit" onClick={() => void create()} disabled={!form.name || !form.base_url || !form.api_key}>
            {t("desktop.providers.add")}
          </Button>
        </CardContent>
      </Card>
      <Table
        emptyText={t("desktop.providers.empty")}
        headers={[t("desktop.providers.name"), t("desktop.providers.format"), t("desktop.providers.baseUrl"), t("desktop.providers.secret"), ""]}
        rows={(providers ?? []).map((provider) => [
          provider.name,
          provider.format,
          provider.base_url,
          provider.keychain_ref,
          <Button key={provider.id} size="sm" variant="destructive" onClick={() => void remove(provider.id)}>
            {t("desktop.providers.delete")}
          </Button>,
        ])}
      />
    </section>
  );
}
