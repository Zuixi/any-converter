import { useEffect, useState, type ReactNode } from "react";
import { Badge, Button } from "@any-converter/ui";
import { useI18n, type TranslationKey } from "@any-converter/core";

import { agentMeta, formatTimestamp, shortKeyId, STATUS_TONE } from "../../lib/agents";
import { AgentIcon } from "./AgentIcon";
import type {
  AgentAction as ManagementAction,
  AgentConnectOptions,
  AgentStatus,
  AgentSummary,
} from "../../lib/management";

export type AgentAction = ManagementAction;
export type AgentActionOptions = AgentConnectOptions;

/** Role slots an agent may expose; unknown roles show their raw id. */
const ROLE_LABEL_KEY: Record<string, TranslationKey> = {
  opus: "agents.role.opus",
  sonnet: "agents.role.sonnet",
  haiku: "agents.role.haiku",
  fable: "agents.role.fable",
};

const STATUS_LABEL_KEY: Record<AgentStatus, TranslationKey> = {
  connected: "agents.status.connected",
  drifted: "agents.status.drifted",
  disconnected: "agents.status.disconnected",
  absent: "agents.status.absent",
};

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
      className={`flex-none text-muted-foreground transition-transform duration-200 ${open ? "rotate-180" : ""}`}
    >
      <path d="M4 6L8 10L12 6" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-0.5">
      <dt className="text-xs uppercase tracking-wide text-muted-foreground">{label}</dt>
      <dd className="truncate text-sm">{children}</dd>
    </div>
  );
}

