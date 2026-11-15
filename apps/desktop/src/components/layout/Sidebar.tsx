import { useRef, useState } from "react";
import { NavLink } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import { useSidebarSignals, type SidebarSignals } from "../../hooks/useSidebarSignals";
import { navGroups, type NavItem } from "../../lib/constants";
import { GatewayStatus } from "./GatewayStatus";
import { BrandMark, CollapseIcon, ExpandIcon, SettingsIcon } from "./nav-icons";
import { SettingsDialog } from "../settings/SettingsDialog";

interface NavBadge {
  count: number;
  /** Amber styling + a corner dot when collapsed: something needs a look. */
  warn?: boolean;
  title?: string;
}

/** Counts worth seeing without visiting the page: connections and drift. */
function badgeForItem(
  item: NavItem,
  label: string,
  signals: SidebarSignals,
  agentsSummary: string,
): NavBadge | undefined {
  if (item.path === "/agents") {
    const { connectedAgents, driftedAgents } = signals;
    if (connectedAgents === undefined || driftedAgents === undefined) {
      return undefined;
    }
    if (driftedAgents > 0) {
      return { count: driftedAgents, warn: true, title: agentsSummary };
    }
    if (connectedAgents > 0) {
      return { count: connectedAgents, title: agentsSummary };
    }
    return undefined;
  }
  if (item.path === "/providers" && signals.providerCount !== undefined && signals.providerCount > 0) {
    return { count: signals.providerCount, title: `${label}: ${signals.providerCount}` };
  }
  return undefined;
}

export function Sidebar({
  collapsed,
  onToggleCollapsed,
}: {
  collapsed: boolean;
  onToggleCollapsed: () => void;
}) {
  const { t } = useI18n();
  const signals = useSidebarSignals();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsButtonRef = useRef<HTMLButtonElement>(null);
  const settingsLabel = t("nav.settings");

  const agentsSummary = t("desktop.sidebar.agentsSummary")
    .replace("{connected}", String(signals.connectedAgents ?? 0))
    .replace("{drifted}", String(signals.driftedAgents ?? 0));

  const closeSettings = () => {
    setSettingsOpen(false);
    settingsButtonRef.current?.focus();
  };

  return (
    <>
      <aside className={collapsed ? "sidebar collapsed" : "sidebar"}>
        <div className="sidebar-header">
          <div className="brand" title="any-converter">
            <BrandMark className="brand-mark" />
            {!collapsed && <span className="brand-name">any-converter</span>}
          </div>
          <button
            type="button"
            className="sidebar-toggle"
            onClick={onToggleCollapsed}
            title={collapsed ? t("desktop.sidebar.expand") : t("desktop.sidebar.collapse")}
            aria-label={collapsed ? t("desktop.sidebar.expand") : t("desktop.sidebar.collapse")}
            aria-expanded={!collapsed}
          >
            {collapsed ? <ExpandIcon /> : <CollapseIcon />}
          </button>
        </div>
        <GatewayStatus collapsed={collapsed} signals={signals} />
        <nav className="sidebar-nav">
          {navGroups.map((group) => (
            <div className="nav-group" role="group" aria-labelledby={group.labelKey} key={group.labelKey}>
              {/* Always rendered: visually hidden (not removed) when collapsed,
                  so screen readers keep the section structure in both modes. */}
              <div className="nav-group-label" id={group.labelKey}>
                {t(group.labelKey) || group.fallback}
              </div>
              {group.items.map((item) => {
                const { Icon } = item;
                const label = t(item.label) || item.fallback || item.label;
                const badge = badgeForItem(item, label, signals, agentsSummary);
                const title = badge?.title ?? label;
                return (
                  <NavLink
                    key={item.path}
                    to={item.path}
                    end={item.path === "/dashboard"}
                    title={title}
                    aria-label={title}
                    className={({ isActive }) => (isActive ? "nav-item active" : "nav-item")}
                  >
                    <Icon className="nav-item-icon" />
                    {!collapsed && <span className="nav-item-label">{label}</span>}
                    {!collapsed && badge && (
                      <span className={badge.warn ? "nav-item-badge warn" : "nav-item-badge"}>
                        {badge.count}
                      </span>
                    )}
                    {collapsed && badge?.warn && (
                      <span className="nav-item-badge-dot" aria-hidden="true" />
                    )}
                  </NavLink>
                );
              })}
            </div>
          ))}
        </nav>
        {/* Bottom bar: the gear opens the settings dialog (theme, language,
            gateway config) — no settings page, no inline toggles. */}
        <div className="sidebar-footer">
          <button
            ref={settingsButtonRef}
            type="button"
            className="sidebar-footer-button"
            aria-haspopup="dialog"
            title={settingsLabel}
            aria-label={settingsLabel}
            onClick={() => setSettingsOpen(true)}
          >
            <SettingsIcon />
          </button>
        </div>
      </aside>
      <SettingsDialog open={settingsOpen} onClose={closeSettings} />
    </>
  );
}
