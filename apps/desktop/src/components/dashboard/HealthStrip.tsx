import { Link } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import type { DashboardStats } from "../../hooks/useDashboardOverview";

/**
 * The consolidated "something needs you" strip: drift, provider discovery
 * failures, unrouted models, a failed gateway. Hidden entirely when the
 * gateway is healthy — silence means everything is fine.
 */
export function HealthStrip({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();

  const items: Array<{ key: string; text: string; to?: string }> = [];

  if (stats.status?.state === "failed") {
    items.push({
      key: "failed",
      text: t("desktop.dashboard.health.failed"),
    });
  }
  if ((stats.driftedAgents ?? 0) > 0) {
    items.push({
      key: "drift",
      text: t("desktop.dashboard.health.agentDrift").replace(
        "{count}",
        String(stats.driftedAgents ?? 0),
      ),
      to: "/agents",
    });
  }
  if ((stats.providerErrors?.length ?? 0) > 0) {
    items.push({
      key: "providers",
      text: t("desktop.dashboard.health.providerErrors").replace(
        "{names}",
        (stats.providerErrors ?? []).join(", "),
      ),
      to: "/providers",
    });
  }
  if ((stats.unroutedModels ?? 0) > 0) {
    items.push({
      key: "unrouted",
      text: t("desktop.dashboard.health.unrouted").replace(
        "{count}",
        String(stats.unroutedModels ?? 0),
      ),
      to: "/routes",
    });
  }

  if (items.length === 0) {
    return null;
  }

  return (
    <section
      role="status"
      className="rounded-lg border border-amber-600/40 bg-amber-500/10 dark:border-amber-400/40 dark:bg-amber-400/10"
    >
      <ul className="grid gap-1 p-3">
        {items.map((item) => (
          <li key={item.key} className="flex items-center gap-2 text-sm">
            <span aria-hidden="true" className="text-amber-600 dark:text-amber-400">
              ⚠
            </span>
            {item.to ? (
              <Link to={item.to} className="underline-offset-2 hover:underline">
                {item.text}
              </Link>
            ) : (
              <span>{item.text}</span>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
