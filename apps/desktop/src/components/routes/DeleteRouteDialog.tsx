import { useI18n } from "@any-converter/core";
import { Button } from "@any-converter/ui";

import { Modal } from "../providers/Modal";

export function DeleteRouteDialog({
  pattern,
  busy,
  onCancel,
  onConfirm,
}: {
  pattern: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useI18n();

  return (
    <Modal
      title={t("desktop.routes.deleteTitle").replace("{pattern}", pattern)}
      onClose={busy ? () => undefined : onCancel}
    >
      <p className="text-sm text-muted-foreground">{t("desktop.routes.deleteBody")}</p>
      <code className="block truncate rounded-md border bg-muted/30 px-3 py-2 font-mono text-sm">{pattern}</code>
      <div className="flex justify-end gap-2">
        <Button size="sm" variant="outline" disabled={busy} onClick={onCancel}>
          {t("desktop.routes.cancel")}
        </Button>
        <Button size="sm" variant="destructive" disabled={busy} onClick={onConfirm}>
          {busy ? t("desktop.routes.deleting") : t("desktop.routes.deleteConfirm")}
        </Button>
      </div>
    </Modal>
  );
}
