import { useState, type DragEvent } from "react";

import { Badge, Button, cn } from "@any-converter/ui";

import { ModelCombobox } from "./ModelCombobox";
import type { ProviderLite, RouteDraft, RouteMemberDraft, UpstreamOption } from "./types";

interface RouteMemberListProps {
  providers: ProviderLite[];
  members: RouteMemberDraft[];
  strategy: RouteDraft["strategy"];
  upstreamOptions: UpstreamOption[];
  onChange: (members: RouteMemberDraft[]) => void;
  labels: {
    help: string;
    empty: string;
    addProvider: string;
    remove: string;
    moveUp: string;
    moveDown: string;
    first: string;
    fallback: string;
    primary: string;
    disabled: string;
    enabled: string;
    model: string;
    auto: string;
    search: string;
    noModels: string;
    noMatches: string;
    useCustom: string;
    weight: string;
    disable: string;
    enable: string;
    makeFallback: string;
    makePrimary: string;
  };
  disabled?: boolean;
}

/** Ordered route members; each provider can target its own upstream model. */
export function RouteMemberList({
  providers,
  members,
  strategy,
  upstreamOptions,
  onChange,
  labels,
  disabled,
}: RouteMemberListProps) {
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);
  const [addingProvider, setAddingProvider] = useState(providers[0]?.name ?? "");

  // A provider may appear more than once with different upstream model ids.
  const available = providers;
  const chosenProvider = available.some((provider) => provider.name === addingProvider)
    ? addingProvider
    : (available[0]?.name ?? "");

  const update = (index: number, patch: Partial<RouteMemberDraft>) => {
    onChange(members.map((member, current) => (current === index ? { ...member, ...patch } : member)));
  };

  const move = (from: number, to: number) => {
    if (to < 0 || to >= members.length || from === to) return;
    if (members[from]?.fallback !== members[to]?.fallback) return;
    const next = [...members];
    const [item] = next.splice(from, 1);
    if (!item) return;
    next.splice(to, 0, item);
    onChange(next);
  };

  const onDrop = (event: DragEvent, to: number) => {
    event.preventDefault();
    if (dragIndex !== null) move(dragIndex, to);
    setDragIndex(null);
    setOverIndex(null);
  };

  const add = () => {
    if (!chosenProvider) return;
    onChange([
      ...members,
      {
        key: `${chosenProvider}-${crypto.randomUUID()}`,
        provider: chosenProvider,
        model: "",
        weight: 1,
        enabled: true,
        fallback: false,
      },
    ]);
    setAddingProvider(chosenProvider);
  };

  const toggleFallback = (index: number) => {
    const next = members.map((member, current) =>
      current === index ? { ...member, fallback: !member.fallback } : member,
    );
    // Keep explicit primary/fallback ordering even when the role changes.
    onChange([...next.filter((member) => !member.fallback), ...next.filter((member) => member.fallback)]);
  };

  return (
    <div className="grid gap-3">
      <p className="text-xs leading-5 text-muted-foreground">{labels.help}</p>

      {members.length === 0 ? (
        <p className="rounded-md border border-dashed px-3 py-5 text-sm text-muted-foreground">{labels.empty}</p>
      ) : (
        <ol className="grid gap-2">
          {members.map((member, index) => {
            const provider = providers.find((candidate) => candidate.name === member.provider);
            const options = upstreamOptions.filter((option) => option.providers.includes(member.provider));
            return (
              <li
                key={member.key}
                draggable={!disabled}
                onDragStart={() => setDragIndex(index)}
                onDragOver={(event) => {
                  event.preventDefault();
                  setOverIndex(index);
                }}
                onDragLeave={() => setOverIndex((current) => (current === index ? null : current))}
                onDrop={(event) => onDrop(event, index)}
                onDragEnd={() => {
                  setDragIndex(null);
                  setOverIndex(null);
                }}
                className={cn(
                  "grid gap-3 rounded-lg border bg-background p-3 md:grid-cols-[auto_minmax(10rem,1fr)_minmax(14rem,1.3fr)_auto] md:items-center",
                  !member.enabled && "opacity-60",
                  member.fallback && "border-dashed",
                  dragIndex === index && "opacity-50",
                  overIndex === index && dragIndex !== index && "border-primary ring-1 ring-ring",
                )}
              >
                <div className="flex items-center gap-2">
                  <span className="cursor-grab select-none px-1 text-muted-foreground" aria-hidden="true">
                    ⋮⋮
                  </span>
                  <Badge variant="outline">{index + 1}</Badge>
                  {index === 0 && !member.fallback && <Badge variant="secondary">{labels.first}</Badge>}
                </div>

                <div className="flex min-w-0 items-center gap-2">
                  <span className="truncate font-medium">{member.provider}</span>
                  {provider && <Badge variant="outline">{provider.format}</Badge>}
                  <Badge variant={member.fallback ? "outline" : "secondary"}>
                    {member.fallback ? labels.fallback : labels.primary}
                  </Badge>
                  {!member.enabled && <Badge variant="destructive">{labels.disabled}</Badge>}
                </div>

                <div className="grid min-w-0 gap-1">
                  <span className="text-[11px] font-medium uppercase text-muted-foreground">{labels.model}</span>
                  <ModelCombobox
                    id={`route-member-${member.key}`}
                    value={member.model}
                    onChange={(model) => update(index, { model })}
                    options={options.map((option) => ({ value: option.value }))}
                    autoLabel={labels.auto}
                    placeholder={labels.auto}
                    searchPlaceholder={labels.search}
                    emptyText={labels.noModels}
                    noMatchesText={labels.noMatches}
                    customLabel={labels.useCustom}
                    ariaLabel={`${member.provider} ${labels.model}`}
                    disabled={disabled || !member.enabled}
                  />
                </div>

                <div className="flex flex-wrap items-center justify-end gap-1">
                  <label
                    className="flex items-center gap-1 rounded px-1.5 text-xs text-muted-foreground"
                    title={labels.weight}
                  >
                    <span>{labels.weight}</span>
                    <input
                      type="number"
                      min={1}
                      max={100}
                      value={member.weight}
                      disabled={disabled || strategy !== "weighted" || !member.enabled || member.fallback}
                      onChange={(event) => update(index, { weight: Math.max(1, Number(event.target.value) || 1) })}
                      className="h-8 w-14 rounded-md border border-input bg-background px-2 text-sm text-foreground"
                      aria-label={`${member.provider} ${labels.weight}`}
                    />
                  </label>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-8 px-2"
                    aria-label={labels.moveUp}
                    disabled={disabled || index === 0}
                    onClick={() => move(index, index - 1)}
                  >
                    ↑
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-8 px-2"
                    aria-label={labels.moveDown}
                    disabled={disabled || index === members.length - 1}
                    onClick={() => move(index, index + 1)}
                  >
                    ↓
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-8 px-2"
                    disabled={disabled}
                    onClick={() => update(index, { enabled: !member.enabled })}
                  >
                    {member.enabled ? labels.disable : labels.enable}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-8 px-2"
                    disabled={disabled}
                    onClick={() => toggleFallback(index)}
                  >
                    {member.fallback ? labels.makePrimary : labels.makeFallback}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    className="h-8 px-2 text-muted-foreground hover:text-destructive"
                    aria-label={labels.remove}
                    disabled={disabled}
                    onClick={() => onChange(members.filter((_, current) => current !== index))}
                  >
                    ✕
                  </Button>
                </div>
              </li>
            );
          })}
        </ol>
      )}

      {available.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <label className="sr-only" htmlFor="route-add-member">
            {labels.addProvider}
          </label>
          <select
            id="route-add-member"
            value={chosenProvider}
            disabled={disabled}
            onChange={(event) => setAddingProvider(event.target.value)}
            className="h-9 min-w-48 rounded-md border border-input bg-background px-3 text-sm"
          >
            {available.map((provider) => (
              <option key={provider.name} value={provider.name}>
                {provider.name} · {provider.format}
              </option>
            ))}
          </select>
          <Button type="button" variant="outline" size="sm" disabled={disabled || !addingProvider} onClick={add}>
            ＋ {labels.addProvider}
          </Button>
        </div>
      )}
    </div>
  );
}
