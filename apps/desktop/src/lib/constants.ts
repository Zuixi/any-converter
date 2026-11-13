import type { ComponentType, SVGProps } from "react";
import type { TranslationKey } from "@any-converter/core";

import {
  AgentsIcon,
  DashboardIcon,
  LogsIcon,
  PlaygroundIcon,
  ProvidersIcon,
  RoutesIcon,
  SettingsIcon,
  UsageIcon,
} from "../components/layout/nav-icons";
import type { AppPath } from "../types";
import type { AuthSchemeOption } from "./management";

export const PROVIDER_FORMATS = [
  { value: "openai_responses", label: "OpenAI Responses" },
  { value: "openai_chat", label: "OpenAI Chat" },
  { value: "claude", label: "Claude" },
  { value: "gemini", label: "Gemini" },
] as const;

/**
 * How a provider key is sent upstream. Most providers follow their API
 * format; this override exists for compatible gateways whose auth header
 * differs from the wire format (e.g. MiniMax's Anthropic route wants
 * `Authorization: Bearer`).
 */
export const AUTH_SCHEME_OPTIONS: ReadonlyArray<{
  value: AuthSchemeOption;
  labelKey: TranslationKey;
}> = [
  { value: "auto", labelKey: "desktop.providers.authScheme.auto" },
  { value: "bearer", labelKey: "desktop.providers.authScheme.bearer" },
  { value: "anthropic", labelKey: "desktop.providers.authScheme.anthropic" },
  { value: "x_api_key", labelKey: "desktop.providers.authScheme.xApiKey" },
  { value: "api_key_header", labelKey: "desktop.providers.authScheme.apiKeyHeader" },
  { value: "google_api_key", labelKey: "desktop.providers.authScheme.googleApiKey" },
  { value: "none", labelKey: "desktop.providers.authScheme.none" },
];

type NavIcon = ComponentType<SVGProps<SVGSVGElement>>;

export const navItems: Array<{
  path: AppPath;
  label: TranslationKey;
  /** Rendered when the i18n catalog has no entry for `label`. */
  fallback?: string;
  Icon: NavIcon;
}> = [
  { path: "/dashboard", label: "nav.dashboard", Icon: DashboardIcon },
  { path: "/providers", label: "nav.providers", Icon: ProvidersIcon },
  { path: "/routes", label: "nav.routes", Icon: RoutesIcon },
  { path: "/agents", label: "nav.agents", Icon: AgentsIcon },
  { path: "/playground", label: "nav.playground", Icon: PlaygroundIcon },
  { path: "/logs", label: "nav.logs", Icon: LogsIcon },
  { path: "/usage", label: "nav.usage", Icon: UsageIcon },
  { path: "/settings", label: "nav.settings", Icon: SettingsIcon },
];

/** Where a preset sits in the add sheet; also its credential story. */
export type PresetKind = "vendor" | "relay";

/**
 * Known API-key providers the add sheet offers as tiles. Every base URL
 * must work with the wire's own path (`/v1/chat/completions`,
 * `/v1/messages`, `/v1/responses`) — vendors whose path differs need an
 * `endpoints` override the config API does not carry yet, so they stay out
 * until it does.
 */
export const PROVIDER_PRESETS = [
  { id: "custom", kind: "vendor", label: "Custom", name: "", format: "openai_responses", base_url: "", keys_url: "" },
  { id: "openai", kind: "vendor", label: "OpenAI", name: "openai", format: "openai_responses", base_url: "https://api.openai.com", keys_url: "https://platform.openai.com/api-keys" },
  { id: "anthropic", kind: "vendor", label: "Anthropic", name: "anthropic", format: "claude", base_url: "https://api.anthropic.com", keys_url: "https://console.anthropic.com/settings/keys" },
  { id: "gemini", kind: "vendor", label: "Google Gemini", name: "gemini", format: "gemini", base_url: "https://generativelanguage.googleapis.com", keys_url: "https://aistudio.google.com/apikey" },
  { id: "deepseek", kind: "vendor", label: "DeepSeek", name: "deepseek", format: "openai_chat", base_url: "https://api.deepseek.com", keys_url: "https://platform.deepseek.com/api_keys" },
  { id: "moonshot", kind: "vendor", label: "Moonshot Kimi", name: "moonshot", format: "openai_chat", base_url: "https://api.moonshot.cn", keys_url: "https://platform.moonshot.cn/console/api-keys" },
  { id: "minimax", kind: "vendor", label: "MiniMax", name: "minimax", format: "openai_chat", base_url: "https://api.minimaxi.com", keys_url: "https://platform.minimaxi.com/" },
  { id: "xai", kind: "vendor", label: "xAI Grok", name: "xai", format: "openai_chat", base_url: "https://api.x.ai", keys_url: "https://console.x.ai" },
  { id: "openrouter", kind: "relay", label: "OpenRouter", name: "openrouter", format: "openai_chat", base_url: "https://openrouter.ai/api", keys_url: "https://openrouter.ai/keys" },
] as const;

/** Search aliases for the add sheet's filter (localized vendor nicknames). */
export const PRESET_ALIASES: Record<string, string[]> = {
  openai: ["gpt", "chatgpt"],
  anthropic: ["claude"],
  gemini: ["google", "谷歌"],
  deepseek: ["深度求索"],
  moonshot: ["kimi", "月之暗面"],
  minimax: ["稀宇", "海螺"],
  xai: ["grok", "马斯克"],
  openrouter: ["relay", "中转"],
};

/** One subscribe-by-sign-in provider the add sheet offers. */
export type SubscriptionPreset = {
  /** The `provider` value `POST /v1/accounts/signin` takes. */
  id: string;
  label: string;
  /** Shown under the tile / in the flow; the sign-in's own way of working. */
  note: string;
};

/**
 * Built-in subscription sign-ins ("log in, no key"): the `provider` picker
 * the accounts page had, now tiles in the add sheet.
 */
export const SUBSCRIPTION_PRESETS: SubscriptionPreset[] = [
  { id: "gemini", label: "Gemini", note: "Browser sign-in with a Google account." },
  { id: "antigravity", label: "Antigravity", note: "Google may suspend an account it sees used outside Antigravity." },
  { id: "codex", label: "Codex", note: "Signs in as Codex CLI; can write back to its auth.json." },
  { id: "chatgpt-api", label: "ChatGPT", note: "Ends by pasting the address the browser landed on." },
  { id: "copilot", label: "Copilot", note: "Shows a device code to confirm on GitHub." },
  { id: "claude", label: "Claude", note: "Needs the `claude` command line installed." },
];

/** One plugin the add sheet offers to install and sign in to. */
export type PluginCatalogEntry = {
  /** npm spec handed to `POST /v1/accounts/plugins/install`. */
  spec: string;
  label: string;
  /** What signing in through it gives you. */
  note: string;
};

/**
 * The built-in plugin catalog: seeded from the specs this repo documents;
 * entries land here as the ecosystem grows, and any npm spec can be typed
 * in directly beside them.
 */
export const PLUGIN_CATALOG: PluginCatalogEntry[] = [
  {
    spec: "@magpie-community/opencode-cursor-auth",
    label: "Cursor",
    note: "Signs in with your Cursor account; serves its subscription models.",
  },
];

export const SIDEBAR_COLLAPSED_KEY = "any-converter.desktop.sidebarCollapsed";

/** Persisted theme preference; absent value means "system". */
export const THEME_STORAGE_KEY = "any-converter.desktop.theme";
