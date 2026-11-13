import { useMemo, useState } from "react";

import { Button, Input } from "@any-converter/ui";

export interface PickerModel {
  id: string;
  display_name?: string | null;
}

interface ModelPickerProps {
  models: PickerModel[];
  selected: string[];
  onChange: (selected: string[]) => void;
  labels: {
    search: string;
    selectAll: string;
    clear: string;
    empty: string;
    noMatches: string;
    selectedCount: string;
  };
}

/**
 * Searchable, scrollable model checklist. Used both for onboarding a new
 * provider and for editing an existing provider's exposed catalog.
 */
export function ModelPicker({ models, selected, onChange, labels }: ModelPickerProps) {
  const [search, setSearch] = useState("");
  const selectedSet = useMemo(() => new Set(selected), [selected]);

  const visible = useMemo(() => {
    const query = search.trim().toLowerCase();
    const unique = new Map<string, PickerModel>();
    for (const model of models) {
      if (!unique.has(model.id)) {
        unique.set(model.id, model);
      }
    }
    const rows = [...unique.values()].sort((left, right) => left.id.localeCompare(right.id));
    if (!query) {
      return rows;
    }
    return rows.filter(
      (model) =>
        model.id.toLowerCase().includes(query) ||
        (model.display_name ?? "").toLowerCase().includes(query),
    );
  }, [models, search]);

  const toggle = (id: string) => {
    onChange(selectedSet.has(id) ? selected.filter((value) => value !== id) : [...selected, id]);
  };

  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          className="max-w-sm"
          placeholder={labels.search}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <Button
          size="sm"
          variant="outline"
          onClick={() => onChange([...new Set([...selected, ...visible.map((model) => model.id)])])}
          disabled={visible.length === 0}
        >
          {labels.selectAll}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => onChange([])} disabled={selected.length === 0}>
          {labels.clear}
        </Button>
        <span className="text-sm text-muted-foreground">
          {selected.length} {labels.selectedCount}
        </span>
      </div>
      {models.length === 0 ? (
        <p className="text-sm text-muted-foreground">{labels.empty}</p>
      ) : visible.length === 0 ? (
        <p className="text-sm text-muted-foreground">{labels.noMatches}</p>
      ) : (
        <div className="max-h-64 overflow-y-auto rounded-md border">
          {visible.map((model) => (
            <label
              key={model.id}
              className="flex cursor-pointer items-center gap-2 border-b px-3 py-2 text-sm last:border-b-0 hover:bg-accent/40"
            >
              <input
                type="checkbox"
                className="h-4 w-4 accent-primary"
                checked={selectedSet.has(model.id)}
                onChange={() => toggle(model.id)}
              />
              <span className="font-mono">{model.id}</span>
              {model.display_name && model.display_name !== model.id && (
                <span className="text-muted-foreground">{model.display_name}</span>
              )}
            </label>
          ))}
        </div>
      )}
    </div>
  );
}
