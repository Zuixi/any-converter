import { useI18n, type Language } from "@any-converter/core";

/** Languages are always shown in their own words, never translated. */
const LANGUAGES: ReadonlyArray<{ value: Language; nativeName: string }> = [
  { value: "en", nativeName: "English" },
  { value: "zh-CN", nativeName: "中文" },
];

/** Language section: one radio list, applied immediately. */
export function LanguageSection() {
  const { t, language, setLanguage } = useI18n();

  return (
    <div className="settings-section">
      <h3 className="settings-section-title">{t("common.language")}</h3>
      <div className="settings-option-list" role="radiogroup" aria-label={t("common.language")}>
        {LANGUAGES.map(({ value, nativeName }) => {
          const selected = value === language;
          return (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={selected}
              className={selected ? "settings-option selected" : "settings-option"}
              onClick={() => setLanguage(value)}
            >
              <span>{nativeName}</span>
              {selected && (
                <span className="settings-option-check" aria-hidden="true">
                  ✓
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
