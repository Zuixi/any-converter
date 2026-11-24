import type { AgentStatus, AgentSummary } from "./management";

/**
 * Presentation facts about a coding agent. The backend owns behaviour; this
 * only names and colours the row. New specs land here as one entry, so the
 * list never hardcodes a fixed set of agent ids.
 */
export interface AgentMeta {
  label: string;
  /** Tile tint behind the agent's logo; keeps rows distinct. */
  accent: string;
}

const META: Record<string, AgentMeta> = {
  "claude-code": { label: "Claude Code", accent: "bg-orange-500/15 text-orange-600 dark:text-orange-400" },
  codex: { label: "Codex", accent: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  "gemini-cli": { label: "Gemini CLI", accent: "bg-blue-500/15 text-blue-600 dark:text-blue-400" },
  pi: { label: "Pi", accent: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  qwen: { label: "Qwen Code", accent: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400" },
  droid: { label: "Droid", accent: "bg-rose-500/15 text-rose-600 dark:text-rose-400" },
  opencode: { label: "OpenCode", accent: "bg-teal-500/15 text-teal-600 dark:text-teal-400" },
  cursor: { label: "Cursor CLI", accent: "bg-slate-500/15 text-slate-600 dark:text-slate-300" },
  vscode: { label: "VS Code", accent: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
  antigravity: { label: "Antigravity", accent: "bg-violet-500/15 text-violet-600 dark:text-violet-400" },
  agy: { label: "Antigravity CLI", accent: "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400" },
  hermes: { label: "Hermes Agent", accent: "bg-lime-500/15 text-lime-600 dark:text-lime-400" },
  // Pi-format family
  omo: { label: "OmO", accent: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
  "prime-agent": { label: "Prime Agent", accent: "bg-purple-500/15 text-purple-600 dark:text-purple-400" },
  mu: { label: "mu", accent: "bg-cyan-500/15 text-cyan-600 dark:text-cyan-400" },
  // Pi/YAML settings family
  stepcode: { label: "StepCode", accent: "bg-yellow-500/15 text-yellow-600 dark:text-yellow-400" },
  mothx: { label: "MothX", accent: "bg-rose-500/15 text-rose-600 dark:text-rose-400" },
  omp: { label: "OMP", accent: "bg-orange-500/15 text-orange-600 dark:text-orange-400" },
  morph: { label: "Morph", accent: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  "minimax-code": { label: "MiniMax Code", accent: "bg-blue-500/15 text-blue-600 dark:text-blue-400" },
  liveagent: { label: "LiveAgent", accent: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  // OpenCode family
  mimocode: { label: "MimoCode", accent: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400" },
  deveco: { label: "DevEco", accent: "bg-teal-500/15 text-teal-600 dark:text-teal-400" },
  openchamber: { label: "OpenChamber", accent: "bg-rose-500/15 text-rose-600 dark:text-rose-400" },
  goose: { label: "Goose", accent: "bg-lime-500/15 text-lime-600 dark:text-lime-400" },
  crush: { label: "Crush", accent: "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400" },
  openclaw: { label: "OpenClaw", accent: "bg-violet-500/15 text-violet-600 dark:text-violet-400" },
  snow: { label: "Snow", accent: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
  qwenpaw: { label: "Qwenpaw", accent: "bg-cyan-500/15 text-cyan-600 dark:text-cyan-400" },
  // JSON simple settings family
  commandcode: { label: "CommandCode", accent: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  fx: { label: "FX", accent: "bg-purple-500/15 text-purple-600 dark:text-purple-400" },
  empryo: { label: "Empryo", accent: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  nanobot: { label: "Nanobot", accent: "bg-blue-500/15 text-blue-600 dark:text-blue-400" },
  miniq: { label: "MiniQ", accent: "bg-yellow-500/15 text-yellow-600 dark:text-yellow-400" },
  paimon: { label: "Paimon", accent: "bg-pink-500/15 text-pink-600 dark:text-pink-400" },
  ante: { label: "Ante", accent: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400" },
  muse: { label: "Muse", accent: "bg-teal-500/15 text-teal-600 dark:text-teal-400" },
  qoder: { label: "Qoder", accent: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
  "qoder-cn": { label: "Qoder CN", accent: "bg-red-500/15 text-red-600 dark:text-red-400" },
  copilot: { label: "Copilot CLI", accent: "bg-slate-500/15 text-slate-600 dark:text-slate-300" },
  "copilot-jetbrains": { label: "Copilot JetBrains", accent: "bg-violet-500/15 text-violet-600 dark:text-violet-400" },
  // Remaining roster
  kimi: { label: "Kimi Code", accent: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  grok: { label: "Grok CLI", accent: "bg-neutral-500/15 text-neutral-600 dark:text-neutral-300" },
  atomcode: { label: "AtomCode", accent: "bg-rose-500/15 text-rose-600 dark:text-rose-400" },
  pencil: { label: "Pencil", accent: "bg-yellow-500/15 text-yellow-600 dark:text-yellow-400" },
  openpencil: { label: "OpenPencil", accent: "bg-orange-500/15 text-orange-600 dark:text-orange-400" },
  openbot: { label: "OpenBot", accent: "bg-green-500/15 text-green-600 dark:text-green-400" },
  "craft-agents": { label: "Craft Agents", accent: "bg-blue-500/15 text-blue-600 dark:text-blue-400" },
  "tuanjie-cowork": { label: "Tuanjie Cowork", accent: "bg-red-500/15 text-red-600 dark:text-red-400" },
  workbuddy: { label: "WorkBuddy", accent: "bg-cyan-500/15 text-cyan-600 dark:text-cyan-400" },
  "workbuddy-ai": { label: "WorkBuddy AI", accent: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400" },
  codebuddy: { label: "CodeBuddy", accent: "bg-teal-500/15 text-teal-600 dark:text-teal-400" },
  "claude-science": { label: "Claude Science", accent: "bg-orange-500/15 text-orange-600 dark:text-orange-400" },
  openmausbot: { label: "OpenMausBot", accent: "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400" },
};

/** Metadata for an agent id; unknown ids fall back to the id itself. */
export function agentMeta(id: string): AgentMeta {
  return (
    META[id] ?? {
      label: id,
      accent: "bg-muted text-muted-foreground",
    }
  );
}

/** Status → dot + label tone, used by the list rows. */
export const STATUS_TONE: Record<AgentStatus, { dot: string; text: string }> = {
  connected: { dot: "bg-emerald-500", text: "text-emerald-600 dark:text-emerald-400" },
  drifted: { dot: "bg-amber-500", text: "text-amber-600 dark:text-amber-400" },
  // Installed but idle is a soft to-do: muted amber, not an alarm color.
  disconnected: { dot: "bg-amber-400/80", text: "text-amber-600/90 dark:text-amber-300/80" },
  absent: { dot: "border border-muted-foreground/50 bg-transparent", text: "text-muted-foreground" },
};

/** Row order: things in use first, then installable, then not installed. */
export const STATUS_ORDER: Record<AgentStatus, number> = {
  connected: 0,
  drifted: 1,
  disconnected: 2,
  absent: 3,
};

/** Short key suffix used in chips and detail lines. */
export function shortKeyId(id?: string): string {
  if (!id) return "";
  return id.length > 8 ? `…${id.slice(-8)}` : id;
}

/**
 * Whether a wired agent's selection depends on `provider`.
 *
 * Catalog model ids are `<provider>/<model>` (routes included), so the
 * provider name is the id's first path segment. Both the default model and
 * the per-role models count: either one being deleted makes the agent's
 * config point at a provider that no longer exists.
 */
export function agentUsesProvider(agent: AgentSummary, provider: string): boolean {
  const prefix = `${provider}/`;
  return [agent.model, ...Object.values(agent.role_models ?? {})].some(
    (id) => typeof id === "string" && id.startsWith(prefix),
  );
}

/** "2026-11-17 14:32" in the reader's locale. */
export function formatTimestamp(unixSeconds?: number): string {
  if (!unixSeconds) return "";
  return new Date(unixSeconds * 1000).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}
