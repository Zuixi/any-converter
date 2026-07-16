import type { Format } from "../types";

export const FORMATS: Format[] = ["openai_chat", "openai_responses", "claude", "gemini"];

export const FORMAT_LABELS: Record<Format, string> = {
  openai_chat: "OpenAI Chat",
  openai_responses: "OpenAI Responses",
  claude: "Claude",
  gemini: "Gemini",
};

export const FORMAT_ALIASES: Record<string, Format> = {
  openai_chat: "openai_chat",
  openai: "openai_chat",
  chat: "openai_chat",
  openai_responses: "openai_responses",
  responses: "openai_responses",
  claude: "claude",
  anthropic: "claude",
  gemini: "gemini",
  google: "gemini",
};

export const API_ROUTES = {
  convert: "/api/convert",
  logs: "/api/logs",
  usage: "/api/usage",
  status: "/api/status",
  config: "/api/config",
} as const;

export const NAV_ITEMS = [
  { href: "/playground", label: "Playground" },
  { href: "/logs", label: "Logs" },
  { href: "/usage", label: "Usage" },
  { href: "/status", label: "Status" },
  { href: "/config", label: "Config" },
] as const;

export const SENSITIVE_KEYS = ["api_key", "apiKey", "authorization", "x-api-key", "x-goog-api-key"] as const;
