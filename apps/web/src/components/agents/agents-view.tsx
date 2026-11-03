"use client";

import { useCallback, useEffect, useState } from "react";

import { Button, Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";

import {
  agentLabel,
  connectAgent,
  disconnectAgent,
  fetchAgents,
  restoreFiles,
  rotateAgentKey,
  syncAgent,
  toAgentError,
} from "./agent-api";
import type { AgentError, AgentSummary } from "./agent-api";
import { AgentCard } from "./agent-card";
import { ConfirmDialog } from "./confirm-dialog";
import { ConnectDialog } from "./connect-dialog";

const POLL_MS = 10_000;
const TOAST_MS = 6_000;

interface Toast extends AgentError {
  kind: "success" | "error";
}

/**
 * `/agents` — one card per supported agent, polled every 10s while the tab is
 * visible. All writes go through `/api/agents/*`, which proxies to the server's
 * `/v1/agents/*` management API with the server key injected server-side.
 */
export function AgentsView() {
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<AgentError | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [connectTarget, setConnectTarget] = useState<AgentSummary | null>(null);
  const [disconnectTarget, setDisconnectTarget] = useState<AgentSummary | null>(null);

  const load = useCallback(async () => {
    try {
      const list = await fetchAgents();
      setAgents(list);
      setLoadError(null);
    } catch (error) {
      setLoadError(toAgentError(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();

    const id = window.setInterval(() => {
      if (document.visibilityState === "visible") {
        void load();
      }
    }, POLL_MS);

    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        void load();
      }
    };
    document.addEventListener("visibilitychange", onVisibilityChange);

    return () => {
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [load]);

  useEffect(() => {
    if (!toast) {
      return;
    }
    const id = window.setTimeout(() => setToast(null), TOAST_MS);
    return () => window.clearTimeout(id);
  }, [toast]);

  const showError = (error: unknown) => {
    setToast({ kind: "error", ...toAgentError(error) });
  };

  async function handleConnect(agent: AgentSummary, model: string) {
    await connectAgent(agent.id, model);
    setToast({ kind: "success", message: `${agentLabel(agent.id)} connected on ${model}.`, code: "connected" });
    await load();
  }

  async function handleDisconnect(agent: AgentSummary) {
    setDisconnectTarget(null);
    setBusyId(agent.id);
    try {
      const result = await disconnectAgent(agent.id);
      const restored = result.files_written?.length ?? 0;
      const warnings = result.warnings ?? [];
      if (warnings.length > 0) {
        setToast({
          kind: "error",
          message: `${agentLabel(agent.id)} disconnected with warnings: ${warnings.join("; ")}`,
          hint: "Restore the .before-any-converter backup manually if a file looks wrong.",
          code: "disconnect_warning",
        });
      } else {
        setToast({
          kind: "success",
          message: `${agentLabel(agent.id)} disconnected (${restored} file${restored === 1 ? "" : "s"} restored).`,
          code: "disconnected",
        });
      }
      await load();
    } catch (error) {
      showError(error);
    } finally {
      setBusyId(null);
    }
  }

  async function handleSync(agent: AgentSummary) {
    setBusyId(agent.id);
    try {
      await syncAgent(agent.id);
      setToast({ kind: "success", message: `${agentLabel(agent.id)} catalog synced.`, code: "synced" });
      await load();
    } catch (error) {
      showError(error);
    } finally {
      setBusyId(null);
    }
  }

  async function handleRotate(agent: AgentSummary) {
    setBusyId(agent.id);
    try {
      const keyId = await rotateAgentKey(agent.id);
      const suffix = keyId ? ` (…${keyId.slice(-4)})` : "";
      setToast({ kind: "success", message: `${agentLabel(agent.id)} key rotated${suffix}.`, code: "rotated" });
      await load();
    } catch (error) {
      showError(error);
    } finally {
      setBusyId(null);
    }
  }

  const filesToRestore = disconnectTarget ? restoreFiles(disconnectTarget.id) : [];

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Agents</h1>
        <p className="text-muted-foreground">
          Point local coding agents at this gateway. Wiring is written by the server; keys never leave it.
        </p>
      </div>

      {loadError && (
        <Card className="border-destructive/50">
          <CardHeader className="pb-2">
            <CardTitle className="text-base text-destructive">{loadError.message}</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {loadError.hint && <p className="text-muted-foreground">{loadError.hint}</p>}
            <p className="text-xs text-muted-foreground">code: {loadError.code}</p>
            <div>
              <Button size="sm" variant="outline" onClick={() => void load()}>
                Retry
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {loading && agents.length === 0 && !loadError && (
        <p className="text-muted-foreground">Loading agents...</p>
      )}

      {!loading && !loadError && agents.length === 0 && (
        <Card className="border-dashed">
          <CardContent className="pt-6 text-sm text-muted-foreground">
            No agents reported by the server.
          </CardContent>
        </Card>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        {agents.map((agent) => (
          <AgentCard
            key={agent.id}
            agent={agent}
            busy={busyId === agent.id}
            onConnect={() => setConnectTarget(agent)}
            onDisconnect={() => setDisconnectTarget(agent)}
            onSync={() => void handleSync(agent)}
            onRotate={() => void handleRotate(agent)}
          />
        ))}
      </div>

      {connectTarget && (
        <ConnectDialog
          agent={connectTarget}
          onClose={() => setConnectTarget(null)}
          onConnect={(model) => handleConnect(connectTarget, model)}
        />
      )}

      {disconnectTarget && (
        <ConfirmDialog
          title={`Disconnect ${agentLabel(disconnectTarget.id)}?`}
          description="The pre-connect backup is restored, so the agent stops routing through any-converter."
          confirmLabel="Disconnect"
          onCancel={() => setDisconnectTarget(null)}
          onConfirm={() => void handleDisconnect(disconnectTarget)}
        >
          <div className="grid gap-3 text-sm">
            <p className="text-muted-foreground">Files restored from the connect-time backup:</p>
            {filesToRestore.length > 0 ? (
              <ul className="grid gap-1 font-mono text-xs">
                {filesToRestore.map((file) => (
                  <li key={file} className="rounded bg-muted px-2 py-1">
                    {file}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-xs text-muted-foreground">
                The server reports the touched files in its response.
              </p>
            )}
            <p className="text-xs text-muted-foreground">
              Backups are kept as <span className="font-mono">*.before-any-converter</span> files.
            </p>
          </div>
        </ConfirmDialog>
      )}

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 w-full max-w-sm">
          <Card
            className={
              toast.kind === "success"
                ? "border-emerald-500/50 bg-emerald-500/10"
                : "border-destructive/50 bg-destructive/10"
            }
            role="status"
            aria-live="polite"
          >
            <CardContent className="grid gap-1 pt-4 text-sm">
              <div className="flex items-start justify-between gap-3">
                <p className={toast.kind === "success" ? "text-emerald-700" : "font-medium text-destructive"}>
                  {toast.message}
                </p>
                <button
                  type="button"
                  className="text-xs text-muted-foreground hover:text-foreground"
                  onClick={() => setToast(null)}
                  aria-label="Dismiss"
                >
                  ×
                </button>
              </div>
              {toast.hint && <p className="text-muted-foreground">{toast.hint}</p>}
              <p className="text-xs text-muted-foreground">code: {toast.code}</p>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}
