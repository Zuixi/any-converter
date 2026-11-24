import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { Input, cn } from "@any-converter/ui";

export interface ComboOption {
  value: string;
  /** Secondary text, e.g. the providers that accept this value. */
  meta?: string;
}

interface ModelComboboxProps {
  /** Unique listbox id; one per combobox instance. */
  id: string;
  value: string;
  onChange: (value: string) => void;
  options: ComboOption[];
  /** When set, the empty string is presented as this "auto" selection. */
  autoLabel?: string;
  /** Offer the typed query as a selectable entry when it matches no option. */
  allowCustom?: boolean;
  customLabel?: string;
  placeholder: string;
  searchPlaceholder: string;
  emptyText: string;
  noMatchesText: string;
  ariaLabel: string;
  disabled?: boolean;
}

/**
 * One searchable combobox used for both the client-model pattern and the
 * upstream override, so the Routes page speaks one input vocabulary.
 *
 * Typing a value that matches nothing is offered as a custom entry (patterns
 * such as `claude-*` and undiscovered model ids are both legitimate).
 */
export function ModelCombobox({
  id,
  value,
  onChange,
  options,
  autoLabel,
  allowCustom = true,
  customLabel,
  placeholder,
  searchPlaceholder,
  emptyText,
  noMatchesText,
  ariaLabel,
  disabled,
}: ModelComboboxProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [placement, setPlacement] = useState<"bottom" | "top">("bottom");
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) {
      return options;
    }
    return options.filter(
      (option) => option.value.toLowerCase().includes(needle) || (option.meta ?? "").toLowerCase().includes(needle),
    );
  }, [options, query]);

  const customValue = query.trim();
  const showCustom =
    allowCustom &&
    customValue.length > 0 &&
    !options.some((option) => option.value.toLowerCase() === customValue.toLowerCase());

  // Rows: optional auto, then filtered options, then the custom entry.
  const autoRow = autoLabel ? 1 : 0;
  const rowCount = autoRow + filtered.length + (showCustom ? 1 : 0);

  useEffect(() => {
    if (!open) {
      return;
    }
    setQuery("");
    const rect = triggerRef.current?.getBoundingClientRect();
    if (rect) {
      const below = window.innerHeight - rect.bottom;
      setPlacement(below < 320 && rect.top > below ? "top" : "bottom");
    }
    const selected = options.findIndex((option) => option.value === value);
    setActive(value === "" ? 0 : selected + autoRow);
    window.requestAnimationFrame(() => searchRef.current?.focus());
    // Only reset when the popover opens; option identity churns every render.
  }, [open]);

  // Keep the highlight in range when the filtered list shrinks.
  useEffect(() => {
    setActive((current) => Math.min(current, Math.max(rowCount - 1, 0)));
  }, [rowCount]);

  useEffect(() => {
    if (!open) {
      return;
    }
    const onPointerDown = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [open]);

  useEffect(() => {
    if (!open) {
      return;
    }
    const row = listRef.current?.querySelector<HTMLElement>(`[data-row="${active}"]`);
    row?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  const close = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const choose = (next: string) => {
    onChange(next);
    close();
  };

  const commitActive = () => {
    if (autoLabel && active === 0) {
      choose("");
      return;
    }
    const option = filtered[active - autoRow];
    if (option) {
      choose(option.value);
      return;
    }
    if (showCustom) {
      choose(customValue);
    }
  };

  const onSearchKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActive((current) => Math.min(current + 1, rowCount - 1));
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((current) => Math.max(current - 1, 0));
        break;
      case "Home":
        event.preventDefault();
        setActive(0);
        break;
      case "End":
        event.preventDefault();
        setActive(Math.max(rowCount - 1, 0));
        break;
      case "Enter":
        event.preventDefault();
        commitActive();
        break;
      case "Escape":
        event.preventDefault();
        close();
        break;
      case "Tab":
        setOpen(false);
        break;
    }
  };

  const rowClass = (selected: boolean, isActive: boolean) =>
    cn(
      "flex w-full cursor-pointer items-center gap-3 rounded-sm px-2 py-2 text-left text-sm outline-none",
      isActive && "bg-accent text-accent-foreground",
      selected && !isActive && "bg-accent/40",
    );

  const check = (visible: boolean) => (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={cn("h-4 w-4 shrink-0", visible ? "opacity-100" : "opacity-0")}
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );

  const listboxId = `${id}-listbox`;
  const display = value === "" ? autoLabel : value;

  return (
    <div ref={rootRef} className="relative">
      <button
        ref={triggerRef}
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        disabled={disabled}
        onClick={() => setOpen((current) => !current)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          } else if (event.key === "Escape" && open) {
            event.preventDefault();
            close();
          }
        }}
        className={cn(
          "flex h-10 w-full items-center justify-between gap-2 rounded-md border border-input bg-background px-3 py-2 text-left text-sm ring-offset-background",
          "hover:bg-accent/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
          "disabled:cursor-not-allowed disabled:opacity-50",
          open && "ring-2 ring-ring ring-offset-2",
        )}
      >
        <span className={cn("truncate", (value === "" || display === undefined) && "text-muted-foreground")}>
          {display === undefined ? (
            <span className="text-muted-foreground">{placeholder}</span>
          ) : value === "" ? (
            autoLabel
          ) : (
            <span className="font-mono">{value}</span>
          )}
        </span>
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={cn("h-4 w-4 shrink-0 opacity-50 transition-transform", open && "rotate-180")}
          aria-hidden="true"
        >
          <path d="m7 15 5 5 5-5" />
          <path d="m7 9 5-5 5 5" />
        </svg>
      </button>

      {open && (
        <div
          className={cn(
            "absolute z-50 w-full min-w-[22rem] max-w-[calc(100vw-2rem)] overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-md",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
            placement === "bottom" ? "top-full mt-1" : "bottom-full mb-1",
          )}
          data-state="open"
        >
          <div className="flex items-center gap-2 border-b px-3">
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              className="h-4 w-4 shrink-0 text-muted-foreground"
              aria-hidden="true"
            >
              <circle cx="11" cy="11" r="8" />
              <path d="m21 21-4.3-4.3" />
            </svg>
            <Input
              ref={searchRef}
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setActive(0);
              }}
              onKeyDown={onSearchKeyDown}
              placeholder={searchPlaceholder}
              aria-label={searchPlaceholder}
              role="combobox"
              aria-expanded
              aria-controls={listboxId}
              aria-autocomplete="list"
              className="h-9 border-0 bg-transparent px-0 shadow-none focus-visible:ring-0 focus-visible:ring-offset-0"
            />
          </div>

          <div
            ref={listRef}
            id={listboxId}
            role="listbox"
            aria-label={ariaLabel}
            className="max-h-64 overflow-y-auto p-1"
          >
            {autoLabel && (
              <button
                type="button"
                role="option"
                aria-selected={value === ""}
                data-row={0}
                onMouseEnter={() => setActive(0)}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => choose("")}
                className={rowClass(value === "", active === 0)}
              >
                {check(value === "")}
                <span className="flex-1 truncate">{autoLabel}</span>
              </button>
            )}

            {filtered.map((option, index) => {
              const row = index + autoRow;
              const selected = option.value === value;
              return (
                <button
                  key={option.value}
                  type="button"
                  role="option"
                  aria-selected={selected}
                  data-row={row}
                  onMouseEnter={() => setActive(row)}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => choose(option.value)}
                  className={rowClass(selected, active === row)}
                >
                  {check(selected)}
                  <span className="min-w-0 flex-1 truncate font-mono">{option.value}</span>
                  {option.meta && (
                    <span className="shrink-0 whitespace-nowrap rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                      {option.meta}
                    </span>
                  )}
                </button>
              );
            })}

            {showCustom && (
              <button
                type="button"
                role="option"
                aria-selected={false}
                data-row={autoRow + filtered.length}
                onMouseEnter={() => setActive(autoRow + filtered.length)}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => choose(customValue)}
                className={rowClass(false, active === autoRow + filtered.length)}
              >
                {check(false)}
                <span className="min-w-0 flex-1 truncate font-mono">{customValue}</span>
                {customLabel && <span className="shrink-0 text-xs text-muted-foreground">{customLabel}</span>}
              </button>
            )}
          </div>

          {options.length === 0 && customValue.length === 0 ? (
            <p className="px-3 pb-3 pt-1 text-xs text-muted-foreground">{emptyText}</p>
          ) : filtered.length === 0 && !showCustom ? (
            <p className="px-3 pb-3 pt-1 text-xs text-muted-foreground">{noMatchesText}</p>
          ) : null}
        </div>
      )}
    </div>
  );
}
