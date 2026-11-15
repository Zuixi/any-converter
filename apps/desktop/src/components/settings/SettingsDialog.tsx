import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "@any-converter/core";

import { AppearanceSection } from "./AppearanceSection";
import { GatewaySection } from "./GatewaySection";
import { LanguageSection } from "./LanguageSection";

type SettingsSection = "appearance" | "language" | "gateway";

/**
 * The settings dialog the bottom-bar gear opens — settings live here, not on
 * a page, with their own navigation: a section rail on the left (Appearance,
 * Language, Gateway) and the selected pane on the right. Every setting
 * applies immediately; Esc, the backdrop, or ✕ closes.
 */
export function SettingsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useI18n();
  const [section, setSection] = useState<SettingsSection>("appearance");
  const dialogRef = useRef<HTMLDivElement>(null);

  // Esc closes; Tab stays inside the dialog; focus enters with the dialog.
  useEffect(() => {
    if (!open) {
      return;
    }
    const dialog = dialogRef.current;
    dialog?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
        return;
      }
      if (event.key !== "Tab") {
        return;
      }
      const focusables =
        dialog?.querySelectorAll<HTMLElement>(
          'button, input, select, textarea, a[href], [tabindex]:not([tabindex="-1"])',
        ) ?? [];
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (!first || !last) {
        return;
      }
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) {
    return null;
  }

  const sections: ReadonlyArray<{ id: SettingsSection; label: string }> = [
    { id: "appearance", label: t("desktop.preferences.appearance") },
    { id: "language", label: t("common.language") },
    { id: "gateway", label: t("desktop.preferences.gateway") },
  ];

  return createPortal(
    <div className="settings-overlay" onClick={onClose}>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("nav.settings")}
        tabIndex={-1}
        className="settings-dialog"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="settings-dialog-header">
          <div className="settings-dialog-title">{t("nav.settings")}</div>
          <button type="button" className="settings-dialog-close" onClick={onClose} aria-label="close">
            ✕
          </button>
        </div>
        <div className="settings-dialog-body">
          <nav className="settings-rail" aria-label={t("nav.settings")}>
            {sections.map(({ id, label }) => (
              <button
                key={id}
                type="button"
                aria-current={section === id}
                className={section === id ? "settings-rail-item active" : "settings-rail-item"}
                onClick={() => setSection(id)}
              >
                {label}
              </button>
            ))}
          </nav>
          <div className="settings-pane">
            {section === "appearance" && <AppearanceSection />}
            {section === "language" && <LanguageSection />}
            {section === "gateway" && <GatewaySection />}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
