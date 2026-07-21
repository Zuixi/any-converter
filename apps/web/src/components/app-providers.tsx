"use client";

import { I18nProvider } from "@any-converter/core";

export function AppProviders({ children }: { children: React.ReactNode }) {
  return <I18nProvider>{children}</I18nProvider>;
}
