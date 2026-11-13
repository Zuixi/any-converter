import { useSyncExternalStore } from "react";
import type { TranslationKey } from "@any-converter/core";

import { THEME_STORAGE_KEY } from "./constants";

/**
 * Theme preference as the user chose it. "system" tracks the OS scheme and
 * updates live; the two explicit values pin the theme.
 */
export type ThemePreference = "light" | "dark" | "system";
/** What is actually on screen after resolving "system" against the OS. */
export type ResolvedTheme = "light" | "dark";

/** The choices the UI offers, in cycle/selector order. */
export const THEME_OPTIONS: ReadonlyArray<{ value: ThemePreference; labelKey: TranslationKey }> = [
  { value: "light", labelKey: "common.theme.light" },
  { value: "dark", labelKey: "common.theme.dark" },
  { value: "system", labelKey: "common.theme.system" },
];

const DARK_MEDIA_QUERY = "(prefers-color-scheme: dark)";
const PREFERENCES: readonly ThemePreference[] = THEME_OPTIONS.map(({ value }) => value);

export function isThemePreference(value: unknown): value is ThemePreference {
  return typeof value === "string" && (PREFERENCES as readonly string[]).includes(value);
}

function readStoredPreference(): ThemePreference {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    return isThemePreference(stored) ? stored : "system";
  } catch {
    // localStorage can throw in restricted webviews; fall back to the OS.
    return "system";
  }
}

function persistPreference(preference: ThemePreference): void {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, preference);
  } catch {
    // ignore persistence failures in restricted environments
  }
}

function systemPrefersDark(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia(DARK_MEDIA_QUERY).matches;
}

export function resolveTheme(preference: ThemePreference): ResolvedTheme {
  if (preference === "system") {
    return systemPrefersDark() ? "dark" : "light";
  }
  return preference;
}

/** Mirrors what theme-init.js does before first paint; idempotent. */
function applyThemeToDocument(resolved: ResolvedTheme): void {
  const root = document.documentElement;
  root.classList.toggle("dark", resolved === "dark");
  root.style.colorScheme = resolved;
}

type ThemeState = {
  preference: ThemePreference;
  resolved: ResolvedTheme;
};

function initialState(): ThemeState {
  const preference = readStoredPreference();
  return { preference, resolved: resolveTheme(preference) };
}

let state: ThemeState = initialState();

const listeners = new Set<() => void>();

function emitChange(): void {
  for (const listener of listeners) {
    listener();
  }
}

/** useSyncExternalStore subscription for both preference and resolved theme. */
function subscribeToTheme(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function setThemePreference(preference: ThemePreference): void {
  persistPreference(preference);
  state = { preference, resolved: resolveTheme(preference) };
  applyThemeToDocument(state.resolved);
  emitChange();
}

/** light -> dark -> system -> light, the order shown in the UI. */
export function cycleThemePreference(current: ThemePreference): ThemePreference {
  const index = PREFERENCES.indexOf(current);
  return PREFERENCES[(index + 1) % PREFERENCES.length] ?? "light";
}

// Keep "system" live: react to OS scheme changes while unpinned.
if (typeof window.matchMedia === "function") {
  const media = window.matchMedia(DARK_MEDIA_QUERY);
  const onChange = (event: MediaQueryListEvent) => {
    if (state.preference !== "system") {
      return;
    }
    state = { preference: "system", resolved: event.matches ? "dark" : "light" };
    applyThemeToDocument(state.resolved);
    emitChange();
  };
  if (typeof media.addEventListener === "function") {
    media.addEventListener("change", onChange);
  } else if (typeof media.addListener === "function") {
    // Safari < 14 / older webviews
    media.addListener(onChange);
  }
}

export function useTheme(): {
  preference: ThemePreference;
  resolved: ResolvedTheme;
  setTheme: (preference: ThemePreference) => void;
} {
  const preference = useSyncExternalStore(
    subscribeToTheme,
    () => state.preference,
    () => "system" as const,
  );
  const resolved = useSyncExternalStore(
    subscribeToTheme,
    () => state.resolved,
    () => resolveTheme("system"),
  );
  return { preference, resolved, setTheme: setThemePreference };
}
