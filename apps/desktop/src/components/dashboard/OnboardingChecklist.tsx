import { useState } from "react";
import { Link } from "react-router-dom";
import { useI18n } from "@any-converter/core";
import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";

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
 * route, agent, first request. Each step links to the page that completes
 * it; once everything is done it collapses to a single confirmation line.
 * Dismissal is the user's and sticks across sessions.
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
      <Card>
        <CardContent className="flex items-center justify-between gap-4 pt-6">
          <p className="flex items-center gap-2 text-sm">
            <span aria-hidden="true">✓</span>
            {t("desktop.dashboard.checklist.done")}
          </p>
          <button
            type="button"
            className="text-sm text-muted-foreground underline-offset-2 hover:underline"
            onClick={dismiss}
          >
            {t("desktop.dashboard.checklist.dismiss")}
          </button>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">{t("desktop.dashboard.checklist.title")}</CardTitle>
      </CardHeader>
      <CardContent className="grid gap-1">
        <ol>
          {steps.map((step, index) => (
            <li key={step.to}>
              <Link
                to={step.to}
                className="flex items-center gap-3 rounded-md px-2 py-2 no-underline hover:bg-accent"
              >
                <span
                  aria-hidden="true"
                  className={
                    step.done
                      ? "flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-full bg-primary text-xs font-bold text-primary-foreground"
                      : "flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-full border text-xs font-semibold text-muted-foreground"
                  }
                >
                  {step.done ? "✓" : index + 1}
                </span>
                <span className={step.done ? "flex-1 text-sm line-through opacity-60" : "flex-1 text-sm"}>
                  {step.label}
                </span>
                <span aria-hidden="true" className="text-sm text-muted-foreground">
                  →
                </span>
              </Link>
            </li>
          ))}
        </ol>
        <div className="mt-2 px-2">
          <button
            type="button"
            className="text-sm text-muted-foreground underline-offset-2 hover:underline"
            onClick={dismiss}
          >
            {t("desktop.dashboard.checklist.dismiss")}
          </button>
        </div>
      </CardContent>
    </Card>
  );
}
