"use client";

import { useEffect, useState } from "react";

import {
  Button,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@any-converter/ui";

import { agentLabel, fetchModels, toAgentError } from "./agent-api";
import type { AgentError, AgentSummary } from "./agent-api";
import { Modal } from "./modal";

interface ConnectDialogProps {
  agent: AgentSummary;
  onClose: () => void;
  /** Performs the connect (and refresh); rejects with an `AgentApiError`. */
  onConnect: (model: string) => Promise<void>;
}

/**
 * Connect / Reapply dialog: picks a model from the server catalog
 * (`GET /api/agents/models`) and writes the agent wiring through the server.
 */
export function ConnectDialog({ agent, onClose, onConnect }: ConnectDialogProps) {
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState(agent.model ?? "");
  const [loadError, setLoadError] = useState<AgentError | null>(null);
  const [submitError, setSubmitError] = useState<AgentError | null>(null);
  const [loading, setLoading] = useState(true);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let active = true;
    async function load() {
      try {
        const list = await fetchModels();
        if (!active) {
          return;
        }
        setModels(list);
        setModel((current) => (list.includes(current) ? current : (list[0] ?? "")));
      } catch (error) {
        if (active) {
          setLoadError(toAgentError(error));
        }
      } finally {
        if (active) {
          setLoading(false);
        }
      }
    }
    void load();
    return () => {
      active = false;
    };
  }, []);

  const error = submitError ?? loadError;

  async function submit() {
    if (!model) {
      return;
    }
    setSubmitting(true);
    setSubmitError(null);
    try {
      await onConnect(model);
      onClose();
    } catch (caught) {
      setSubmitError(toAgentError(caught));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Modal
      title={`Connect ${agentLabel(agent.id)}`}
      description="any-converter writes a keyed gateway entry into the agent's config so its requests route through this server."
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={submitting}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} disabled={submitting || loading || !model}>
            {submitting ? "Connecting..." : agent.status === "drifted" ? "Reapply" : "Connect"}
          </Button>
        </>
      }
    >
      <div className="grid gap-4">
        <div className="grid gap-2">
          <Label htmlFor="agent-model">Default model</Label>
          <Select value={model} onValueChange={setModel} disabled={loading || models.length === 0}>
            <SelectTrigger id="agent-model">
              <SelectValue placeholder={loading ? "Loading models..." : "Select a model"} />
            </SelectTrigger>
            <SelectContent>
              {models.map((id) => (
                <SelectItem key={id} value={id}>
                  {id}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            Models come from the server catalog (`GET /v1/models`). Add providers to the server config
            if the list is empty.
          </p>
        </div>

        {error && (
          <div className="rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm">
            <p className="font-medium text-destructive">{error.message}</p>
            {error.hint && <p className="mt-1 text-muted-foreground">{error.hint}</p>}
            <p className="mt-1 text-xs text-muted-foreground">code: {error.code}</p>
          </div>
        )}
      </div>
    </Modal>
  );
}
