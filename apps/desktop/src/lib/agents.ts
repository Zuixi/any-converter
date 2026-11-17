import type { AgentStatus } from "./management";

/**
 * Presentation facts about a coding agent. The backend owns behaviour; this
 * only names and colours the row. New specs land here as one entry, so the
 * list never hardcodes a fixed set of agent ids.
 */
export interface AgentMeta {
  label: string;
  /** Two letters for the tile — no brand assets to ship. */
  monogram: string;
  /** Tile tint; keeps rows distinct without bundling logos. */
  accent: string;
}

const META: Record<string, AgentMeta> = {
  "claude-code": { label: "Claude Code", monogram: "CC", accent: "bg-orange-500/15 text-orange-600 dark:text-orange-400" },
  codex: { label: "Codex", monogram: "CX", accent: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  "gemini-cli": { label: "Gemini CLI", monogram: "GM", accent: "bg-blue-500/15 text-blue-600 dark:text-blue-400" },
  pi: { label: "Pi", monogram: "PI", accent: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  qwen: { label: "Qwen Code", monogram: "QW", accent: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400" },
  droid: { label: "Droid", monogram: "DR", accent: "bg-rose-500/15 text-rose-600 dark:text-rose-400" },
  opencode: { label: "OpenCode", monogram: "OC", accent: "bg-teal-500/15 text-teal-600 dark:text-teal-400" },
  cursor: { label: "Cursor CLI", monogram: "CU", accent: "bg-slate-500/15 text-slate-600 dark:text-slate-300" },
  vscode: { label: "VS Code", monogram: "VS", accent: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
  antigravity: { label: "Antigravity", monogram: "AG", accent: "bg-violet-500/15 text-violet-600 dark:text-violet-400" },
  agy: { label: "Antigravity CLI", monogram: "AC", accent: "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400" },
  hermes: { label: "Hermes Agent", monogram: "HE", accent: "bg-lime-500/15 text-lime-600 dark:text-lime-400" },
};

/** Metadata for an agent id; unknown ids fall back to the id itself. */
export function agentMeta(id: string): AgentMeta {
  return (
    META[id] ?? {
      label: id,
      monogram: id.slice(0, 2).toUpperCase(),
      accent: "bg-muted text-muted-foreground",
    }
  );
}

/** Status → dot + label tone, used by the list rows. */
export const STATUS_TONE: Record<AgentStatus, { dot: string; text: string }> = {
  connected: { dot: "bg-emerald-500", text: "text-emerald-600 dark:text-emerald-400" },
  drifted: { dot: "bg-amber-500", text: "text-amber-600 dark:text-amber-400" },
  disconnected: { dot: "bg-muted-foreground/50", text: "text-muted-foreground" },
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

/** "2026-11-17 14:32" in the reader's locale. */
export function formatTimestamp(unixSeconds?: number): string {
  if (!unixSeconds) return "";
  return new Date(unixSeconds * 1000).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}
