import { useState } from "react";
import { useI18n } from "@any-converter/core";
import {
  Button,
  Card,
  CardContent,
  Input,
  Label,
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
import type { DesktopProvider, DesktopRoute } from "../types";

export function RoutesPage() {
  const { t } = useI18n();
  const [routes, setRoutes, error, setError] = useAsyncState<DesktopRoute[]>(api.listModelRoutes, []);
  const [providers] = useAsyncState<DesktopProvider[]>(api.listProviders, []);
  const [form, setForm] = useState({ pattern: "*", upstream_model: "", strategy: "priority" });
  const [selectedProviderIds, setSelectedProviderIds] = useState<number[]>([]);

  const toggleProvider = (id: number) => {
    setSelectedProviderIds((current) =>
      current.includes(id) ? current.filter((value) => value !== id) : [...current, id],
    );
  };

  const create = async () => {
    try {
      const next = await api.createModelRoute({
        pattern: form.pattern,
        provider_ids: selectedProviderIds,
        upstream_model: form.upstream_model || null,
        strategy: form.strategy,
      });
      setRoutes(next);
      setForm({ pattern: "*", upstream_model: "", strategy: "priority" });
      setSelectedProviderIds([]);
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.routes")} subtitle={t("desktop.routes.subtitle")} />
      {error && <ErrorBanner message={error} />}
      <Card>
        <CardContent className="grid gap-4 pt-6">
          <div className="grid gap-4 md:grid-cols-3">
            <Field label={t("desktop.routes.pattern")} help={t("desktop.routes.patternHelp")}>
              <Input value={form.pattern} onChange={(event) => setForm({ ...form, pattern: event.target.value })} />
            </Field>
            <Field label={t("desktop.routes.upstream")} help={t("desktop.routes.upstreamHelp")}>
              <Input value={form.upstream_model} onChange={(event) => setForm({ ...form, upstream_model: event.target.value })} />
            </Field>
            <Field
              label={t("desktop.routes.strategy")}
              help={form.strategy === "priority" ? t("desktop.routes.priorityHelp") : t("desktop.routes.roundRobinHelp")}
            >
              <Select value={form.strategy} onValueChange={(strategy) => setForm({ ...form, strategy })}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="priority">{t("desktop.routes.priority")}</SelectItem>
                  <SelectItem value="round_robin">{t("desktop.routes.roundRobin")}</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
          <div className="grid gap-2">
            <Label>{t("desktop.routes.providerPool")}</Label>
            {(providers ?? []).length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("desktop.routes.noProviders")}</p>
            ) : (
              <div className="flex flex-wrap gap-4">
                {(providers ?? []).map((provider) => (
                  <label key={provider.id} className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      className="h-4 w-4 accent-primary"
                      checked={selectedProviderIds.includes(provider.id)}
                      onChange={() => toggleProvider(provider.id)}
                    />
                    {provider.name} ({provider.format})
                  </label>
                ))}
              </div>
            )}
          </div>
          <Button className="w-fit" onClick={() => void create()} disabled={selectedProviderIds.length === 0 || !form.pattern}>
            {t("desktop.routes.add")}
          </Button>
        </CardContent>
      </Card>
      <Table
        emptyText={t("desktop.routes.empty")}
        headers={[t("desktop.routes.pattern"), t("desktop.routes.providers"), t("desktop.routes.upstreamColumn"), t("desktop.routes.strategy")]}
        rows={(routes ?? []).map((route) => [route.pattern, route.providers.join(", "), route.upstream_model ?? "", route.strategy])}
      />
    </section>
  );
}
