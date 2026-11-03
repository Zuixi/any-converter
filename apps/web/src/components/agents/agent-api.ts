/**
 * Browser-side client for the `/api/agents` proxy routes.
 *
 * The proxy keeps the server API key server-side and relays the Rust server's
 * `{ error: { code, message, hint } }` envelope unchanged, so every failure here
 * is an `AgentApiError` carrying that code/message/hint.
 */

export type AgentStatus = "connected" | "drifted" | "disconnected" | "absent";

export interface AgentSummary {
  id: string;
  status: AgentStatus;
  model?: string;
  catalog_digest?: string;
  key_id?: string;
  connected_at?: number;
}

export interface AgentError {
  code: string;
  message: string;
  hint?: string;
}

export class AgentApiError extends Error implements AgentError {
  readonly code: string;
  readonly hint?: string;

  constructor(error: AgentError) {
    super(error.message);
    this.name = "AgentApiError";
    this.code = error.code;
    this.hint = error.hint;
  }
}

const SERVER_UNREACHABLE: AgentError = {
  code: "server_unreachable",
  message: "Cannot reach the any-converter server.",
  hint: "Start any-converter-server, or point SERVER_URL at the running server.",
};

/** Normalizes anything thrown by the client into the uniform error shape. */
export function toAgentError(error: unknown): AgentError {
  if (error instanceof AgentApiError) {
    return { code: error.code, message: error.message, hint: error.hint };
  }
  if (error instanceof Error) {
    return { code: "unexpected", message: error.message };
  }
  return { code: "unexpected", message: "Unexpected error." };
}

function readError(data: unknown, status: number): AgentError {
  const error = (data as { error?: Partial<AgentError> } | null)?.error;
  if (error && typeof error.message === "string") {
    return {
      code: typeof error.code === "string" ? error.code : "unknown",
      message: error.message,
      hint: typeof error.hint === "string" ? error.hint : undefined,
    };
  }
  return {
    code: "unknown",
    message: `Request failed with HTTP ${status}.`,
    hint: "Check the any-converter server logs for details.",
  };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      ...init,
      cache: "no-store",
      headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    });
  } catch {
    throw new AgentApiError(SERVER_UNREACHABLE);
  }

  let data: unknown = null;
  try {
    data = await res.json();
  } catch {
    data = null;
  }

  if (!res.ok) {
    throw new AgentApiError(readError(data, res.status));
  }
  return data as T;
}

export async function fetchAgents(): Promise<AgentSummary[]> {
  const data = await request<{ agents?: AgentSummary[] }>("/api/agents");
  return data.agents ?? [];
}

export async function fetchModels(): Promise<string[]> {
  const data = await request<{ models?: string[] }>("/api/agents/models");
  return data.models ?? [];
}

interface ConnectResult {
  agent: string;
  status: AgentStatus;
  files_written?: string[];
}

export async function connectAgent(id: string, model: string): Promise<ConnectResult> {
  return request<ConnectResult>(`/api/agents/${encodeURIComponent(id)}/connect`, {
    method: "POST",
    body: JSON.stringify({ model }),
  });
}

interface DisconnectResult {
  agent: string;
  status: AgentStatus;
  files_written?: string[];
  kept_backup?: string[];
  warnings?: string[];
}

export async function disconnectAgent(id: string): Promise<DisconnectResult> {
  return request<DisconnectResult>(`/api/agents/${encodeURIComponent(id)}/disconnect`, {
    method: "POST",
  });
}

export async function syncAgent(id: string): Promise<void> {
  await request(`/api/agents/${encodeURIComponent(id)}/sync`, { method: "POST" });
}

export async function rotateAgentKey(id: string): Promise<string | undefined> {
  const data = await request<{ key_id?: string }>(`/api/agents/${encodeURIComponent(id)}/rotate`, {
    method: "POST",
  });
  return data.key_id;
}

const AGENT_LABELS: Record<string, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  "gemini-cli": "Gemini CLI",
  pi: "Pi",
};

/** Human display name for an agent id. */
export function agentLabel(id: string): string {
  return AGENT_LABELS[id] ?? id;
}

/**
 * Config files `disconnect` restores from the `.before-any-converter` backups
 * taken at connect time. Mirrors `crates/agent-wiring/src/spec/*.rs`; the
 * server's disconnect response is authoritative for what it actually touched.
 */
const AGENT_RESTORE_FILES: Record<string, string[]> = {
  "claude-code": ["~/.claude/settings.json"],
  codex: ["~/.codex/config.toml", "~/.codex/auth.json", "~/.codex/any-converter-models.json"],
  "gemini-cli": ["~/.gemini/.env", "~/.gemini/settings.json"],
  pi: ["~/.pi/agent/settings.json", "~/.pi/agent/models.json"],
};

/** Files shown in the disconnect confirmation for an agent. */
export function restoreFiles(id: string): string[] {
  return AGENT_RESTORE_FILES[id] ?? [];
}

/** Last four characters of a key id, enough to tell keys apart at a glance. */
export function shortKeyId(keyId?: string): string | null {
  if (!keyId) {
    return null;
  }
  return keyId.slice(-4);
}
