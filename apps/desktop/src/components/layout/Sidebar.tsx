import { NavLink } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import { navItems } from "../../lib/constants";

function LanguageToggle() {
  const { language, setLanguage, t } = useI18n();
  return (
    <button
      type="button"
      className="nav-item"
      onClick={() => setLanguage(language === "en" ? "zh-CN" : "en")}
    >
      {t("common.language")}: {language === "en" ? t("common.chinese") : t("common.english")}
    </button>
  );
}

export function Sidebar() {
  const { t } = useI18n();

  return (
    <aside className="sidebar">
      <div className="brand">any-converter</div>
      <nav>
        {navItems.map((item) => (
          <NavLink
            key={item.path}
            to={item.path}
            end={item.path === "/dashboard"}
            className={({ isActive }) => (isActive ? "nav-item active" : "nav-item")}
          >
            {t(item.label)}
          </NavLink>
        ))}
      </nav>
      <LanguageToggle />
    </aside>
  );
}
