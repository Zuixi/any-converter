import { useI18n } from "@any-converter/core";

import { THEME_OPTIONS, useTheme, type ThemePreference } from "../../lib/theme";
import { MoonIcon, SunIcon, SystemThemeIcon } from "../layout/nav-icons";

const THEME_ICONS: Record<ThemePreference, typeof SunIcon> = {
  light: SunIcon,
  dark: MoonIcon,
  system: SystemThemeIcon,
};

/** Appearance section: the theme as three tiles, applied immediately. */
export function AppearanceSection() {
  const { t } = useI18n();
  const { preference, setTheme } = useTheme();

  return (
    <div className="settings-section">
      <h3 className="settings-section-title">{t("common.theme")}</h3>
      <div className="theme-tiles" role="radiogroup" aria-label={t("common.theme")}>
        {THEME_OPTIONS.map(({ value, labelKey }) => {
          const Icon = THEME_ICONS[value];
          const selected = value === preference;
          return (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={selected}
              className={selected ? "theme-tile selected" : "theme-tile"}
              onClick={() => setTheme(value)}
            >
              <Icon />
              <span>{t(labelKey)}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
