"use client";

import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";

import { agentLabel, shortKeyId } from "./agent-api";
import type { AgentStatus, AgentSummary } from "./agent-api";

const STATUS_META: Record<AgentStatus, { label: string; badge: string; card: string }> = {
  connected: {
    label: "Connected",
    badge: "border-emerald-500/60 bg-emerald-500/10 text-emerald-600",
    card: "",
  },
  drifted: {
    label: "Drifted",
    badge: "border-amber-500/60 bg-amber-500/10 text-amber-600",
    card: "border-amber-500/60",
  },
  disconnected: {
    label: "Disconnected",
    badge: "border-border bg-muted text-muted-foreground",
    card: "",
  },
  absent: {
    label: "Not installed",
    badge: "border-dashed border-border bg-transparent text-muted-foreground",
    card: "opacity-60",
  },
};

interface AgentCardProps {
  agent: AgentSummary;
  busy: boolean;
  /** Opens the connect/reapply dialog. */
  onConnect: () => void;
  onDisconnect: () => void;
  onSync: () => void;
  onRotate: () => void;
}

export function AgentCard({ agent, busy, onConnect, onDisconnect, onSync, onRotate }: AgentCardProps) {
  const meta = STATUS_META[agent.status] ?? STATUS_META.disconnected;
  const keySuffix = shortKeyId(agent.key_id);
  const wired = agent.status === "connected" || agent.status === "drifted";
  const installed = agent.status !== "absent";

  return (
    <Card className={meta.card}>
      <CardHeader className="flex-row items-start justify-between space-y-0 pb-4">
        <div className="grid gap-1">
          <CardTitle className="text-xl">{agentLabel(agent.id)}</CardTitle>
          <p className="font-mono text-xs text-muted-foreground">{agent.id}</p>
        </div>
        <Badge variant="outline" className={meta.badge}>
          {meta.label}
        </Badge>
      </CardHeader>

      <CardContent className="grid gap-4">
        <dl className="grid grid-cols-2 gap-2 text-sm">
          <dt className="text-muted-foreground">Model</dt>
          <dd className="truncate text-right font-mono text-xs" title={agent.model ?? undefined}>
            {agent.model ?? "—"}
          </dd>
          <dt className="text-muted-foreground">Key</dt>
          <dd className="text-right font-mono text-xs" title={agent.key_id ?? undefined}>
            {keySuffix ? `…${keySuffix}` : "—"}
          </dd>
        </dl>

        {agent.status === "drifted" && (
          <div className="flex flex-wrap items-center gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
            <span className="text-amber-700">config changed externally</span>
            <Button size="sm" variant="outline" onClick={onConnect} disabled={busy}>
              Reapply
            </Button>
          </div>
        )}

        {agent.status === "absent" && (
          <p className="text-sm text-muted-foreground">
            not installed — no agent config directory found on this machine.
          </p>
        )}

        {installed && (
          <div className="flex flex-wrap items-center gap-2">
            {!wired && (
              <Button size="sm" onClick={onConnect} disabled={busy}>
                Connect
              </Button>
            )}
            {wired && (
              <>
                <Button size="sm" variant="outline" onClick={onSync} disabled={busy}>
                  Sync
                </Button>
                <Button size="sm" variant="outline" onClick={onRotate} disabled={busy}>
                  Rotate key
                </Button>
                <Button size="sm" variant="ghost" className="text-destructive" onClick={onDisconnect} disabled={busy}>
                  Disconnect
                </Button>
              </>
            )}
            {busy && <span className="text-xs text-muted-foreground">working…</span>}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
