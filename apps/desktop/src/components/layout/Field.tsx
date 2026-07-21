import type React from "react";
import { Label } from "@any-converter/ui";

export function Field({
  label,
  help,
  children,
}: {
  label: string;
  help: string;
  children: React.ReactNode;
}) {
  return (
    <label className="grid gap-2">
      <Label>{label}</Label>
      {children}
      <span className="text-xs leading-5 text-muted-foreground">{help}</span>
    </label>
  );
}
