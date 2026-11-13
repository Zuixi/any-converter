import { useMemo, useState } from "react";
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
import {
  management,
  useServerRequest,
  type AdminConfigView,
  type CatalogView,
} from "../lib/management";

export function RoutesPage() {
  const { t } = useI18n();
  const { describeError } = useServerRequest();
  const [config, setConfig, loadError] = useAsyncState<AdminConfigView>(() =>
    management.getConfig().catch((cause) => {
      throw new Error(describeError(cause));
    }),
  );
  const [catalog] = useAsyncState<CatalogView>(() =>
    management.getCatalog().catch(() => ({ revision: "", providers: [], models: [] })),
  );
  const [form, setForm] = useState({ pattern: "*", upstream_model: "", strategy: "priority" });
  const [selectedProviders, setSelectedProviders] = useState<string[]>([]);
  const [actionError, setActionError] = useState<string>();

  // Route patterns match the model ids clients send. Offer the ids the
  // gateway already exposes, so a route is never typed blind.
  const suggestions = useMemo(() => {
    const ids = new Set<string>();
    for (const model of catalog?.models ?? []) {
      if (model.exposed) {
        ids.add(model.id);
      }
    }
    for (const provider of config?.providers ?? []) {
      for (const model of Object.keys(provider.model_map)) {
        if (!model.includes("*") && !model.includes("?")) {
          ids.add(model);
        }
      }
    }
    return [...ids].sort();
  }, [catalog, config]);

  const toggleProvider = (name: string) => {
    setSelectedProviders((current) =>
      current.includes(name) ? current.filter((value) => value !== name) : [...current, name],
    );
  };

  const create = async () => {
    try {
      // The mutation's precondition must be the revision observed right
      // before the write, not one cached when the page first loaded.
      const current = await management.getConfig();
      const next = await management.patchConfig(current.revision, {
        upsert_model_routes: [
          {
            pattern: form.pattern,
            providers: selectedProviders,
            upstream_model: form.upstream_model || null,
            strategy: form.strategy === "round_robin" ? "round_robin" : "priority",
          },
        ],
      });
      setConfig(next);
      setForm({ pattern: "*", upstream_model: "", strategy: "priority" });
      setSelectedProviders([]);
      setActionError(undefined);
    } catch (cause) {
      setActionError(describeError(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.routes")} subtitle={t("desktop.routes.subtitle")} />
      {(loadError || actionError) && <ErrorBanner message={loadError ?? actionError ?? ""} />}
      {config?.restart_required && (
        <p className="text-sm text-muted-foreground">{t("desktop.config.restartRequired")}</p>
      )}
      <Card>
        <CardContent className="grid gap-4 pt-6">
          <div className="grid gap-4 md:grid-cols-3">
            <Field
              label={t("desktop.routes.pattern")}
              help={`${t("desktop.routes.patternHelp")} ${t("desktop.routes.modelSuggestionsHelp")}`}
            >
              <Input
                list="route-model-suggestions"
                value={form.pattern}
                onChange={(event) => setForm({ ...form, pattern: event.target.value })}
              />
              <datalist id="route-model-suggestions">
                {suggestions.map((model) => (
                  <option key={model} value={model} />
                ))}
              </datalist>
            </Field>
            <Field label={t("desktop.routes.upstream")} help={t("desktop.routes.upstreamHelp")}>
              <Input
                value={form.upstream_model}
                onChange={(event) => setForm({ ...form, upstream_model: event.target.value })}
              />
            </Field>
            <Field
              label={t("desktop.routes.strategy")}
              help={
                form.strategy === "priority" ? t("desktop.routes.priorityHelp") : t("desktop.routes.roundRobinHelp")
              }
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
            {(config?.providers ?? []).length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("desktop.routes.noProviders")}</p>
            ) : (
              <div className="flex flex-wrap gap-4">
                {(config?.providers ?? []).map((provider) => (
                  <label key={provider.name} className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      className="h-4 w-4 accent-primary"
                      checked={selectedProviders.includes(provider.name)}
                      onChange={() => toggleProvider(provider.name)}
                    />
                    {provider.name} ({provider.format})
                  </label>
                ))}
              </div>
            )}
          </div>
          <Button
            className="w-fit"
            onClick={() => void create()}
            disabled={selectedProviders.length === 0 || !form.pattern}
          >
            {t("desktop.routes.add")}
          </Button>
        </CardContent>
      </Card>
      <Table
        emptyText={t("desktop.routes.empty")}
        headers={[
          t("desktop.routes.pattern"),
          t("desktop.routes.providers"),
          t("desktop.routes.upstreamColumn"),
          t("desktop.routes.strategy"),
        ]}
        rows={(config?.model_routes ?? []).map((route) => [
          route.pattern,
          route.providers.join(", "),
          route.upstream_model ?? "",
          route.strategy,
        ])}
      />
    </section>
  );
}
