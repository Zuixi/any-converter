import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";

import {
  management,
  useServerRequest,
  type AgentStatus,
  type AgentSummary,
} from "../lib/management";

const AGENT_LABELS: Record<string, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  "gemini-cli": "Gemini CLI",
  pi: "Pi",
};

function shortKeyId(id?: string): string {
  if (!id) return "";
  return id.length > 4 ? `…${id.slice(-4)}` : id;
}

const STATUS_STYLE: Record<AgentStatus, string> = {
  connected: "border-emerald-500/60 bg-emerald-500/10 text-emerald-600",
  drifted: "border-amber-500/60 bg-amber-500/10 text-amber-600",
  disconnected: "border-border bg-muted text-muted-foreground",
  absent: "border-dashed border-border bg-transparent text-muted-foreground",
};

export function AgentsPage() {
  const { t } = useI18n();
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const visible = useRef(true);

  // Management calls travel over Tauri IPC (see useServerRequest); the
  // webview never fetches the embedded server cross-origin.
  const { describeError } = useServerRequest();

  const refresh = useCallback(async () => {
    if (!visible.current) return;
    try {
      const { agents: list, models: ids } = await management.agentsWithModels();
      setAgents(list);
      setModels(ids);
      setModel((current) => (current && ids.includes(current) ? current : (ids[0] ?? "")));
      setError(undefined);
    } catch (cause) {
      setError(describeError(cause));
    }
  }, [describeError]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 10_000);
    const onVisibility = () => {
      visible.current = document.visibilityState === "visible";
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh]);

  const act = useCallback(
    async (id: string, action: "connect" | "disconnect" | "sync" | "key/rotate", body?: object) => {
      setBusy(`${id}:${action}`);
      setNotice(undefined);
      try {
        const report = await management.agentAction(id, action, body);
        if (action === "connect" || action === "key/rotate") {
          const files = report.files_written?.length ?? 0;
          setNotice(`${id}: ${report.status}${files ? ` (${files} file(s) written)` : ""}`);
        } else {
          setNotice(`${id}: ${report.status}`);
        }
        await refresh();
      } catch (cause) {
        setError(describeError(cause));
      } finally {
        setBusy(null);
      }
    },
    [refresh, describeError],
  );

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold">{t("nav.agents")}</h1>
          <p className="text-sm text-muted-foreground">
            Connect coding agents to this gateway; disconnect restores their configs byte for byte.
          </p>
        </div>
        {models.length > 0 && (
          <label className="flex items-center gap-2 text-sm">
            <span className="text-muted-foreground">Default model</span>
            <select
              className="rounded border border-border bg-background px-2 py-1"
              value={model}
              onChange={(event) => setModel(event.target.value)}
            >
              {models.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>

      {error && (
        <Card className="border-destructive/60">
          <CardContent className="text-sm text-destructive">{error}</CardContent>
        </Card>
      )}
      {notice && (
        <Card>
          <CardContent className="text-sm text-muted-foreground">{notice}</CardContent>
        </Card>
      )}
      {agents.some((agent) => agent.status === "drifted") && (
        <Card className="border-amber-500/60">
          <CardContent className="text-sm text-amber-600">
            {t("agents.drift_banner") ||
              "Config changed externally — Reapply rewrites the wiring on top of your other changes."}
          </CardContent>
        </Card>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        {agents.map((agent) => {
          const wired = agent.status === "connected" || agent.status === "drifted";
          const installed = agent.status !== "absent";
          const isBusy = busy?.startsWith(`${agent.id}:`);
          return (
            <Card key={agent.id} className={installed ? "" : "opacity-60"}>
              <CardHeader className="flex-row items-start justify-between space-y-0 pb-4">
                <div className="grid gap-1">
                  <CardTitle className="text-xl">
                    {AGENT_LABELS[agent.id] ?? agent.id}
                  </CardTitle>
                  <p className="font-mono text-xs text-muted-foreground">
                    {agent.id}
                    {agent.key_id ? ` · ${shortKeyId(agent.key_id)}` : ""}
                  </p>
                </div>
                <Badge variant="outline" className={STATUS_STYLE[agent.status]}>
                  {agent.status}
                </Badge>
              </CardHeader>
              <CardContent className="grid gap-3">
                <p className="text-sm text-muted-foreground">
                  {agent.model ? `Model: ${agent.model}` : "No model wired"}
                </p>
                <div className="flex flex-wrap gap-2">
                  {(wired || agent.status === "drifted") && (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={isBusy || !installed}
                      onClick={() =>
                        act(agent.id, "connect", model ? { model } : undefined)
                      }
                    >
                      {agent.status === "drifted" ? "Reapply" : "Reconnect"}
                    </Button>
                  )}
                  {!wired && installed && (
                    <Button
                      size="sm"
                      disabled={isBusy || models.length === 0}
                      onClick={() =>
                        act(agent.id, "connect", model ? { model } : undefined)
                      }
                    >
                      Connect
                    </Button>
                  )}
                  {wired && (
                    <>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={isBusy}
                        onClick={() => act(agent.id, "sync")}
                      >
                        Sync
                      </Button>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={isBusy}
                        onClick={() => act(agent.id, "key/rotate")}
                      >
                        Rotate key
                      </Button>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={isBusy}
                        onClick={() => {
                          if (window.confirm("Restore this agent's pre-connect config?")) {
                            void act(agent.id, "disconnect");
                          }
                        }}
                      >
                        Disconnect
                      </Button>
                    </>
                  )}
                </div>
              </CardContent>
            </Card>
          );
        })}
      </div>
    </div>
  );
}
