import { Outlet } from "react-router-dom";

import { Sidebar } from "./Sidebar";

export function AppShell() {
  return (
    <div className="desktop-shell">
      <Sidebar />
      <main className="main-panel">
        <Outlet />
      </main>
    </div>
  );
}
