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
import { PROVIDER_FORMATS, PROVIDER_PRESETS } from "../lib/constants";
import { management, useServerRequest, type AdminConfigView, type ConfigFormat } from "../lib/management";

export function ProvidersPage() {
  const { t } = useI18n();
  const { describeError } = useServerRequest();
  const [config, setConfig, loadError] = useAsyncState<AdminConfigView>(() =>
    management.getConfig().catch((cause) => {
      throw new Error(describeError(cause));
    }),
  );
  const [form, setForm] = useState({ name: "", format: "openai_responses", base_url: "", api_key: "" });
  const [preset, setPreset] = useState("custom");
  const [actionError, setActionError] = useState<string>();

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
      const current = await management.getConfig();
      let next = await management.patchConfig(current.revision, {
        upsert_providers: [
          {
            name: form.name,
            format: form.format as ConfigFormat,
            base_url: form.base_url,
          },
        ],
      });
      if (form.api_key) {
        const result = await management.putProviderSecret(next.revision, form.name, form.api_key);
        next = await management.getConfig();
        if (next.revision !== result.revision) {
          throw new Error("Configuration changed while saving the provider secret");
        }
      }
      setConfig(next);
      setForm({ name: "", format: "openai_responses", base_url: "", api_key: "" });
      setPreset("custom");
      setActionError(undefined);
    } catch (cause) {
      setActionError(describeError(cause));
    }
  };

  const remove = async (name: string) => {
    try {
      const next = await management.patchConfig(config?.revision ?? "", {
        remove_providers: [name],
      });
      await management.deleteProviderSecret(next.revision, name);
      setConfig(await management.getConfig());
      setActionError(undefined);
    } catch (cause) {
      setActionError(describeError(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.providers")} subtitle={t("desktop.providers.subtitle")} />
      {(loadError || actionError) && <ErrorBanner message={loadError ?? actionError ?? ""} />}
      {config?.restart_required && (
        <p className="text-sm text-muted-foreground">{t("desktop.config.restartRequired")}</p>
      )}
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
                    <SelectItem key={format.value} value={format.value}>
                      {format.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label={t("desktop.providers.baseUrl")} help={t("desktop.providers.baseUrlHelp")}>
              <Input value={form.base_url} onChange={(event) => setForm({ ...form, base_url: event.target.value })} />
            </Field>
            <Field label={t("desktop.providers.apiKey")} help={t("desktop.providers.apiKeyHelp")}>
              <Input
                type="password"
                value={form.api_key}
                onChange={(event) => setForm({ ...form, api_key: event.target.value })}
              />
            </Field>
          </div>
          <Button className="w-fit" onClick={() => void create()} disabled={!form.name || !form.base_url}>
            {t("desktop.providers.add")}
          </Button>
        </CardContent>
      </Card>
      <Table
        emptyText={t("desktop.providers.empty")}
        headers={[
          t("desktop.providers.name"),
          t("desktop.providers.format"),
          t("desktop.providers.baseUrl"),
          t("desktop.providers.secret"),
          "",
        ]}
        rows={(config?.providers ?? []).map((provider) => [
          provider.name,
          provider.format,
          provider.base_url,
          provider.api_key_configured ? t("desktop.config.configured") : t("desktop.config.notConfigured"),
          <Button key={provider.name} size="sm" variant="destructive" onClick={() => void remove(provider.name)}>
            {t("desktop.providers.delete")}
          </Button>,
        ])}
      />
    </section>
  );
}
