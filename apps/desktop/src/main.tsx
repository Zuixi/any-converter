import React, { useMemo, useState } from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";

import { ApiClientProvider, I18nProvider, useI18n } from "@any-converter/core";
import type { ApiClient, TranslationKey } from "@any-converter/core";
import type { AggregatedUsage, ConvertApiRequest, RequestLogRecord, StatusData } from "@any-converter/shared";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@any-converter/ui";
import { LogsView, PlaygroundView, UsageView } from "@any-converter/views";

import "./styles.css";

type Page = "dashboard" | "providers" | "routes" | "playground" | "logs" | "usage" | "settings";

interface DesktopProvider {
  id: number;
  name: string;
  format: string;
  base_url: string;
  keychain_ref: string;
}

interface DesktopRoute {
  id: number;
  pattern: string;
  providers: string[];
  upstream_model?: string;
  strategy: string;
}

interface ServerStatus {
  state: "stopped" | "starting" | "running" | "error";
  host: string;
  port: number;
  last_error?: string;
}

const PROVIDER_FORMATS = [
  { value: "openai_responses", label: "OpenAI Responses" },
  { value: "openai_chat", label: "OpenAI Chat" },
  { value: "claude", label: "Claude" },
  { value: "gemini", label: "Gemini" },
] as const;

const navItems: Array<{ id: Page; label: TranslationKey }> = [
  { id: "dashboard", label: "nav.dashboard" },
  { id: "providers", label: "nav.providers" },
  { id: "routes", label: "nav.routes" },
  { id: "playground", label: "nav.playground" },
  { id: "logs", label: "nav.logs" },
  { id: "usage", label: "nav.usage" },
  { id: "settings", label: "nav.settings" },
];

const PROVIDER_PRESETS = [
  { id: "custom", label: "Custom", name: "", format: "openai_responses", base_url: "" },
  { id: "openai", label: "OpenAI", name: "openai", format: "openai_responses", base_url: "https://api.openai.com" },
  { id: "anthropic", label: "Anthropic", name: "anthropic", format: "claude", base_url: "https://api.anthropic.com" },
  { id: "gemini", label: "Google Gemini", name: "gemini", format: "gemini", base_url: "https://generativelanguage.googleapis.com" },
  { id: "deepseek", label: "DeepSeek", name: "deepseek", format: "openai_chat", base_url: "https://api.deepseek.com" },
  { id: "moonshot", label: "Moonshot", name: "moonshot", format: "openai_responses", base_url: "https://api.moonshot.cn" },
] as const;

function createDesktopApiClient(): ApiClient {
  return {
    async convert(request: ConvertApiRequest) {
      const result = await invoke<{ output: string }>("convert_payload", { request });
      return { output: result.output };
    },
    async getLogs() {
      return await invoke<RequestLogRecord[]>("list_request_logs", { limit: 500 });
    },
    async getUsage() {
      return await invoke<AggregatedUsage[]>("get_usage_summary", { limit: 50 });
    },
    async getConfig() {
      const settings = await invoke<Record<string, string>>("get_settings");
      return { config: { server: { host: settings["server.host"], port: Number(settings["server.port"] ?? 8080) } }, raw: JSON.stringify(settings, null, 2) };
    },
    async saveConfig(raw: string) {
      const settings = JSON.parse(raw) as Record<string, string>;
      for (const [key, value] of Object.entries(settings)) {
        await invoke("update_setting", { request: { key, value: String(value) } });
      }
    },
    async getStatus() {
      const status = await invoke<ServerStatus>("get_server_status");
      const data: StatusData = {
        health: {
          status: status.state === "running" ? "ok" : "error",
          error: status.last_error ?? (status.state === "running" ? undefined : status.state),
        },
        disk: { used_bytes: 0, max_bytes: null, percent: null },
        recentErrors: status.last_error ? [status.last_error] : [],
      };
      return data;
    },
  };
}

function App() {
  const { t } = useI18n();
  const [page, setPage] = useState<Page>("dashboard");
  const apiClient = useMemo(() => createDesktopApiClient(), []);

  return (
    <ApiClientProvider client={apiClient}>
      <div className="desktop-shell">
        <aside className="sidebar">
          <div className="brand">any-converter</div>
          <nav>
            {navItems.map((item) => (
              <button key={item.id} className={page === item.id ? "nav-item active" : "nav-item"} onClick={() => setPage(item.id)}>
                {t(item.label)}
              </button>
            ))}
          </nav>
          <LanguageToggle />
        </aside>
        <main className="main-panel">
          {page === "dashboard" && <Dashboard />}
          {page === "providers" && <Providers />}
          {page === "routes" && <Routes />}
          {page === "playground" && <PlaygroundView />}
          {page === "logs" && <LogsView />}
          {page === "usage" && <UsageView />}
          {page === "settings" && <Settings />}
        </main>
      </div>
    </ApiClientProvider>
  );
}

const statusBadgeVariant: Record<ServerStatus["state"], "default" | "secondary" | "destructive" | "outline"> = {
  running: "default",
  starting: "secondary",
  stopped: "outline",
  error: "destructive",
};

