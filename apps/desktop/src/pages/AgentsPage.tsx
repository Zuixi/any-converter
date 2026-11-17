import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Button, Card, CardContent } from "@any-converter/ui";

import { AgentDiffModal } from "../components/agents/AgentDiffModal";
import { AgentRow, type AgentAction, type AgentActionOptions } from "../components/agents/AgentRow";
import { Header } from "../components/layout/Header";
import { agentMeta, STATUS_ORDER } from "../lib/agents";
import {
  management,
  useServerRequest,
  type AgentStatus,
  type AgentSummary,
} from "../lib/management";

/** Rows share one list; captions only split the states worth telling apart. */
const GROUPS: ReadonlyArray<{ statuses: AgentStatus[]; labelKey: "agents.group.wired" | "agents.group.available" | "agents.group.absent" }> = [
  { statuses: ["connected", "drifted"], labelKey: "agents.group.wired" },
  { statuses: ["disconnected"], labelKey: "agents.group.available" },
  { statuses: ["absent"], labelKey: "agents.group.absent" },
];

export function AgentsPage() {
  const { t } = useI18n();
  const [agents, setAgents] = useState<AgentSummary[]>([]);
  const [models, setModels] = useState<string[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [diffAgent, setDiffAgent] = useState<AgentSummary | null>(null);
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
    async (agent: AgentSummary, action: AgentAction, options?: AgentActionOptions) => {
      if (action === "disconnect" && !window.confirm(t("agents.confirm.disconnect").replace("{agent}", agentMeta(agent.id).label))) {
        return;
      }
      setBusy(`${agent.id}:${action}`);
      setNotice(undefined);
      try {
        const report = await management.agentAction(
          agent.id,
          action,
          action === "connect" ? options : undefined,
        );
        const files = report.files_written?.length ?? 0;
        setNotice(
          files > 0
            ? t("agents.notice.files")
                .replace("{agent}", agentMeta(agent.id).label)
                .replace("{status}", report.status)
                .replace("{files}", String(files))
            : t("agents.notice")
                .replace("{agent}", agentMeta(agent.id).label)
                .replace("{status}", report.status),
        );
        await refresh();
      } catch (cause) {
        setError(describeError(cause));
      } finally {
        setBusy(null);
      }
    },
    [refresh, describeError, t],
  );

  const sorted = useMemo(
    () =>
      [...agents].sort(
        (a, b) =>
          STATUS_ORDER[a.status] - STATUS_ORDER[b.status] ||
          agentMeta(a.id).label.localeCompare(agentMeta(b.id).label),
      ),
    [agents],
  );

  const hasDrift = agents.some((agent) => agent.status === "drifted");

  return (
    <>
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <Header title={t("nav.agents")} subtitle={t("agents.subtitle")} />
        <div className="flex flex-wrap items-center gap-2 pb-1">
          {models.length > 0 ? (
            <span className="text-sm text-muted-foreground">
              {t("agents.modelCount").replace("{count}", String(models.length))}
            </span>
          ) : (
            <span className="text-sm text-muted-foreground">{t("agents.noModels")}</span>
          )}
          <Button size="sm" variant="outline" onClick={() => void refresh()}>
            {t("agents.refresh")}
          </Button>
        </div>
      </div>

      {error && (
        <Card className="border-destructive/60">
          <CardContent className="pt-6 text-sm text-destructive">{error}</CardContent>
        </Card>
      )}
      {notice && (
        <Card>
          <CardContent className="pt-6 text-sm text-muted-foreground">{notice}</CardContent>
        </Card>
      )}
      {hasDrift && (
        <Card className="border-amber-500/60 dark:border-amber-500/40">
          <CardContent className="pt-6 text-sm text-amber-600 dark:text-amber-400">
            {t("agents.drift_banner")}
          </CardContent>
        </Card>
      )}

      {agents.length === 0 ? (
        <Card>
          <CardContent className="grid justify-items-center gap-1 py-10 text-center">
            <p className="font-medium">{t("agents.empty.title")}</p>
            <p className="max-w-md text-sm text-muted-foreground">{t("agents.empty.body")}</p>
          </CardContent>
        </Card>
      ) : (
        <div className="overflow-hidden rounded-lg border bg-card">
          {GROUPS.map((group) => {
            const rows = sorted.filter((agent) => group.statuses.includes(agent.status));
            if (rows.length === 0) return null;
            return (
              <section key={group.labelKey} aria-label={t(group.labelKey)}>
                <h2 className="border-b bg-muted/40 px-4 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  {t(group.labelKey)}
                </h2>
                <ul className="divide-y">
                  {rows.map((agent) => (
                    <AgentRow
                      key={agent.id}
                      agent={agent}
                      models={models}
                      expanded={openId === agent.id}
                      busy={busy?.startsWith(`${agent.id}:`) ?? false}
                      onToggle={() => setOpenId((current) => (current === agent.id ? null : agent.id))}
                      onAction={(action, options) => void act(agent, action, options)}
                      onViewDiff={() => setDiffAgent(agent)}
                    />
                  ))}
                </ul>
              </section>
            );
          })}
        </div>
      )}
    </div>
    {diffAgent && <AgentDiffModal agent={diffAgent} onClose={() => setDiffAgent(null)} />}
    </>
  );
}
