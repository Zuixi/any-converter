import { Link } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import type { DashboardStats } from "../../hooks/useDashboardOverview";

/**
 * The consolidated "something needs you" row: drift, provider discovery
 * failures, unrouted models, a failed gateway — one soft chip per issue,
 * linking to the page that fixes it. Hidden entirely when the gateway is
 * healthy: silence means everything is fine.
 */
export function HealthStrip({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();

  const items: Array<{ key: string; text: string; to?: string; tone: "warn" | "drift" }> = [];

  if (stats.status?.state === "failed") {
    items.push({
      key: "failed",
      text: t("desktop.dashboard.health.failed"),
      tone: "drift",
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
      tone: "drift",
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
      tone: "warn",
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
      tone: "warn",
    });
  }

  if (items.length === 0) {
    return null;
  }

  return (
    <section role="status" className="flex flex-wrap gap-2">
      {items.map((item) => {
        const className = `status-chip ${item.tone}`;
        const content = (
          <>
            <span aria-hidden="true" className="dot" />
            {item.text}
          </>
        );
        return (
          <div key={item.key}>
            {item.to ? (
              <Link to={item.to} className={className}>
                {content}
              </Link>
            ) : (
              <span className={className}>{content}</span>
            )}
          </div>
        );
      })}
    </section>
  );
}
