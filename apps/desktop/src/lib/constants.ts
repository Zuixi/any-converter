import type { TranslationKey } from "@any-converter/core";

import type { AppPath } from "../types";

export const PROVIDER_FORMATS = [
  { value: "openai_responses", label: "OpenAI Responses" },
  { value: "openai_chat", label: "OpenAI Chat" },
  { value: "claude", label: "Claude" },
  { value: "gemini", label: "Gemini" },
] as const;

export const navItems: Array<{ path: AppPath; label: TranslationKey }> = [
  { path: "/dashboard", label: "nav.dashboard" },
  { path: "/providers", label: "nav.providers" },
  { path: "/routes", label: "nav.routes" },
  { path: "/playground", label: "nav.playground" },
  { path: "/logs", label: "nav.logs" },
  { path: "/usage", label: "nav.usage" },
  { path: "/settings", label: "nav.settings" },
];

export const PROVIDER_PRESETS = [
  { id: "custom", label: "Custom", name: "", format: "openai_responses", base_url: "" },
  { id: "openai", label: "OpenAI", name: "openai", format: "openai_responses", base_url: "https://api.openai.com" },
  { id: "anthropic", label: "Anthropic", name: "anthropic", format: "claude", base_url: "https://api.anthropic.com" },
  { id: "gemini", label: "Google Gemini", name: "gemini", format: "gemini", base_url: "https://generativelanguage.googleapis.com" },
  { id: "deepseek", label: "DeepSeek", name: "deepseek", format: "openai_chat", base_url: "https://api.deepseek.com" },
  { id: "moonshot", label: "Moonshot", name: "moonshot", format: "openai_responses", base_url: "https://api.moonshot.cn" },
] as const;