/** Launcher disclosure: hidden until asked because it carries the key. */
function LaunchCommand({ command }: { command: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  return (
    <details className="rounded-md border bg-muted/30 px-3 py-2">
      <summary className="cursor-pointer text-sm font-medium">{t("agents.detail.launch")}</summary>
      <p className="mt-1 text-xs text-muted-foreground">{t("agents.detail.launchHint")}</p>
      <div className="mt-2 flex items-start gap-2">
        <code className="min-w-0 flex-1 break-all rounded bg-background px-2 py-1 font-mono text-xs">
          {command}
        </code>
        <Button size="sm" variant="outline" onClick={() => void copy()}>
          {copied ? t("agents.detail.copied") : t("agents.detail.copy")}
        </Button>
      </div>
    </details>
  );
}

export function AgentRow({
  agent,
  models,
  expanded,
  busy,
  onToggle,
  onAction,
  onViewDiff,
}: {
  agent: AgentSummary;
  models: string[];
  expanded: boolean;
  busy: boolean;
  onToggle: () => void;
  onAction: (action: AgentAction, options?: AgentActionOptions) => void;
  onViewDiff: () => void;
}) {
  const { t } = useI18n();
  const meta = agentMeta(agent.id);
  const tone = STATUS_TONE[agent.status];
  const wired = agent.status === "connected" || agent.status === "drifted";
  const installed = agent.status !== "absent";
  const statusLabel = t(STATUS_LABEL_KEY[agent.status]);
  const primaryLabel =
    agent.status === "drifted"
      ? t("agents.reapply")
      : agent.status === "connected"
        ? t("agents.reconnect")
        : t("agents.connect");

  // Each row owns its model choice: the stored one when wired, the catalog's
  // first model otherwise. A successful connect refreshes `agent.model`.
  //
  // The stored model can outlive its provider (a provider deleted while this
  // agent stayed wired). Sending that dead id makes the server reject the
  // connect with `model_not_found` and the agent keeps its old config, so a
  // stored model no longer in the catalog is treated as no choice at all.
  const [selected, setSelected] = useState("");
  useEffect(() => {
    setSelected((current) => {
      if (agent.model && models.includes(agent.model) && agent.model !== current) {
        return agent.model;
      }
      if ((!current || !models.includes(current)) && models.length > 0) return models[0] ?? "";
      return current;
    });
  }, [agent.model, models]);

  // Reasoning effort and per-role models: the stored values by default, so a
  // reconnect never silently drops them. Empty effort means the model default.
  const [effort, setEffort] = useState(agent.effort ?? "");
  const [roles, setRoles] = useState<Record<string, string>>(agent.role_models ?? {});
  const roleKey = JSON.stringify(agent.role_models ?? {});

  useEffect(() => {
    setEffort((current) => {
      const stored = agent.effort ?? "";
      if (stored && stored !== current) return stored;
      return current;
    });
  }, [agent.effort]);

  useEffect(() => {
    const stored = JSON.parse(roleKey) as Record<string, string>;
    setRoles((current) => {
      const differs =
        Object.keys(stored).some((role) => stored[role] !== current[role]) ||
        Object.keys(current).some((role) => !(role in stored));
      return differs ? stored : current;
    });
  }, [roleKey]);

  // A stored per-role model can outlive its provider just like `selected`.
  // Sending one the catalog no longer lists makes the server reject the
  // whole connect, so drop those selections instead of echoing them back.
  useEffect(() => {
    setRoles((current) => {
      const valid = Object.fromEntries(
        Object.entries(current).filter(([, id]) => !id || models.includes(id)),
      );
      return Object.keys(valid).length === Object.keys(current).length ? current : valid;
    });
  }, [models]);

  const apply = () =>
    onAction("connect", {
      model: selected || undefined,
      // Omit effort for agents without levels, and when neither a stored nor
      // a picked level exists, so the agent's own default is left alone.
      effort:
        agent.efforts.length === 0 || (effort === "" && !agent.effort)
          ? undefined
          : effort,
      roleModels: agent.roles.length > 0 ? roles : undefined,
    });

  // The left rail marks the card the user is editing, not a status; status
  // lives in the dot. Background stays flat so rows read as one surface.
  return (
    <li
      className={`border-l-2 transition-colors ${
        expanded ? "border-primary/40" : "border-transparent"
      } ${installed ? "" : "opacity-70"}`}
    >
      <div className="flex items-center gap-3 px-4 py-3">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={expanded}
          aria-label={t("agents.toggleDetails").replace("{agent}", meta.label)}
          className="flex min-w-0 flex-1 items-center gap-3 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span
            aria-hidden="true"
            className={`flex h-10 w-10 flex-none items-center justify-center rounded-lg p-2 ${meta.accent}`}
          >
            <AgentIcon id={agent.id} className="h-full w-full" />
          </span>
          <span className="grid min-w-0 flex-1 gap-0.5">
            <span className="flex min-w-0 items-baseline gap-2">
              <span className="truncate font-medium">{meta.label}</span>
              <code className="truncate font-mono text-xs text-muted-foreground">{agent.id}</code>
            </span>
            <span className="flex min-w-0 items-center gap-1.5 text-xs">
              <span aria-hidden="true" className={`h-1.5 w-1.5 flex-none rounded-full ${tone.dot}`} />
              <span className={tone.text}>{statusLabel}</span>
              {agent.model && (
                <>
                  <span aria-hidden="true" className="text-muted-foreground/50">·</span>
                  <span className="truncate text-muted-foreground">{agent.model}</span>
                </>
              )}
              {agent.key_id && (
                <>
                  <span aria-hidden="true" className="text-muted-foreground/50">·</span>
                  <code className="flex-none font-mono text-muted-foreground">{shortKeyId(agent.key_id)}</code>
                </>
              )}
            </span>
          </span>
        </button>

        <div className="flex flex-none items-center gap-2">
          {agent.status === "absent" ? (
            <Badge variant="outline" className="hidden border-dashed sm:inline-flex">
              {t("agents.notInstalled")}
            </Badge>
          ) : (
            <Button
              size="sm"
              variant={wired ? "outline" : "default"}
              disabled={busy}
              onClick={apply}
            >
              {primaryLabel}
            </Button>
          )}
          <button
            type="button"
            onClick={onToggle}
            aria-expanded={expanded}
            aria-label={t("agents.toggleDetails").replace("{agent}", meta.label)}
            className="flex h-8 w-8 items-center justify-center rounded-md hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Chevron open={expanded} />
          </button>
        </div>
      </div>

      {expanded && (
        <div className="grid gap-4 border-t bg-muted/40 px-4 py-4">
          <dl className="grid gap-3 text-sm sm:grid-cols-2 lg:grid-cols-4">
            <Detail label={t("agents.detail.status")}>{statusLabel}</Detail>
            <Detail label={t("agents.detail.model")}>
              {agent.model ?? <span className="text-muted-foreground">{t("agents.noModelWired")}</span>}
            </Detail>
            <Detail label={t("agents.detail.key")}>
              {agent.key_id ? (
                <code className="font-mono text-xs">{agent.key_id}</code>
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </Detail>
            <Detail label={t("agents.detail.connectedAt")}>
              {agent.connected_at ? (
                formatTimestamp(agent.connected_at)
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </Detail>
          </dl>

          {installed && (
            <div className="grid gap-3 rounded-md border p-3">
              {models.length > 0 && (
                <label className="flex min-w-0 items-center gap-2 text-sm">
                  <span className="flex-none text-muted-foreground">{t("agents.detail.modelFor")}</span>
                  <select
                    className="min-w-0 max-w-80 flex-1 rounded-md border border-input bg-background px-2 py-1.5 text-sm"
                    value={selected}
                    disabled={busy}
                    onChange={(event) => setSelected(event.target.value)}
                  >
                    {models.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                </label>
              )}

              {agent.efforts.length > 0 && (
                <label className="flex min-w-0 items-center gap-2 text-sm">
                  <span className="flex-none text-muted-foreground">{t("agents.detail.effort")}</span>
                  <select
                    className="min-w-0 max-w-80 flex-1 rounded-md border border-input bg-background px-2 py-1.5 text-sm"
                    value={effort}
                    disabled={busy}
                    onChange={(event) => setEffort(event.target.value)}
                  >
                    <option value="">{t("agents.detail.effortDefault")}</option>
                    {agent.efforts.map((level) => (
                      <option key={level} value={level}>
                        {level}
                      </option>
                    ))}
                  </select>
                </label>
              )}

              {agent.roles.length > 0 && (
                <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
                  <p className="text-sm text-muted-foreground sm:col-span-2 lg:col-span-3">
                    {t("agents.detail.roles")}
                  </p>
                  {agent.roles.map((role) => (
                    <label key={role} className="grid gap-1 text-sm">
                      <span className="text-muted-foreground">
                        {ROLE_LABEL_KEY[role] ? t(ROLE_LABEL_KEY[role]) : role}
                      </span>
                      <select
                        className="min-w-0 rounded-md border border-input bg-background px-2 py-1.5 text-sm"
                        value={roles[role] ?? ""}
                        disabled={busy}
                        onChange={(event) =>
                          setRoles((current) => ({ ...current, [role]: event.target.value }))
                        }
                      >
                        <option value="">{t("agents.detail.roleDefault")}</option>
                        {models.map((id) => (
                          <option key={id} value={id}>
                            {id}
                          </option>
                        ))}
                      </select>
                    </label>
                  ))}
                </div>
              )}

              <div className="flex justify-end">
                <Button size="sm" variant="outline" disabled={busy} onClick={apply}>
                  {t("agents.detail.applyModel")}
                </Button>
              </div>
            </div>
          )}

          {agent.config_files.length > 0 && (
            <div className="grid gap-1">
              <p className="text-xs uppercase tracking-wide text-muted-foreground">
                {t("agents.detail.configFiles")}
              </p>
              <ul className="grid gap-0.5">
                {agent.config_files.map((path) => (
                  <li key={path} className="min-w-0">
                    <button
                      type="button"
                      onClick={onViewDiff}
                      title={t("agents.diff.view")}
                      className="max-w-full truncate rounded font-mono text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      {path}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {agent.catalog_digest && (
            <p className="truncate font-mono text-xs text-muted-foreground" title={agent.catalog_digest}>
              {t("agents.detail.catalog")}: {agent.catalog_digest.slice(0, 16)}…
            </p>
          )}

          {agent.launch && <LaunchCommand command={agent.launch} />}

          <div className="flex flex-wrap gap-2">
            {wired && (
              <>
                <Button size="sm" variant="outline" disabled={busy} onClick={() => onAction("sync")}>
                  {t("agents.sync")}
                </Button>
                <Button size="sm" variant="outline" disabled={busy} onClick={() => onAction("key/rotate")}>
                  {t("agents.rotateKey")}
                </Button>
                <Button size="sm" variant="outline" disabled={busy} onClick={() => onAction("disconnect")}>
                  {t("agents.restore")}
                </Button>
              </>
            )}
            {!installed && (
              <p className="text-sm text-muted-foreground">
                {t("agents.absentHint").replace("{agent}", meta.label)}
              </p>
            )}
          </div>
        </div>
      )}
    </li>
  );
}
