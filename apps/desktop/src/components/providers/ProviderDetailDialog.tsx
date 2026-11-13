import { useMemo, useState } from "react";
import { Link } from "react-router-dom";

import { Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@any-converter/ui";

import { AUTH_SCHEME_OPTIONS, SUBSCRIPTION_PRESETS } from "../../lib/constants";
import {
  management,
  useServerRequest,
  type AccountEntry,
  type DiscoveryReport,
  type PluginEntry,
  type QuotaEntry,
  type SignInTask,
} from "../../lib/management";
import type { ProviderCardView } from "../../hooks/useProvidersOverview";
import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { ModelPicker, type PickerModel } from "./ModelPicker";
import { Modal } from "./Modal";
import { SignInFlow } from "./SignInFlow";
import { accountForTask, parseManualModels, useProvidersStrings } from "./strings";

/** Sections of the detail dialog; accounts and models often cross-reference,
 * so this is one scroll, not tabs. */
export function ProviderDetailDialog({
  card,
  allAccounts,
  quotaOf,
  plugins,
  onClose,
  onChanged,
}: {
  card: ProviderCardView;
  allAccounts: AccountEntry[];
  quotaOf: (accountId: string) => QuotaEntry | undefined;
  plugins: PluginEntry[];
  onClose: () => void;
  onChanged: () => Promise<void>;
}) {
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  // — models editing (from the old providers page, kept whole) —
  const [models, setModels] = useState<string[]>(card.exposed);
  const [report, setReport] = useState<DiscoveryReport>();
  const [loading, setLoading] = useState(false);
  const [manual, setManual] = useState("");
  const [scheme, setScheme] = useState(card.provider.auth_scheme);

  const [addingAccount, setAddingAccount] = useState<string | undefined>(undefined);

  const loadModels = async (refresh: boolean) => {
    setLoading(true);
    try {
      const next = await management.getProviderModels(card.provider.name, refresh);
      setReport(next);
      if (next.status === "ok") {
        setModels((current) => (current.length > 0 ? current : next.models.map((model) => model.id)));
      }
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setLoading(false);
    }
  };

  const pickerModels = useMemo<PickerModel[]>(() => {
    const seen = new Map<string, PickerModel>();
    for (const model of report?.models ?? []) {
      seen.set(model.id, model);
    }
    for (const model of models) {
      if (!seen.has(model)) {
        seen.set(model, { id: model });
      }
    }
    return [...seen.values()];
  }, [report, models]);

  const save = async () => {
    setError(undefined);
    try {
      const current = await management.getConfig();
      const fresh = current.providers.find((entry) => entry.name === card.provider.name);
      if (!fresh) {
        throw new Error(`provider ${card.provider.name} no longer exists`);
      }
      // Preserve upstream rewrites and wildcard entries; only the exposed
      // set changes (the old page's rule, kept).
      const preserved = Object.entries(fresh.model_map).filter(
        ([model]) => model.includes("*") || model.includes("?"),
      );
      const patch: {
        name: string;
        model_map?: Record<string, string>;
        auth_scheme?: typeof card.provider.auth_scheme;
        accounts?: string[];
      } = { name: card.provider.name };
      if (models.length > 0) {
        patch.model_map = Object.fromEntries([
          ...preserved,
          ...models.map((id) => [id, fresh.model_map[id] ?? id] as const),
        ]);
      }
      if (scheme !== fresh.auth_scheme) {
        patch.auth_scheme = scheme;
      }
      await management.patchConfig(current.revision, { upsert_providers: [patch] });
      setNotice(s.savedNotice);
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    }
  };

  const { provider } = card;
  const isPlugin = provider.plugin;

  return (
    <Modal
      wide
      escape={addingAccount === undefined}
      title={
        <span className="flex flex-wrap items-center gap-2">
          <span className="grid h-7 w-7 place-items-center rounded bg-muted text-xs font-semibold uppercase">
            {provider.name.slice(0, 2)}
          </span>
          {provider.name}
          <span className="text-sm font-normal text-muted-foreground">{provider.format}</span>
        </span>
      }
      onClose={onClose}
    >
      {/* ① access */}
      <section className="grid gap-3">
        <h3 className="text-sm font-semibold">{s.sectionAccess}</h3>
        {card.accounts.length > 0 && (
          <ul className="grid gap-2">
            {card.accounts.map((account, index) => (
              <li
                key={account.id}
                className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm"
                data-account-row={account.id}
              >
                <span className="min-w-0 flex-1 truncate" title={account.id}>
                  {account.account || account.id}
                </span>
                <QuotaInline quota={quotaOf(account.id)} />
                {index > 0 && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void moveUp(card, account.id, card.accounts)}
                  >
                    ↑ {s.moveUp}
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="destructive"
                  onClick={() => void removeAccount(account.id)}
                >
                  {s.removeAccount}
                </Button>
              </li>
            ))}
          </ul>
        )}
        <p className="text-xs text-muted-foreground">{s.orderHint}</p>
        {isPlugin ? (
          <p className="text-sm text-muted-foreground">
            {s.pluginManaged} ·{" "}
            {plugins.find((entry) => entry.providers.some((served) => served.id === accountPrefix(card)))
              ?.spec ?? ""}
          </p>
        ) : !addingAccount ? (
          card.accounts.length > 0 ? (
            <Button
              size="sm"
              variant="outline"
              className="w-fit"
              onClick={() => setAddingAccount(card.accounts[0]?.id.split(":")[0] ?? "gemini")}
            >
              {s.addAccount}
            </Button>
          ) : (
            // Key-based provider: pick which subscription to sign in, then
            // the finished account is bound (the key stops being used).
            <div className="flex flex-wrap items-end gap-2">
              <div className="w-64">
                <Field label={s.addAccount} help={s.accessSwitchNote}>
                  <Select value="" onValueChange={(id) => setAddingAccount(id)}>
                    <SelectTrigger>
                      <SelectValue placeholder={s.sectionSubs} />
                    </SelectTrigger>
                    <SelectContent>
                      {SUBSCRIPTION_PRESETS.map((preset) => (
                        <SelectItem key={preset.id} value={preset.id}>
                          {preset.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
              </div>
            </div>
          )
        ) : null}
        {addingAccount && (
          <SignInFlow
            provider={addingAccount}
            accounts={allAccounts}
            onDone={(task) => void attachSignedIn(task)}
            onClose={() => setAddingAccount(undefined)}
          />
        )}
      </section>

      {/* ② basics + probe */}
      <section className="grid gap-3 border-t pt-4">
        <h3 className="text-sm font-semibold">{s.sectionBasic}</h3>
        <div className="grid gap-3 md:grid-cols-2">
          <Field label={s.baseUrl} help="">
            <Input value={provider.base_url} readOnly />
          </Field>
          <Field label={s.authScheme} help="">
            <Select value={scheme} onValueChange={(next) => setScheme(next as typeof scheme)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AUTH_SCHEME_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.labelKey.slice(option.labelKey.lastIndexOf(".") + 1)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <Button
            size="sm"
            variant="outline"
            disabled={loading}
            onClick={() => void loadModels(true)}
          >
            {loading ? s.probing : s.probeAgain}
          </Button>
          {report?.status === "ok" && (
            <span className="text-sm text-emerald-600 dark:text-emerald-400">
              {s.discoveryOk} · {report.count} ({report.latency_ms} ms)
            </span>
          )}
          {report && report.status !== "ok" && report.error && (
            <span className="text-sm text-destructive">
              {s.discoveryFailed} — {report.error.message}
              {report.error.hint ? ` (${report.error.hint})` : ""}
            </span>
          )}
        </div>
      </section>

      {/* ③ models */}
      <section className="grid gap-3 border-t pt-4">
        <h3 className="text-sm font-semibold">{s.sectionModels}</h3>
        <ModelPicker
          models={pickerModels}
          selected={models}
          onChange={setModels}
          labels={{
            search: s.searchModels,
            selectAll: s.selectAll,
            clear: s.clearSelection,
            empty: s.noModelsFound,
            noMatches: s.noMatches,
            selectedCount: s.selectedCount,
          }}
        />
        <Field label={s.manualModels} help={s.manualModelsHelp}>
          <Input
            value={manual}
            onChange={(event) => {
              setManual(event.target.value);
              const ids = parseManualModels(event.target.value);
              setModels((current) => [...new Set([...current, ...ids])]);
            }}
          />
        </Field>
      </section>

      {/* ④ routes (read-only; editing lives on the Routes page) */}
      <section className="grid gap-2 border-t pt-4">
        <h3 className="text-sm font-semibold">{s.sectionRoutes}</h3>
        {card.routePatterns.length === 0 ? (
          <p className="text-sm text-muted-foreground">{s.noRoutes}</p>
        ) : (
          <div className="flex flex-wrap gap-1.5">
            {card.routePatterns.map((pattern) => (
              <code key={pattern} className="rounded bg-muted px-2 py-0.5 text-xs">
                {pattern}
              </code>
            ))}
            <span className="text-xs text-muted-foreground">
              · {card.routePatterns.length} {s.routePatterns}
            </span>
          </div>
        )}
        <Link className="w-fit text-sm underline" to="/routes" onClick={onClose}>
          {s.gotoRoutes} →
        </Link>
      </section>

      {error && <ErrorBanner message={error} />}
      {notice && (
        <p className="text-sm text-emerald-600 dark:text-emerald-400">{notice}</p>
      )}
      <div className="flex justify-end gap-2 border-t pt-4">
        <Button variant="ghost" onClick={onClose}>
          {s.close}
        </Button>
        <Button onClick={() => void save()}>{s.saveModels}</Button>
      </div>
    </Modal>
  );

  async function moveUp(target: ProviderCardView, accountId: string, siblings: AccountEntry[]) {
    const ids = siblings.map((entry) => entry.id);
    const at = ids.indexOf(accountId);
    if (at <= 0) return;
    const above = ids[at - 1];
    const here = ids[at];
    if (above === undefined || here === undefined) return;
    ids[at - 1] = here;
    ids[at] = above;
    setError(undefined);
    try {
      // The order file keys by the account's provider prefix (gemini, …),
      // not the config provider's name.
      await management.saveAccountOrder(accountId.split(":")[0] ?? target.provider.name, ids);
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    }
  }

  async function removeAccount(accountId: string) {
    setError(undefined);
    try {
      await management.removeAccount(accountId);
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    }
  }

  /// Bind the account a finished sign-in stored to this provider (setting
  /// accounts makes the api key unused — the config's own semantics).
  async function attachSignedIn(task: SignInTask) {
    if (task.state !== "done") {
      await onChanged();
      return;
    }
    setError(undefined);
    try {
      const current = await management.getConfig();
      const freshAccounts = await management.accountsOverview();
      const account = accountForTask(task, freshAccounts.accounts);
      if (!account) {
        await onChanged();
        return;
      }
      const accounts = [...new Set([...provider.accounts, account.id])];
      await management.patchConfig(current.revision, {
        upsert_providers: [{ name: card.provider.name, accounts }],
      });
      setAddingAccount(undefined);
      setNotice(s.savedNotice);
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    }
  }
}

/** The sign-in provider prefix of a provider's first account. */
function accountPrefix(card: ProviderCardView): string {
  return card.provider.accounts[0]?.split(":")[0] ?? "";
}

function QuotaInline({ quota }: { quota: QuotaEntry | undefined }) {
  const s = useProvidersStrings();
  const windows = quota?.windows ?? [];
  if (windows.length === 0) {
    return <span className="text-xs text-muted-foreground">{s.quotaNone}</span>;
  }
  return (
    <span className="inline-flex items-center gap-1.5">
      {windows.slice(0, 2).map((window) => (
        <span
          key={window.name}
          className="inline-flex h-1.5 w-12 overflow-hidden rounded bg-muted"
          title={`${window.name}: ${Math.round(window.used)}%`}
        >
          <span
            className={
              window.used >= 95
                ? "block h-full bg-destructive"
                : window.used >= 70
                  ? "block h-full bg-amber-500"
                  : "block h-full bg-emerald-500"
            }
            style={{ width: `${Math.min(100, Math.max(0, window.used))}%` }}
          />
        </span>
      ))}
    </span>
  );
}
