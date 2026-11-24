/**
 * Client logos, copied from magpie's `internal/gui/assets/icons` (MIT).
 * The vendor marks themselves remain their owners' trademarks; they are
 * shipped only to identify the agent a row is about.
 *
 * magpie draws a logo ending in `-color`, or a PNG, as a picture, and every
 * other SVG as a one-tone mask so it follows the theme. The four agents
 * magpie has no logo for (`ante`, `pencil`, `openpencil`) get the generic
 * cube, exactly as magpie draws them.
 */

/** Logo base names shipped as PNGs rather than SVGs. */
const PNG_ICONS = new Set(["mothx", "crush", "snow", "atomcode", "openbot"]);

/** Agent id → logo base name. Ids absent here get the generic cube. */
const AGENT_ICON: Record<string, string> = {
  "claude-code": "claudecode-color",
  codex: "codex-color",
  droid: "factory",
  "gemini-cli": "geminicli-color",
  pi: "pi",
  omo: "omo",
  "prime-agent": "prime-agent",
  mu: "mu",
  qwen: "qwen-color",
  opencode: "opencode",
  mimocode: "mimocode",
  deveco: "deveco-color",
  vscode: "vscode",
  antigravity: "antigravity-color",
  agy: "antigravity-color",
  cursor: "cursor",
  hermes: "hermes",
  stepcode: "stepfun-color",
  mothx: "mothx",
  omp: "omp",
  morph: "mistermorph-color",
  "minimax-code": "minimax-color",
  liveagent: "liveagent-color",
  openchamber: "openchamber",
  goose: "goose",
  crush: "crush",
  openclaw: "openclaw",
  snow: "snow",
  qwenpaw: "qwenpaw",
  commandcode: "commandcode",
  fx: "fx",
  empryo: "empryo-color",
  nanobot: "nanobot-color",
  miniq: "miniq-color",
  paimon: "paimon",
  muse: "meta-color",
  qoder: "qoder",
  "qoder-cn": "qoder",
  copilot: "githubcopilot",
  "copilot-jetbrains": "githubcopilot",
  kimi: "kimi",
  grok: "xai",
  atomcode: "atomcode",
  openbot: "openbot",
  "craft-agents": "craft-agents-color",
  "tuanjie-cowork": "tuanjie-cowork-color",
  workbuddy: "workbuddy-color",
  "workbuddy-ai": "workbuddy-color",
  codebuddy: "codebuddy-color",
  "claude-science": "claude-color",
  openmausbot: "openmausbot-color",
};

const ICON_URLS = new Map(
  Object.entries(
    import.meta.glob("../assets/agent-icons/*.{svg,png}", {
      eager: true,
      query: "?url",
      import: "default",
    }) as Record<string, string>,
  ).map(([path, url]) => [path.slice(path.lastIndexOf("/") + 1), url]),
);

/** How a row draws an agent's logo. */
export type AgentIconSource =
  | { kind: "generic" }
  | { kind: "image"; url: string }
  | { kind: "mask"; url: string };

/** Resolves an agent id to its vendored logo, or the generic cube. */
export function agentIconSource(id: string): AgentIconSource {
  const name = AGENT_ICON[id];
  if (!name) return { kind: "generic" };
  if (PNG_ICONS.has(name)) {
    const url = ICON_URLS.get(`${name}.png`);
    return url ? { kind: "image", url } : { kind: "generic" };
  }
  const url = ICON_URLS.get(`${name}.svg`);
  if (!url) return { kind: "generic" };
  // A `-color` logo carries its own palette; the rest are one-tone masks.
  return { kind: name.endsWith("-color") ? "image" : "mask", url };
}
