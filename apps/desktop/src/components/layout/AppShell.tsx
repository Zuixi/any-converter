import { useEffect, useState } from "react";
import { Outlet } from "react-router-dom";

import { SIDEBAR_COLLAPSED_KEY } from "../../lib/constants";
import { Sidebar } from "./Sidebar";

function readCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

export function AppShell() {
  const [collapsed, setCollapsed] = useState(readCollapsed);

  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_COLLAPSED_KEY, collapsed ? "1" : "0");
    } catch {
      // ignore persistence failures in restricted environments
    }
  }, [collapsed]);

  return (
    <div className={collapsed ? "desktop-shell sidebar-collapsed" : "desktop-shell"}>
      <Sidebar collapsed={collapsed} onToggleCollapsed={() => setCollapsed((value) => !value)} />
      <main className="main-panel">
        <Outlet />
      </main>
    </div>
  );
}
