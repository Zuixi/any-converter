import { useNavigate } from "react-router-dom";
import { useI18n, type TranslationKey } from "@any-converter/core";

import type { SidebarSignals } from "../../hooks/useSidebarSignals";
import type { ServerStatus } from "../../types";

type GatewayState = ServerStatus["state"];

const STATE_STYLES: Record<GatewayState, { dotClass: string; labelKey: TranslationKey }> = {
  running: { dotClass: "status-dot ok", labelKey: "desktop.status.running" },
  attached: { dotClass: "status-dot ok", labelKey: "desktop.status.attached" },
  starting: { dotClass: "status-dot warn", labelKey: "desktop.status.starting" },
  stopping: { dotClass: "status-dot warn", labelKey: "desktop.status.stopping" },
  stopped: { dotClass: "status-dot", labelKey: "desktop.status.stopped" },
  failed: { dotClass: "status-dot error", labelKey: "desktop.status.failed" },
};

/**
 * The one fact this product can never hide: is the endpoint up, where is it,
 * who is on it. Always visible at the top of the sidebar; a click jumps to
 * the dashboard, which owns the lifecycle controls.
 */
export function GatewayStatus({
  collapsed,
  signals,
}: {
  collapsed: boolean;
  signals: SidebarSignals;
}) {
  const { t } = useI18n();
  const navigate = useNavigate();

  // Probe still in flight -> "Loading"; settled without an answer -> "Unknown".
  const state = signals.status ? STATE_STYLES[signals.status.state] : undefined;
  const label = !signals.statusKnown
    ? t("common.loading")
    : (state?.labelKey ? t(state.labelKey) : t("desktop.status.unknown"));
  const dotClass = signals.statusKnown && !signals.status
    ? "status-dot error"
    : (state?.dotClass ?? "status-dot");

  const address = signals.status ? `${signals.status.host}:${signals.status.port}` : "";
  const counts =
    signals.connectedAgents !== undefined && signals.providerCount !== undefined
      ? t("desktop.sidebar.stats")
          .replace("{agents}", String(signals.connectedAgents))
          .replace("{providers}", String(signals.providerCount))
      : "";
  const detail = [address, counts].filter(Boolean).join(" · ");
  const summary = detail ? `${label} · ${detail}` : label;

  return (
    <button
      type="button"
      className="gateway-status"
      onClick={() => navigate("/dashboard")}
      title={summary}
      aria-label={summary}
    >
      <span className={dotClass} aria-hidden="true" />
      {!collapsed && (
        <span className="gateway-status-body">
          <span className="gateway-status-label">{label}</span>
          {detail && <span className="gateway-status-detail">{detail}</span>}
        </span>
      )}
    </button>
  );
}
