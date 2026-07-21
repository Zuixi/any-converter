import { useMemo } from "react";
import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import { ApiClientProvider } from "@any-converter/core";

import { AppShell } from "./components/layout/AppShell";
import { createDesktopApiClient } from "./lib/create-desktop-api-client";
import { DashboardPage } from "./pages/DashboardPage";
import { LogsPage } from "./pages/LogsPage";
import { PlaygroundPage } from "./pages/PlaygroundPage";
import { ProvidersPage } from "./pages/ProvidersPage";
import { RoutesPage } from "./pages/RoutesPage";
import { SettingsPage } from "./pages/SettingsPage";
import { UsagePage } from "./pages/UsagePage";

export function App() {
  const apiClient = useMemo(() => createDesktopApiClient(), []);

  return (
    <ApiClientProvider client={apiClient}>
      <HashRouter>
        <Routes>
          <Route element={<AppShell />}>
            <Route index element={<Navigate to="/dashboard" replace />} />
            <Route path="dashboard" element={<DashboardPage />} />
            <Route path="providers" element={<ProvidersPage />} />
            <Route path="routes" element={<RoutesPage />} />
            <Route path="playground" element={<PlaygroundPage />} />
            <Route path="logs" element={<LogsPage />} />
            <Route path="usage" element={<UsagePage />} />
            <Route path="settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/dashboard" replace />} />
          </Route>
        </Routes>
      </HashRouter>
    </ApiClientProvider>
  );
}
