import { useEffect, type ReactNode } from "react";

/**
 * Lightweight modal overlay for the providers family: the desktop UI kit
 * has no dialog yet, and this page needs exactly one — a panel over a
 * dimmed page, closed by ESC, the backdrop, or the close button.
 *
 * When two modals stack (a sign-in flow over a provider detail), only the
 * top one should take ESC; the one below passes `escape={false}`.
 */
export function Modal({
  title,
  onClose,
  children,
  wide,
  escape = true,
}: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  escape?: boolean;
}) {
  useEffect(() => {
    if (!escape) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, escape]);

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center overflow-y-auto bg-black/50 p-4"
      role="dialog"
      aria-modal="true"
      onClick={onClose}
    >
      <div
        className={`grid max-h-[88vh] w-full gap-4 overflow-y-auto rounded-lg border bg-background p-6 shadow-lg ${
          wide ? "max-w-3xl" : "max-w-xl"
        }`}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0 text-base font-semibold">{title}</div>
          <button
            type="button"
            className="rounded px-2 py-1 text-sm text-muted-foreground hover:bg-muted"
            onClick={onClose}
            aria-label="close"
          >
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
