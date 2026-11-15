import { useRef } from "react";

import { api } from "../lib/api";
import { management, type AgentSummary } from "../lib/management";
import type { ServerStatus } from "../types";
import { usePolling } from "./usePolling";

/** Status/agents refresh cadence; loopback-only traffic. */
const POLL_INTERVAL_MS = 8000;
/** The catalog payload is the heavy one; refetch it every Nth tick only. */
const CATALOG_EVERY_N_TICKS = 3;

export interface SidebarSignals {
  /** Gateway process state; undefined when the last probe failed. */
  status: ServerStatus | undefined;
  /** True once the first status probe has settled (answer or failure). */
  statusKnown: boolean;
  /** Agents currently wired to the gateway; undefined when unreachable. */
  connectedAgents: number | undefined;
  /** Connected agents whose config another tool rewrote; same rule. */
  driftedAgents: number | undefined;
  /** Configured providers; same rule. */
  providerCount: number | undefined;
}

const EMPTY_SIGNALS: SidebarSignals = {
  status: undefined,
  statusKnown: false,
  connectedAgents: undefined,
  driftedAgents: undefined,
  providerCount: undefined,
};

function signalsEqual(a: SidebarSignals, b: SidebarSignals): boolean {
  return (
    a.status === b.status &&
    a.statusKnown === b.statusKnown &&
    a.connectedAgents === b.connectedAgents &&
    a.driftedAgents === b.driftedAgents &&
    a.providerCount === b.providerCount
  );
}

function countByStatus(
  agents: AgentSummary[] | undefined,
  status: AgentSummary["status"],
): number | undefined {
  return agents?.filter((agent) => agent.status === status).length;
}

/**
 * One poll round; every probe degrades to undefined instead of rejecting.
 * When `withCatalog` is false the previous provider count is carried over.
 */
async function fetchSignals(
  previous: SidebarSignals,
  withCatalog: boolean,
): Promise<SidebarSignals> {
  const [status, agents, catalog] = await Promise.all([
    api.getServerStatus().catch(() => undefined),
    management.listAgents().catch(() => undefined),
    withCatalog
      ? management.getCatalog().catch(() => undefined)
      : Promise.resolve(undefined),
  ]);
  return {
    status,
    statusKnown: true,
    connectedAgents: countByStatus(agents, "connected"),
    driftedAgents: countByStatus(agents, "drifted"),
    providerCount: catalog ? catalog.providers.length : previous.providerCount,
  };
}

/**
 * The gateway facts the sidebar always shows: process state plus the counts
 * behind the nav badges. A stopped/unreachable gateway dims the sidebar
 * (undefined counts) rather than erroring it.
 */
export function useSidebarSignals(): SidebarSignals {
  const tickCount = useRef(0);

  const { data } = usePolling(
    (previous) => {
      const withCatalog = tickCount.current % CATALOG_EVERY_N_TICKS === 0;
      tickCount.current += 1;
      return fetchSignals(previous ?? EMPTY_SIGNALS, withCatalog);
    },
    signalsEqual,
    POLL_INTERVAL_MS,
  );

  return data ?? EMPTY_SIGNALS;
}
