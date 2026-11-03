"use client";

import { useEffect, useMemo, useState } from "react";

import type { LedgerFilter, LedgerRow } from "@any-converter/shared";
import { formatDuration, formatTimestamp } from "@any-converter/shared";
import {
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@any-converter/ui";
import { useApiClient, useI18n, useLedger } from "@any-converter/core";

type Preset = "today" | "3d" | "7d" | "30d" | "all";

/** `3d` has no translation key yet, so it keeps its period token as its label. */
const PRESETS = [
  { value: "today", key: "usage.period.today" },
  { value: "3d" },
  { value: "7d", key: "usage.period.7d" },
  { value: "30d", key: "usage.period.30d" },
  { value: "all", key: "usage.period.all" },
] as const;

const DAY_MS = 24 * 60 * 60 * 1000;

/** Radix select items cannot carry an empty value, so "any" stands in for "no filter". */
const ANY = "any";

/** UTC query bounds for a period preset, truncated to whole seconds. */
function presetRange(preset: Preset): { from: string; to: string } {
  const now = new Date();
  const start = new Date(now.getTime());
  if (preset === "today") {
    start.setUTCHours(0, 0, 0, 0);
  } else if (preset === "3d") {
    start.setTime(now.getTime() - 3 * DAY_MS);
  } else if (preset === "7d") {
    start.setTime(now.getTime() - 7 * DAY_MS);
  } else if (preset === "30d") {
    start.setTime(now.getTime() - 30 * DAY_MS);
  } else {
    start.setTime(0);
  }
  return { from: toIsoSeconds(start), to: toIsoSeconds(now) };
}

function toIsoSeconds(date: Date): string {
  return date.toISOString().slice(0, 19) + "Z";
}

/** Distinct sorted values for a select, keeping the active choice selectable. */
function optionsFor(values: (string | undefined)[], selected: string): string[] {
  const seen = new Set<string>();
  for (const value of values) {
    if (value !== undefined && value !== "") {
      seen.add(value);
    }
  }
  const options = [...seen].sort((a, b) => a.localeCompare(b));
  if (selected === ANY || options.includes(selected)) {
    return options;
  }
  return [selected, ...options];
}

export function LedgerView() {
  const { t } = useI18n();
  const api = useApiClient();

  const [preset, setPreset] = useState<Preset>("3d");
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [provider, setProvider] = useState(ANY);
  const [agent, setAgent] = useState(ANY);
  const [model, setModel] = useState(ANY);
  const [account, setAccount] = useState(ANY);
  const [failed, setFailed] = useState(false);
  const [retried, setRetried] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState("");

  // Typing should not fire one request per keystroke: commit the text after a pause.
  useEffect(() => {
    const timer = setTimeout(() => setQuery(search.trim()), 300);
    return () => clearTimeout(timer);
  }, [search]);

  // Anchor the range per period change: recomputing "now" on every render would
  // hand useLedger a new filter key and refetch in a loop.
  const range = useMemo(() => presetRange(preset), [preset]);
  const filter = useMemo<LedgerFilter>(
    () => ({
      ...range,
      provider: provider === ANY ? undefined : provider,
      agent: agent === ANY ? undefined : agent,
      model: model === ANY ? undefined : model,
      account: account === ANY ? undefined : account,
      failed: failed ? true : undefined,
      retried: retried ? true : undefined,
      query: query === "" ? undefined : query,
    }),
    [range, provider, agent, model, account, failed, retried, query],
  );

  const { page, loading, error, loadMore } = useLedger(filter);
  const rows = useMemo(() => page?.rows ?? [], [page]);

  const providerOptions = useMemo(() => optionsFor(rows.map((row) => row.provider), provider), [rows, provider]);
  const agentOptions = useMemo(() => optionsFor(rows.map((row) => row.agent), agent), [rows, agent]);
  const modelOptions = useMemo(() => optionsFor(rows.map((row) => row.client_model), model), [rows, model]);
  const accountOptions = useMemo(() => optionsFor(rows.map((row) => row.account), account), [rows, account]);

  const downloadCsv = async () => {
    setExporting(true);
    setExportError("");
    try {
      const csv = await api.getLedgerCsv(filter);
      const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `ledger-${new Date().toISOString().slice(0, 10)}.csv`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      // Revoke on the next task so the click has already started the download.
      setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (err: unknown) {
      setExportError(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className="grid min-w-0 gap-6">
      <div className="flex flex-wrap items-end gap-4">
        <div className="grid gap-2">
          <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{t("usage.period")}</span>
          <div className="flex flex-wrap gap-2">
            {PRESETS.map((item) => (
              <Button
                key={item.value}
                type="button"
                size="sm"
                variant={preset === item.value ? "default" : "outline"}
                aria-pressed={preset === item.value}
                onClick={() => setPreset(item.value)}
              >
                {"key" in item ? t(item.key) : item.value}
              </Button>
            ))}
          </div>
        </div>

        <div className="grid gap-1">
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => void downloadCsv()}
            disabled={exporting}
          >
            {t("usage.downloadCsv")}
          </Button>
          {exportError !== "" && <span className="text-xs text-destructive">{exportError}</span>}
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>{t("usage.filters")}</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <div className="grid gap-2">
              <Label htmlFor="ledger-search">{t("usage.search")}</Label>
              <Input
                id="ledger-search"
                placeholder={t("usage.search")}
                value={search}
                onChange={(event) => setSearch(event.target.value)}
              />
            </div>
            <FilterSelect
              id="ledger-provider"
              label={t("usage.provider")}
              anyLabel={t("usage.any")}
              value={provider}
              options={providerOptions}
              onChange={setProvider}
            />
            <FilterSelect
              id="ledger-agent"
              label={t("usage.agent")}
              anyLabel={t("usage.any")}
              value={agent}
              options={agentOptions}
              onChange={setAgent}
            />
            <FilterSelect
              id="ledger-model"
              label={t("usage.model")}
              anyLabel={t("usage.any")}
              value={model}
              options={modelOptions}
              onChange={setModel}
            />
            <FilterSelect
              id="ledger-account"
              label={t("usage.account")}
              anyLabel={t("usage.any")}
              value={account}
              options={accountOptions}
              onChange={setAccount}
            />
            <label className="flex items-end gap-2 pb-2 text-sm">
              <input
                type="checkbox"
                className="h-4 w-4 accent-primary"
                checked={failed}
                onChange={(event) => setFailed(event.target.checked)}
              />
              {t("usage.failedOnly")}
            </label>
            <label className="flex items-end gap-2 pb-2 text-sm">
              <input
                type="checkbox"
                className="h-4 w-4 accent-primary"
                checked={retried}
                onChange={(event) => setRetried(event.target.checked)}
              />
              {t("usage.retriedOnly")}
            </label>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardContent className="pt-6">
          {loading && rows.length === 0 && <p className="text-muted-foreground">{t("usage.loading")}</p>}
          {error !== "" && <p className="text-destructive">{error}</p>}
          {!loading && error === "" && rows.length === 0 && (
            <p className="text-muted-foreground">{t("usage.empty")}</p>
          )}

          {rows.length > 0 && (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b text-left text-xs uppercase tracking-wide text-muted-foreground">
                    <th className="py-2 pr-4 font-medium">{t("usage.time")}</th>
                    <th className="py-2 pr-4 font-medium">{t("usage.agent")}</th>
                    <th className="py-2 pr-4 font-medium">{t("usage.provider")}</th>
                    <th className="py-2 pr-4 font-medium">{t("usage.model")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.inputTokens")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.outputTokens")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.cacheRead")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.cacheWrite")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.cost")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.status")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.attempt")}</th>
                    <th className="py-2 pr-4 text-right font-medium">{t("usage.latency")}</th>
                    <th className="py-2 text-right font-medium">{t("usage.ttft")}</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row) => (
                    <LedgerTableRow key={row.request_id} row={row} />
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {page !== null && (
            <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t pt-3">
              <span className="text-sm text-muted-foreground">
                {t("usage.total")}: <span className="tabular-nums">{formatNumber(page.total)}</span>
              </span>
              {page.next_cursor !== undefined && (
                <Button type="button" variant="outline" size="sm" onClick={loadMore} disabled={loading}>
                  {t("usage.loadMore")}
                </Button>
              )}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function FilterSelect({
  id,
  label,
  anyLabel,
  value,
  options,
  onChange,
}: {
  id: string;
  label: string;
  anyLabel: string;
  value: string;
  options: string[];
  onChange: (value: string) => void;
}) {
  return (
    <div className="grid gap-2">
      <Label htmlFor={id}>{label}</Label>
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger id={id}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ANY}>{anyLabel}</SelectItem>
          {options.map((option) => (
            <SelectItem key={option} value={option}>
              {option}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

function LedgerTableRow({ row }: { row: LedgerRow }) {
  const { t } = useI18n();
  const servedModel =
    row.served_model !== undefined && row.served_model !== "" && row.served_model !== row.client_model
      ? row.served_model
      : null;

  return (
    <tr className="border-b last:border-b-0">
      <td className="whitespace-nowrap py-2 pr-4 tabular-nums">{formatTimestamp(row.timestamp)}</td>
      <td className="py-2 pr-4">{row.agent ?? t("usage.notAvailable")}</td>
      <td className="py-2 pr-4">{row.provider}</td>
      <td className="py-2 pr-4">
        <div className="font-medium">{row.client_model}</div>
        {servedModel !== null && <div className="text-xs text-muted-foreground">{servedModel}</div>}
      </td>
      <td className="py-2 pr-4 text-right tabular-nums">{formatNumber(row.input_tokens)}</td>
      <td className="py-2 pr-4 text-right tabular-nums">{formatNumber(row.output_tokens)}</td>
      <td className="py-2 pr-4 text-right tabular-nums">{formatNumber(row.cache_read_tokens)}</td>
      <td className="py-2 pr-4 text-right tabular-nums">{formatNumber(row.cache_write_tokens)}</td>
      <td className="py-2 pr-4 text-right tabular-nums">
        {row.cost_usd === undefined ? t("usage.notAvailable") : row.cost_usd.toFixed(2)}
      </td>
      <td className={`py-2 pr-4 text-right tabular-nums${row.status >= 400 ? " text-destructive" : ""}`}>
        {row.status}
      </td>
      <td className="py-2 pr-4 text-right tabular-nums">{row.attempt + 1}</td>
      <td className="py-2 pr-4 text-right tabular-nums">{formatDuration(row.latency_ms)}</td>
      <td className="py-2 text-right tabular-nums">{formatDuration(row.ttft_ms)}</td>
    </tr>
  );
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(Math.round(value));
}
