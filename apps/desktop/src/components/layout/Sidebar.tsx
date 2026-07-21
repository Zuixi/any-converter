import { NavLink } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import { navItems } from "../../lib/constants";
import { CollapseIcon, ExpandIcon } from "./nav-icons";

function LanguageToggle({ collapsed }: { collapsed: boolean }) {
  const { language, setLanguage, t } = useI18n();
  const next = language === "en" ? "zh-CN" : "en";
  const label = language === "en" ? t("common.chinese") : t("common.english");

  return (
    <button
      type="button"
      className="nav-item"
      title={`${t("common.language")}: ${label}`}
      aria-label={`${t("common.language")}: ${label}`}
      onClick={() => setLanguage(next)}
    >
      <span className="nav-item-icon" aria-hidden="true">
        {language === "en" ? "中" : "EN"}
      </span>
      {!collapsed && (
        <span className="nav-item-label">
          {t("common.language")}: {label}
        </span>
      )}
    </button>
  );
}

export function Sidebar({
  collapsed,
  onToggleCollapsed,
}: {
  collapsed: boolean;
  onToggleCollapsed: () => void;
}) {
  const { t } = useI18n();

  return (
    <aside className={collapsed ? "sidebar collapsed" : "sidebar"}>
      <div className="sidebar-header">
        <div className="brand" title="any-converter">
          {collapsed ? "ac" : "any-converter"}
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
      <nav className="sidebar-nav">
        {navItems.map((item) => {
          const { Icon } = item;
          const label = t(item.label);
          return (
            <NavLink
              key={item.path}
              to={item.path}
              end={item.path === "/dashboard"}
              title={label}
              aria-label={label}
              className={({ isActive }) => (isActive ? "nav-item active" : "nav-item")}
            >
              <Icon className="nav-item-icon" />
              {!collapsed && <span className="nav-item-label">{label}</span>}
            </NavLink>
          );
        })}
      </nav>
      <div className="sidebar-footer">
        <LanguageToggle collapsed={collapsed} />
      </div>
    </aside>
  );
}
