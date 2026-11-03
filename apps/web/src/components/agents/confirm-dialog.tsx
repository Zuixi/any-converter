"use client";

import { Button } from "@any-converter/ui";

import { Modal } from "./modal";

interface ConfirmDialogProps {
  title: string;
  description?: string;
  confirmLabel: string;
  onCancel: () => void;
  onConfirm: () => void;
  children?: React.ReactNode;
}

export function ConfirmDialog({
  title,
  description,
  confirmLabel,
  onCancel,
  onConfirm,
  children,
}: ConfirmDialogProps) {
  return (
    <Modal
      title={title}
      description={description}
      onClose={onCancel}
      footer={
        <>
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="destructive" onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      {children}
    </Modal>
  );
}
