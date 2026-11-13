import { useCallback, useEffect, useState } from "react";

import {
  management,
  type AccountEntry,
  type AdminConfigView,
  type CatalogView,
  type PluginEntry,
  type QuotaEntry,
} from "../lib/management";

export type ProviderView = AdminConfigView["providers"][number];
export type CatalogProvider = CatalogView["providers"][number];

/** One Zone-1 card: a config provider joined with its catalog and accounts. */
export interface ProviderCardView {
  provider: ProviderView;
  catalog: CatalogProvider | undefined;
  /** Bound accounts (those the provider's `accounts` name), user order. */
  accounts: AccountEntry[];
  exposed: string[];
  routed: number;
  unrouted: number;
  /** Route patterns naming this provider (read-only display). */
  routePatterns: string[];
}

export interface ProvidersOverview {
  cards: ProviderCardView[];
  /** Every stored account (bound and not), for sign-in result naming. */
  accounts: AccountEntry[];
  unbound: AccountEntry[];
  quotas: Record<string, QuotaEntry>;
  plugins: PluginEntry[];
  config: AdminConfigView | undefined;
  catalog: CatalogView | undefined;
  error: string | undefined;
  /** Bump to refetch everything. */
  refresh: () => Promise<void>;
}

const wildcard = (model: string) => model.includes("*") || model.includes("?");

/**
 * Everything the merged providers page shows, in one place: config
 * providers joined with catalog status, bound accounts with their quota
 * windows, plugins, and the accounts no provider claims (Zone 2).
 */
export function useProvidersOverview(describeError: (cause: unknown) => string): ProvidersOverview {
  const [config, setConfig] = useState<AdminConfigView>();
  const [catalog, setCatalog] = useState<CatalogView>();
  const [accounts, setAccounts] = useState<AccountEntry[]>([]);
  const [quotas, setQuotas] = useState<Record<string, QuotaEntry>>({});
  const [plugins, setPlugins] = useState<PluginEntry[]>([]);
  const [error, setError] = useState<string>();

  const refresh = useCallback(async () => {
    try {
      const overview = await management.accountsOverview();
      const [nextConfig, nextCatalog] = await Promise.all([
        management.getConfig(),
        management.getCatalog().catch(() => undefined),
      ]);
      setAccounts(overview.accounts);
      setPlugins(overview.plugins);
      const byAccount: Record<string, QuotaEntry> = {};
      for (const entry of overview.quotas) {
        byAccount[entry.account] = entry;
      }
      setQuotas(byAccount);
      setConfig(nextConfig);
      setCatalog(nextCatalog);
      setError(undefined);
    } catch (cause) {
      setError(describeError(cause));
    }
  }, [describeError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const providers = config?.providers ?? [];
  const catalogByName = new Map((catalog?.providers ?? []).map((entry) => [entry.name, entry]));
  const bound = new Set(providers.flatMap((provider) => provider.accounts));

  const cards: ProviderCardView[] = providers.map((provider) => {
    const exposed = Object.keys(provider.model_map).filter((model) => !wildcard(model));
    const entry = catalogByName.get(provider.name);
    const routedModels = new Set(
      (catalog?.models ?? [])
        .filter((model) => model.provider === provider.name && model.exposed && model.routed)
        .map((model) => model.id),
    );
    const routed = exposed.filter((model) => routedModels.has(model)).length;
    return {
      provider,
      catalog: entry,
      accounts: provider.accounts
        .map((id) => accounts.find((account) => account.id === id))
        .filter((account): account is AccountEntry => Boolean(account)),
      exposed,
      routed,
      unrouted: Math.max(0, exposed.length - routed),
      routePatterns: (config?.model_routes ?? [])
        .filter((route) => route.providers.includes(provider.name))
        .map((route) => route.pattern),
    };
  });

  return {
    cards,
    accounts,
    unbound: accounts.filter(
      (account) => account.source !== "plugin" && account.provider !== "" && !bound.has(account.id),
    ),
    quotas,
    plugins,
    config,
    catalog,
    error,
    refresh,
  };
}
