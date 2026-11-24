import { Button } from "@any-converter/ui";

import { agentMeta } from "../../lib/agents";
import type { AgentSummary } from "../../lib/management";
import { Modal } from "./Modal";
import { useProvidersStrings } from "./strings";

/**
 * Confirmation for deleting a provider that agents are still wired to.
 *
 * Deleting it silently would leave those agents pointing at a provider that
 * no longer exists — the failure mode behind "OpenCode still shows the old
 * model". The confirm path restores (disconnects) each agent first, so the
 * modal spells that out instead of asking a bare yes/no.
 */
export function DeleteProviderDialog({
  provider,
  agents,
  busy,
  onCancel,
  onConfirm,
}: {
  provider: string;
  agents: AgentSummary[];
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const s = useProvidersStrings();

  return (
    <Modal
      title={s.deleteInUseTitle.replace("{provider}", provider)}
      onClose={busy ? () => undefined : onCancel}
    >
      <p className="text-sm text-muted-foreground">{s.deleteInUseBody}</p>
      <ul className="grid gap-1.5 rounded-md border bg-muted/30 px-3 py-2 text-sm">
        {agents.map((agent) => (
          <li key={agent.id} className="flex min-w-0 items-baseline justify-between gap-3">
            <span className="font-medium">{agentMeta(agent.id).label}</span>
            <code className="truncate font-mono text-xs text-muted-foreground">{agent.model}</code>
          </li>
        ))}
      </ul>
      <p className="text-sm text-muted-foreground">{s.deleteInUseHint}</p>
      <div className="flex justify-end gap-2">
        <Button size="sm" variant="outline" disabled={busy} onClick={onCancel}>
          {s.cancel}
        </Button>
        <Button size="sm" variant="destructive" disabled={busy} onClick={onConfirm}>
          {busy ? s.deleting : s.deleteInUseConfirm}
        </Button>
      </div>
    </Modal>
  );
}
