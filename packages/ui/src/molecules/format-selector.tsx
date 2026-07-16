"use client";

import { FORMATS, FORMAT_LABELS, type Format } from "@any-converter/shared";
import { Label } from "../atoms/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../atoms/select";

interface FormatSelectorProps {
  label: string;
  value: Format;
  onChange: (value: Format) => void;
  exclude?: Format[];
}

export function FormatSelector({ label, value, onChange, exclude = [] }: FormatSelectorProps) {
  const options = FORMATS.filter((format) => !exclude.includes(format));

  return (
    <div className="grid gap-2">
      <Label>{label}</Label>
      <Select value={value} onValueChange={(v) => onChange(v as Format)}>
        <SelectTrigger className="w-[180px]">
          <SelectValue placeholder="Select format" />
        </SelectTrigger>
        <SelectContent>
          {options.map((format) => (
            <SelectItem key={format} value={format}>
              {FORMAT_LABELS[format]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}
