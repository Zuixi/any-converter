import { useMemo, useState } from "react";

import { Button, Card, CardContent, Input } from "@any-converter/ui";

import {
  PLUGIN_CATALOG,
  PRESET_ALIASES,
  PROVIDER_PRESETS,
  SUBSCRIPTION_PRESETS,
  type PresetKind,
} from "../../lib/constants";
import {
  management,
  useServerRequest,
  type AccountEntry,
  type InstalledPlugin,
  type SignInTask,
} from "../../lib/management";
import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { SignInFlow } from "./SignInFlow";
import { VendorProviderModal } from "./VendorProviderModal";
import { accountForTask, useProvidersStrings } from "./strings";

/**
 * Zone 3: everything onboarding, in place — sign in to a subscription,
 * install-and-sign-in to a plugin, or fill a vendor API key. Choosing a
 * tile never leaves the page: its form or flow opens under the sheet.
 */
export function AddProviderSheet({
  accounts,
  onChanged,
}: {
  accounts: AccountEntry[];
  onChanged: () => Promise<void>;
}) {
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const [query, setQuery] = useState("");

  // the one flow/form the sheet has open, if any
  const [subscription, setSubscription] = useState<string>();
  const [pluginEntry, setPluginEntry] = useState<InstalledPlugin | "installing" | undefined>();
  const [pluginSpec, setPluginSpec] = useState("");
  const [presetId, setPresetId] = useState<string>();
  const [error, setError] = useState<string>();

  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const matches = (parts: (string | undefined)[]) =>
    words.length === 0 || words.every((word) => parts.some((part) => part?.toLowerCase().includes(word)));

  const vendorPresets = useMemo(
    () =>
      PROVIDER_PRESETS.filter(
        (preset) =>
          preset.id !== "custom" &&
          matches([
            preset.label,
            preset.name,
            preset.base_url,
            ...(PRESET_ALIASES[preset.id] ?? []),
          ]),
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [query],
  );

  const close = () => {
    setSubscription(undefined);
    setPluginEntry(undefined);
    setPresetId(undefined);
    setError(undefined);
  };

  const installPlugin = async (spec: string) => {
    setPluginEntry("installing");
    setError(undefined);
    try {
      const installed = await management.installPlugin(spec);
      setPluginEntry(installed);
      if (installed.error) {
        setError(installed.error);
      }
      await onChanged();
    } catch (cause) {
      setPluginEntry(undefined);
      setError(`${s.installFailed}: ${describeError(cause)}`);
    }
  };

  return (
    <Card>
      <CardContent className="grid gap-5 pt-6" data-testid="add-provider-sheet">
        <div className="flex flex-wrap items-center gap-3">
          <p className="text-sm font-medium">{s.addTitle}</p>
          <span className="grow" />
          <Input
            className="w-72"
            value={query}
            placeholder={s.addSearch}
            onChange={(event) => setQuery(event.target.value)}
            data-testid="add-provider-search"
          />
        </div>

        {/* subscriptions: sign in, no key */}
        <SheetSection title={s.sectionSubs} hint={s.sectionSubsHint}>
          {SUBSCRIPTION_PRESETS.filter((preset) => matches([preset.label, preset.id])).map((preset) => (
            <button
              key={preset.id}
              type="button"
              className="flex flex-col items-start gap-0.5 rounded-md border bg-background px-3 py-2 text-left text-sm shadow-sm transition-colors hover:border-primary/50"
              data-testid={`sub-${preset.id}`}
              onClick={() => {
                setPluginEntry(undefined);
                setPresetId(undefined);
                setSubscription((current) => (current === preset.id ? undefined : preset.id));
              }}
            >
              <span className="font-medium">{preset.label}</span>
              <span className="text-xs text-muted-foreground">{preset.id}</span>
            </button>
          ))}
        </SheetSection>

        {/* plugins: installed for you when picked */}
        <SheetSection title={s.sectionPlugins} hint={s.sectionPluginsHint}>
          {PLUGIN_CATALOG.filter((entry) => matches([entry.label, entry.spec])).map((entry) => (
            <button
              key={entry.spec}
              type="button"
              className="flex flex-col items-start gap-0.5 rounded-md border bg-background px-3 py-2 text-left text-sm shadow-sm transition-colors hover:border-primary/50"
              data-testid={`plugin-${entry.label}`}
              title={entry.note}
              onClick={() => {
                setSubscription(undefined);
                setPresetId(undefined);
                void installPlugin(entry.spec);
              }}
            >
              <span className="font-medium">{entry.label}</span>
              <span className="text-xs text-muted-foreground">plugin</span>
            </button>
          ))}
          <div className="flex items-end gap-2">
            <div className="w-80">
              <Field label={s.sectionPlugins} help="">
                <Input
                  value={pluginSpec}
                  placeholder={s.pluginSpecPlaceholder}
                  onChange={(event) => setPluginSpec(event.target.value)}
                />
              </Field>
            </div>
            <Button
              size="sm"
              variant="outline"
              disabled={!pluginSpec.trim()}
              onClick={() => void installPlugin(pluginSpec.trim())}
            >
              {s.installPlugin}
            </Button>
          </div>
        </SheetSection>

        {/* vendor keys */}
        {(["vendor", "relay"] as PresetKind[]).map((kind) => {
          const presets = vendorPresets.filter((preset) => preset.kind === kind);
          if (presets.length === 0) return null;
          return (
            <SheetSection
              key={kind}
              title={kind === "vendor" ? s.sectionVendors : s.sectionRelays}
              hint={kind === "vendor" ? s.sectionVendorsHint : s.sectionRelaysHint}
            >
              {presets.map((preset) => (
                <button
                  key={preset.id}
                  type="button"
                  className="flex flex-col items-start gap-0.5 rounded-md border bg-background px-3 py-2 text-left text-sm shadow-sm transition-colors hover:border-primary/50"
                  data-testid={`preset-${preset.id}`}
                  onClick={() => {
                    setSubscription(undefined);
                    setPluginEntry(undefined);
                    setPresetId((current) => (current === preset.id ? undefined : preset.id));
                  }}
                >
                  <span className="font-medium">{preset.label}</span>
                  <span className="text-xs text-muted-foreground">{preset.kind}</span>
                </button>
              ))}
              <button
                type="button"
                className="flex flex-col items-start gap-0.5 rounded-md border bg-background px-3 py-2 text-left text-sm shadow-sm transition-colors hover:border-primary/50"
                data-testid="preset-custom"
                onClick={() => {
                  setSubscription(undefined);
                  setPluginEntry(undefined);
                  setPresetId((current) => (current === "custom" ? undefined : "custom"));
                }}
              >
                <span className="font-medium">{s.addCustom}</span>
                <span className="text-xs text-muted-foreground">{s.addCustomHint}</span>
              </button>
            </SheetSection>
          );
        })}

        {error && <ErrorBanner message={error} />}

        {/* — the open flow, under the sheet — */}

        {subscription && (
          <SignInFlow
            key={subscription}
            provider={subscription}
            autoStart
            accounts={accounts}
            onDone={(task) => void finishSubscription(task)}
            onClose={() => setSubscription(undefined)}
          />
        )}

        {pluginEntry !== undefined && (
          <div className="grid gap-3 border-t pt-4" data-testid="plugin-flow">
            {pluginEntry === "installing" ? (
              <p className="text-sm text-muted-foreground">{s.installing}</p>
            ) : pluginEntry.error ? (
              <ErrorBanner message={`${s.installFailed}: ${pluginEntry.error}`} />
            ) : (
              <>
                <p className="text-sm text-muted-foreground">
                  {pluginEntry.spec} · {pluginEntry.providers.map((served) => served.id).join(", ")}
                </p>
                {pluginEntry.providers.map((served) => (
                  <SignInFlow
                    key={served.id}
                    provider={`plugin:${served.id}`}
                    spec={pluginEntry.spec}
                    autoStart
                    accounts={accounts}
                    onDone={(task) => void finishSubscription(task)}
                    onClose={() => setPluginEntry(undefined)}
                  />
                ))}
              </>
            )}
          </div>
        )}

        {presetId && (
          <VendorProviderModal
            key={presetId}
            presetId={presetId}
            onDone={async () => {
              close();
              await onChanged();
            }}
            onClose={close}
          />
        )}
      </CardContent>
    </Card>
  );

  /// The shared finish for sign-ins: bind the account to a provider named
  /// after it (creating the provider when absent) and offer default routes.
  async function finishSubscription(task: SignInTask) {
    if (task.state !== "done") {
      await onChanged();
      return;
    }
    setError(undefined);
    try {
      const [current, overview] = await Promise.all([
        management.getConfig(),
        management.accountsOverview(),
      ]);
      const account = accountForTask(task, overview.accounts);
      if (!account) {
        await onChanged();
        return;
      }
      const existing = current.providers.find(
        (provider) => provider.accounts.includes(account.id),
      );
      if (existing) {
        await onChanged();
        return;
      }
      // A provider named after the subscription, bound to the account.
      const name = task.provider.startsWith("plugin:")
        ? task.provider.slice("plugin:".length)
        : task.provider;
      const routes =
        current.model_routes.length === 0
          ? [{ pattern: "*", providers: [name], upstream_model: null, strategy: "priority" as const }]
          : [];
      await management.patchConfig(current.revision, {
        upsert_providers: [{ name, accounts: [account.id] }],
        upsert_model_routes: routes,
      });
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    }
  }
}

/** One titled group of tiles. */
function SheetSection({
  title,
  hint,
  children,
}: {
  title: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <section className="grid gap-2">
      <div className="flex items-baseline gap-2">
        <h3 className="text-sm font-semibold">{title}</h3>
        <span className="text-xs text-muted-foreground">{hint}</span>
      </div>
      <div className="flex flex-wrap gap-2">{children}</div>
    </section>
  );
}

