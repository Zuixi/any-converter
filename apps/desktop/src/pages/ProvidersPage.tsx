import { useMemo, useState } from "react";
import { useI18n } from "@any-converter/core";

import { Input } from "@any-converter/ui";

import { AddProviderSheet } from "../components/providers/AddProviderSheet";
import { DeleteProviderDialog } from "../components/providers/DeleteProviderDialog";
import { ProviderCard } from "../components/providers/ProviderCard";
import { ProviderDetailDialog } from "../components/providers/ProviderDetailDialog";
import { UnboundSection } from "../components/providers/UnboundSection";
import { useProvidersStrings } from "../components/providers/strings";
import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { useProvidersOverview } from "../hooks/useProvidersOverview";
import { agentUsesProvider } from "../lib/agents";
import { management, useServerRequest, type AgentSummary } from "../lib/management";

/**
 * The one providers page (design: docs/design/2026-11-12-providers-page-ia.md):
 * a card per provider (Zone 1), accounts not yet attached (Zone 2), and
 * everything onboarding in one sheet (Zone 3). Accounts are managed inside
 * the provider they belong to; routing lives on the Routes page.
 */
export function ProvidersPage() {
  const { t } = useI18n();
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const overview = useProvidersOverview(describeError);
  const [filter, setFilter] = useState("");
  const [openProvider, setOpenProvider] = useState<string>();
  const [actionError, setActionError] = useState<string>();
  const [pendingDelete, setPendingDelete] = useState<{
    provider: string;
    agents: AgentSummary[];
  }>();
  const [deleting, setDeleting] = useState(false);

  const cards = useMemo(() => {
    const words = filter.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (words.length === 0) {
      return overview.cards;
    }
    return overview.cards.filter((card) => {
      const text = [
        card.provider.name,
        card.provider.format,
        card.provider.base_url,
        ...card.provider.accounts,
        ...card.exposed,
      ]
        .join(" ")
        .toLowerCase();
      return words.every((word) => text.includes(word));
    });
  }, [overview.cards, filter]);

  const open = overview.cards.find((card) => card.provider.name === openProvider);

  const removeProvider = async (name: string) => {
    // Revision read at click time: a cached view may predate another
    // mutation, and an empty If-Match is rejected by the gateway.
    const current = await management.getConfig();
    const next = await management.patchConfig(current.revision, { remove_providers: [name] });
    await management.deleteProviderSecret(next.revision, name).catch(() => undefined);
    if (openProvider === name) {
      setOpenProvider(undefined);
    }
    await overview.refresh();
  };

  /**
   * Deleting a provider an agent is wired to would leave that agent pointing
   * at a provider that no longer exists. Find those agents first: with none,
   * a plain confirm is enough; with some, the dialog restores them before
   * the provider is removed.
   */
  const requestDelete = async (name: string) => {
    setActionError(undefined);
    try {
      const agents = await management.listAgents();
      const wired = agents.filter(
        (agent) =>
          (agent.status === "connected" || agent.status === "drifted") &&
          agentUsesProvider(agent, name),
      );
      if (wired.length > 0) {
        setPendingDelete({ provider: name, agents: wired });
        return;
      }
      if (!window.confirm(s.deleteConfirm)) {
        return;
      }
      await removeProvider(name);
    } catch (cause) {
      setActionError(describeError(cause));
    }
  };

  const confirmDelete = async () => {
    if (!pendingDelete) {
      return;
    }
    setActionError(undefined);
    setDeleting(true);
    try {
      // Restore the agents before the provider disappears. A failed
      // disconnect aborts the delete, so no agent is ever left wired to a
      // provider this action removed.
      for (const agent of pendingDelete.agents) {
        await management.agentAction(agent.id, "disconnect");
      }
      await removeProvider(pendingDelete.provider);
      setPendingDelete(undefined);
    } catch (cause) {
      setActionError(describeError(cause));
    } finally {
      setDeleting(false);
    }
  };

  const totalExposed = overview.cards.reduce((total, card) => total + card.exposed.length, 0);
  const totalRouted = overview.cards.reduce((total, card) => total + card.routed, 0);
  const totalUnrouted = overview.cards.reduce((total, card) => total + card.unrouted, 0);

  return (
    <section className="grid gap-6">
      <Header title={t("nav.providers")} subtitle={s.subtitle} />
      {(overview.error || actionError) && <ErrorBanner message={overview.error ?? actionError ?? ""} />}
      {overview.config?.restart_required && (
        <p className="text-sm text-muted-foreground">{s.restartRequired}</p>
      )}

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
        <span>
          {overview.cards.length} {s.summaryProviders} · {totalExposed} {s.summaryExposed} ·{" "}
          {totalRouted} {s.summaryRouted}
        </span>
        {totalUnrouted > 0 && (
          <span className="text-amber-600 dark:text-amber-400">
            {totalUnrouted} {s.unroutedWarning}
          </span>
        )}
        <span className="grow" />
        <Input
          className="w-64"
          value={filter}
          placeholder={s.filter}
          onChange={(event) => setFilter(event.target.value)}
          data-testid="provider-filter"
        />
      </div>

      {/* Zone 1 */}
      {overview.cards.length === 0 && overview.error === undefined ? (
        <p className="rounded-md border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">
          {s.empty}
        </p>
      ) : (
        <div className="grid gap-3" data-testid="provider-cards">
          {cards.map((card) => (
            <ProviderCard
              key={card.provider.name}
              card={card}
              quotaOf={(id) => overview.quotas[id]}
              onOpen={() => setOpenProvider(card.provider.name)}
              onDelete={() => void requestDelete(card.provider.name)}
            />
          ))}
        </div>
      )}

      {/* Zone 2 */}
      <UnboundSection unbound={overview.unbound} onChanged={overview.refresh} />

      {/* Zone 3 */}
      <AddProviderSheet accounts={overview.accounts} onChanged={overview.refresh} />

      {pendingDelete && (
        <DeleteProviderDialog
          provider={pendingDelete.provider}
          agents={pendingDelete.agents}
          busy={deleting}
          onCancel={() => {
            if (!deleting) {
              setPendingDelete(undefined);
            }
          }}
          onConfirm={() => void confirmDelete()}
        />
      )}

      {open && (
        <ProviderDetailDialog
          card={open}
          allAccounts={overview.accounts}
          quotaOf={(id) => overview.quotas[id]}
          plugins={overview.plugins}
          onClose={() => setOpenProvider(undefined)}
          onChanged={overview.refresh}
        />
      )}
    </section>
  );
}
