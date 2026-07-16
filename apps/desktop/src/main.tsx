import React, { useMemo, useState } from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";

import { ApiClientProvider } from "@any-converter/core";
import type { ApiClient } from "@any-converter/core";
import type { AggregatedUsage, ConvertApiRequest, RequestLogRecord, StatusData } from "@any-converter/shared";
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

const navItems: Array<{ id: Page; label: string }> = [
  { id: "dashboard", label: "Dashboard" },
  { id: "providers", label: "Providers" },
  { id: "routes", label: "Routes" },
  { id: "playground", label: "Playground" },
  { id: "logs", label: "Logs" },
  { id: "usage", label: "Usage" },
  { id: "settings", label: "Settings" },
];

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
                {item.label}
              </button>
            ))}
          </nav>
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

function Dashboard() {
  const [status, setStatus] = useAsyncState<ServerStatus>("get_server_status");

  const refresh = async () => setStatus(await invoke<ServerStatus>("get_server_status"));
  const run = async (command: "start_server" | "stop_server" | "restart_server") => {
    setStatus(await invoke<ServerStatus>(command));
  };

  return (
    <section className="page">
      <Header title="Dashboard" subtitle="Embedded server control and local proxy status." />
      <div className="grid">
        <div className="panel">
          <div className="metric-label">Server</div>
          <div className={`status ${status?.state ?? "stopped"}`}>{status?.state ?? "loading"}</div>
          <div className="muted">
            {status?.host}:{status?.port}
          </div>
          {status?.last_error && <div className="error">{status.last_error}</div>}
          <div className="actions">
            <button onClick={() => void run("start_server")}>Start</button>
            <button onClick={() => void run("stop_server")}>Stop</button>
            <button onClick={() => void run("restart_server")}>Restart</button>
            <button onClick={() => void refresh()}>Refresh</button>
          </div>
        </div>
        <div className="panel">
          <div className="metric-label">Next steps</div>
          <p className="muted">Add providers, create model routes, start the embedded server, then inspect logs and usage.</p>
        </div>
      </div>
    </section>
  );
}

function Providers() {
  const [providers, setProviders] = useAsyncState<DesktopProvider[]>("list_providers", []);
  const [form, setForm] = useState({ name: "", format: "openai_responses", base_url: "", api_key: "" });

  const create = async () => {
    const next = await invoke<DesktopProvider[]>("create_provider", { request: form });
    setProviders(next);
    setForm({ name: "", format: "openai_responses", base_url: "", api_key: "" });
  };

  return (
    <section className="page">
      <Header title="Providers" subtitle="Manage upstream model providers and credentials." />
      <div className="panel form-panel">
        <input placeholder="Name" value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} />
        <select value={form.format} onChange={(event) => setForm({ ...form, format: event.target.value })}>
          <option value="openai_responses">OpenAI Responses</option>
          <option value="openai_chat">OpenAI Chat</option>
          <option value="claude">Claude</option>
          <option value="gemini">Gemini</option>
        </select>
        <input placeholder="Base URL" value={form.base_url} onChange={(event) => setForm({ ...form, base_url: event.target.value })} />
        <input placeholder="API key" type="password" value={form.api_key} onChange={(event) => setForm({ ...form, api_key: event.target.value })} />
        <button onClick={() => void create()}>Add Provider</button>
      </div>
      <Table
        headers={["Name", "Format", "Base URL", "Secret"]}
        rows={(providers ?? []).map((provider) => [provider.name, provider.format, provider.base_url, provider.keychain_ref])}
      />
    </section>
  );
}

function Routes() {
  const [routes, setRoutes] = useAsyncState<DesktopRoute[]>("list_model_routes", []);
  const [providers] = useAsyncState<DesktopProvider[]>("list_providers", []);
  const [form, setForm] = useState({ pattern: "*", provider_ids: "", upstream_model: "", strategy: "priority" });

  const create = async () => {
    const providerIds = form.provider_ids
      .split(",")
      .map((value) => Number(value.trim()))
      .filter((value) => Number.isFinite(value));
    const next = await invoke<DesktopRoute[]>("create_model_route", {
      request: { pattern: form.pattern, provider_ids: providerIds, upstream_model: form.upstream_model || null, strategy: form.strategy },
    });
    setRoutes(next);
  };

  return (
    <section className="page">
      <Header title="Routes" subtitle="Map client model patterns to provider pools." />
      <div className="panel form-panel">
        <input placeholder="Pattern, e.g. gpt-*" value={form.pattern} onChange={(event) => setForm({ ...form, pattern: event.target.value })} />
        <input placeholder="Provider IDs, comma-separated" value={form.provider_ids} onChange={(event) => setForm({ ...form, provider_ids: event.target.value })} />
        <input placeholder="Upstream model" value={form.upstream_model} onChange={(event) => setForm({ ...form, upstream_model: event.target.value })} />
        <select value={form.strategy} onChange={(event) => setForm({ ...form, strategy: event.target.value })}>
          <option value="priority">Priority</option>
          <option value="round_robin">Round robin</option>
        </select>
        <button onClick={() => void create()}>Add Route</button>
      </div>
      <p className="muted">Available providers: {(providers ?? []).map((provider) => `${provider.id}:${provider.name}`).join(", ") || "none"}</p>
      <Table
        headers={["Pattern", "Providers", "Upstream", "Strategy"]}
        rows={(routes ?? []).map((route) => [route.pattern, route.providers.join(", "), route.upstream_model ?? "", route.strategy])}
      />
    </section>
  );
}

function Settings() {
  const [settings, setSettings] = useAsyncState<Record<string, string>>("get_settings", {});
  const [key, setKey] = useState("server.port");
  const [value, setValue] = useState("");

  const save = async () => {
    const next = await invoke<Record<string, string>>("update_setting", { request: { key, value } });
    setSettings(next);
  };

  return (
    <section className="page">
      <Header title="Settings" subtitle="Configure embedded server defaults and logging." />
      <div className="panel form-panel">
        <input value={key} onChange={(event) => setKey(event.target.value)} />
        <input value={value} onChange={(event) => setValue(event.target.value)} />
        <button onClick={() => void save()}>Save Setting</button>
      </div>
      <Table headers={["Key", "Value"]} rows={Object.entries(settings ?? {})} />
    </section>
  );
}

function Header({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="page-header">
      <h1>{title}</h1>
      <p>{subtitle}</p>
    </div>
  );
}

function Table({ headers, rows }: { headers: string[]; rows: string[][] }) {
  return (
    <div className="panel table-panel">
      <table>
        <thead>
          <tr>{headers.map((header) => <th key={header}>{header}</th>)}</tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={index}>
              {row.map((cell, cellIndex) => <td key={cellIndex}>{cell}</td>)}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function useAsyncState<T>(command: string, fallback?: T): [T | undefined, React.Dispatch<React.SetStateAction<T | undefined>>] {
  const [value, setValue] = useState<T | undefined>(fallback);
  React.useEffect(() => {
    let cancelled = false;
    void invoke<T>(command).then((result) => {
      if (!cancelled) {
        setValue(result);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [command]);
  return [value, setValue];
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
