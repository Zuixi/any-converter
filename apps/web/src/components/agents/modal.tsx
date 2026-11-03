"use client";

import { useEffect } from "react";

import { Card, CardDescription, CardFooter, CardHeader, CardTitle } from "@any-converter/ui";

interface ModalProps {
  title: string;
  description?: string;
  onClose: () => void;
  footer?: React.ReactNode;
  children: React.ReactNode;
}

/** Minimal accessible-enough modal: Escape and backdrop clicks close it. */
export function Modal({ title, description, onClose, footer, children }: ModalProps) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 p-4 backdrop-blur-sm"
      onClick={onClose}
      role="presentation"
    >
      <Card
        className="w-full max-w-lg"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <CardHeader>
          <CardTitle className="text-xl">{title}</CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </CardHeader>
        <div className="px-6 pb-6">{children}</div>
        {footer && <CardFooter className="justify-end gap-2 border-t pt-4">{footer}</CardFooter>}
      </Card>
    </div>
  );
}
