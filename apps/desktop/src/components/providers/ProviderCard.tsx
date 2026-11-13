import { Badge } from "@any-converter/ui";

import type { QuotaEntry } from "../../lib/management";
import type { ProviderCardView } from "../../hooks/useProvidersOverview";
import { useProvidersStrings } from "./strings";

function QuotaStrip({ quota }: { quota: QuotaEntry | undefined }) {
  const s = useProvidersStrings();
  const windows = quota?.windows ?? [];  if (windows.length === 0) {
    return <span className="text-xs text-muted-foreground">{s.quotaNone}</span>;
  }
  return (
    <span className="inline-flex max-w-56 flex-wrap items-center gap-1.5">
      {windows.slice(0, 3).map((window) => (
        <span
          key={window.name}
          className="inline-flex h-1.5 w-14 overflow-hidden rounded bg-muted"
          title={`${window.name}: ${Math.round(window.used)}%${
            window.resets ? ` · ${s.quotaResets} ${new Date(window.resets).toLocaleString()}` : ""
          }`}
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

/**
 * Zone 1: one card per provider — name and wire, the credential story
 * (key / accounts / plugin), each bound account's quota, and the
 * model/route summary. A summary, not an editor: the click opens the
 * detail dialog.
 */
export function ProviderCard({
  card,
  quotaOf,
  onOpen,
  onDelete,
}: {
  card: ProviderCardView;
  quotaOf: (accountId: string) => QuotaEntry | undefined;
  onOpen: () => void;
  onDelete: () => void;
}) {
  const s = useProvidersStrings();
  const { provider, accounts } = card;
  const isPlugin = provider.plugin;
  const usesAccounts = accounts.length > 0;

  return (
    <button
      type="button"
      className="grid w-full gap-2 rounded-lg border bg-card p-4 text-left shadow-sm transition-colors hover:border-primary/50"
      onClick={onOpen}
      data-provider-card={provider.name}
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-md bg-muted text-sm font-semibold uppercase">
          {provider.name.slice(0, 2)}
        </span>
        <span className="font-medium">{provider.name}</span>
        <Badge variant="secondary">{provider.format}</Badge>
        {isPlugin ? (
          <Badge variant="secondary">{s.pluginBadge}</Badge>
        ) : usesAccounts ? (
          <Badge variant="secondary">
            {s.oauthBadge} ×{accounts.length}
          </Badge>
        ) : provider.api_key_configured ? (
          <Badge variant="secondary">{s.keyConfigured}</Badge>
        ) : (
          <Badge variant="destructive">{s.keyMissing}</Badge>
        )}
        {card.catalog?.status === "ok" && (
          <span className="text-xs text-emerald-600 dark:text-emerald-400">
            {s.discoveryOk} · {card.catalog.discovered_count}
          </span>
        )}
        {card.catalog?.status === "error" && (
          <span className="text-xs text-destructive">{s.discoveryFailed}</span>
        )}
        <span className="grow" />
        <span
          role="button"
          tabIndex={0}
          className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-muted hover:text-destructive"
          onClick={(event) => {
            event.stopPropagation();
            if (window.confirm(s.deleteConfirm)) {
              onDelete();
            }
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") {
              event.stopPropagation();
              if (window.confirm(s.deleteConfirm)) {
                onDelete();
              }
            }
          }}
        >
          {s.delete}
        </span>
      </div>

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
        {usesAccounts ? (
          <span className="inline-flex flex-wrap items-center gap-2">
            {accounts.slice(0, 2).map((account) => (
              <span key={account.id} className="inline-flex items-center gap-1.5">
                <span className="max-w-40 truncate" title={account.id}>
                  {s.signedInAs} {account.account || account.id}
                </span>
                <QuotaStrip quota={quotaOf(account.id)} />
              </span>
            ))}
            {accounts.length > 2 && <span>+{accounts.length - 2}</span>}
          </span>
        ) : (
          <span className="truncate">{provider.base_url || "—"}</span>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span>
          {card.exposed.length} {s.modelsCount} · {card.routed} {s.routedCount}
        </span>
        {card.unrouted > 0 && (
          <span className="text-amber-600 dark:text-amber-400">
            {card.unrouted} {s.cardUnrouted} ⚠
          </span>
        )}
      </div>
    </button>
  );
}