function Dashboard() {
  const { t } = useI18n();
  const [status, setStatus, error, setError] = useAsyncState<ServerStatus>("get_server_status");

  const run = async (command: "start_server" | "stop_server" | "restart_server" | "get_server_status") => {
    try {
      setStatus(await invoke<ServerStatus>(command));
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.dashboard")} subtitle={t("desktop.dashboard.subtitle")} />
      {error && <ErrorBanner message={error} />}
      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.server")}</CardTitle>
            <CardDescription>
              {status ? `${status.host}:${status.port}` : "Loading…"}
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <Badge variant={status ? statusBadgeVariant[status.state] : "secondary"} className="w-fit capitalize">
              {status?.state ?? "loading"}
            </Badge>
            {status?.last_error && <p className="text-sm text-destructive">{status.last_error}</p>}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" onClick={() => void run("start_server")}>{t("desktop.dashboard.start")}</Button>
              <Button size="sm" variant="secondary" onClick={() => void run("stop_server")}>{t("desktop.dashboard.stop")}</Button>
              <Button size="sm" variant="secondary" onClick={() => void run("restart_server")}>{t("desktop.dashboard.restart")}</Button>
              <Button size="sm" variant="outline" onClick={() => void run("get_server_status")}>{t("desktop.dashboard.refresh")}</Button>
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">{t("desktop.dashboard.nextSteps")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground">
              {t("desktop.dashboard.nextStepsBody")}
            </p>
          </CardContent>
        </Card>
      </div>
    </section>
  );
}

function Providers() {
  const { t } = useI18n();
  const [providers, setProviders, error, setError] = useAsyncState<DesktopProvider[]>("list_providers", []);
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
      setProviders(await invoke<DesktopProvider[]>("create_provider", { request: form }));
      setForm({ name: "", format: "openai_responses", base_url: "", api_key: "" });
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const remove = async (id: number) => {
    try {
      setProviders(await invoke<DesktopProvider[]>("delete_provider", { id }));
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

function Routes() {
  const { t } = useI18n();
  const [routes, setRoutes, error, setError] = useAsyncState<DesktopRoute[]>("list_model_routes", []);
  const [providers] = useAsyncState<DesktopProvider[]>("list_providers", []);
  const [form, setForm] = useState({ pattern: "*", upstream_model: "", strategy: "priority" });
  const [selectedProviderIds, setSelectedProviderIds] = useState<number[]>([]);

  const toggleProvider = (id: number) => {
    setSelectedProviderIds((current) =>
      current.includes(id) ? current.filter((value) => value !== id) : [...current, id],
    );
  };

  const create = async () => {
    try {
      const next = await invoke<DesktopRoute[]>("create_model_route", {
        request: {
          pattern: form.pattern,
          provider_ids: selectedProviderIds,
          upstream_model: form.upstream_model || null,
          strategy: form.strategy,
        },
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

function Settings() {
  const { t } = useI18n();
  const [settings, setSettings, error, setError] = useAsyncState<Record<string, string>>("get_settings", {});
  const [key, setKey] = useState("server.port");
  const [value, setValue] = useState("");

  const save = async () => {
    try {
      setSettings(await invoke<Record<string, string>>("update_setting", { request: { key, value } }));
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

function Header({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="grid gap-2">
      <h1 className="text-3xl font-bold">{title}</h1>
      <p className="text-muted-foreground">{subtitle}</p>
    </div>
  );
}

function ErrorBanner({ message }: { message: string }) {
  return <p className="rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">{message}</p>;
}

function LanguageToggle() {
  const { language, setLanguage, t } = useI18n();
  return (
    <button
      type="button"
      className="nav-item"
      onClick={() => setLanguage(language === "en" ? "zh-CN" : "en")}
    >
      {t("common.language")}: {language === "en" ? t("common.chinese") : t("common.english")}
    </button>
  );
}

function Field({ label, help, children }: { label: string; help: string; children: React.ReactNode }) {
  return (
    <label className="grid gap-2">
      <Label>{label}</Label>
      {children}
      <span className="text-xs leading-5 text-muted-foreground">{help}</span>
    </label>
  );
}

function Table({ headers, rows, emptyText }: { headers: string[]; rows: React.ReactNode[][]; emptyText?: string }) {
  return (
    <Card className="overflow-x-auto p-0">
      <table className="w-full border-collapse">
        <thead>
          <tr>
            {headers.map((header, index) => (
              <th key={index} className="border-b px-4 py-3 text-left text-xs font-semibold uppercase text-muted-foreground">
                {header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && emptyText ? (
            <tr>
              <td className="px-4 py-6 text-sm text-muted-foreground" colSpan={headers.length}>
                {emptyText}
              </td>
            </tr>
          ) : rows.map((row, index) => (
            <tr key={index}>
              {row.map((cell, cellIndex) => (
                <td key={cellIndex} className="border-b px-4 py-3 align-top text-sm last:border-b-0">
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

function useAsyncState<T>(
  command: string,
  fallback?: T,
): [T | undefined, React.Dispatch<React.SetStateAction<T | undefined>>, string | undefined, React.Dispatch<React.SetStateAction<string | undefined>>] {
  const [value, setValue] = useState<T | undefined>(fallback);
  const [error, setError] = useState<string | undefined>();
  React.useEffect(() => {
    let cancelled = false;
    invoke<T>(command)
      .then((result) => {
        if (!cancelled) {
          setValue(result);
        }
      })
      .catch((cause) => {
        if (!cancelled) {
          setError(String(cause));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [command]);
  return [value, setValue, error, setError];
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <App />
    </I18nProvider>
  </React.StrictMode>,
);
