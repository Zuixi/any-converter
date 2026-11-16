import { useState } from "react";
import { Link } from "react-router-dom";
import { useI18n } from "@any-converter/core";

import type { DashboardStats } from "../../hooks/useDashboardOverview";
import { ONBOARDING_DISMISSED_KEY } from "../../lib/constants";

function readDismissed(): boolean {
  try {
    return localStorage.getItem(ONBOARDING_DISMISSED_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * The getting-started checklist, derived live from the gateway: provider,
 * route, agent, first request. Each step is a row that links to the page
 * that completes it; done steps get a green check disc. Once everything is
 * done it collapses to a single confirmation row. Dismissal is the user's
 * and sticks across sessions.
 */
export function OnboardingChecklist({ stats }: { stats: DashboardStats }) {
  const { t } = useI18n();
  const [dismissed, setDismissed] = useState(readDismissed);

  if (dismissed) {
    return null;
  }

  const steps = [
    {
      to: "/providers",
      label: t("desktop.dashboard.checklist.provider"),
      done: (stats.providerCount ?? 0) > 0,
    },
    {
      to: "/routes",
      label: t("desktop.dashboard.checklist.route"),
      done: (stats.routableModels ?? 0) > 0,
    },
    {
      to: "/agents",
      label: t("desktop.dashboard.checklist.agent"),
      done: (stats.connectedAgents ?? 0) > 0,
    },
    {
      to: "/playground",
      label: t("desktop.dashboard.checklist.request"),
      done: (stats.calls ?? 0) > 0,
    },
  ];
  const allDone = steps.every((step) => step.done);

  const dismiss = () => {
    try {
      localStorage.setItem(ONBOARDING_DISMISSED_KEY, "1");
    } catch {
      // ignore persistence failures in restricted environments
    }
    setDismissed(true);
  };

  if (allDone) {
    return (
      <div className="list-card">
        <div className="lc-row">
          <span aria-hidden="true" className="step-disc done">
            ✓
          </span>
          <span className="lc-title flex-1">{t("desktop.dashboard.checklist.done")}</span>
          <button
            type="button"
            className="text-xs text-muted-foreground underline-offset-2 hover:underline"
            onClick={dismiss}
          >
            {t("desktop.dashboard.checklist.dismiss")}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="list-card">
      <div className="lc-head">
        {t("desktop.dashboard.checklist.title")}
        <button
          type="button"
          className="ml-auto text-xs font-normal text-muted-foreground underline-offset-2 hover:underline"
          onClick={dismiss}
        >
          {t("desktop.dashboard.checklist.dismiss")}
        </button>
      </div>
      <ol>
        {steps.map((step, index) => (
          <li key={step.to}>
            <Link to={step.to} className="lc-row no-underline">
              <span
                aria-hidden="true"
                className={`step-disc ${step.done ? "done" : "todo"}`}
              >
                {step.done ? "✓" : index + 1}
              </span>
              <span className={`lc-title flex-1 ${step.done ? "opacity-60 line-through" : ""}`}>
                {step.label}
              </span>
              <span aria-hidden="true" className="text-muted-foreground">
                →
              </span>
            </Link>
          </li>
        ))}
      </ol>
    </div>
  );
}
