"use client";

import { useState } from "react";

import { LedgerView, UsageView } from "@any-converter/views";
import { Button } from "@any-converter/ui";
import { useI18n } from "@any-converter/core";

const TABS = [
  { value: "summary", key: "usage.summary" },
  { value: "ledger", key: "usage.ledger" },
] as const;

type Tab = (typeof TABS)[number]["value"];

export default function UsagePage() {
  const { t } = useI18n();
  const [tab, setTab] = useState<Tab>("summary");

  return (
    <div className="grid gap-6">
      <div className="flex gap-2" role="tablist">
        {TABS.map((item) => (
          <Button
            key={item.value}
            type="button"
            size="sm"
            role="tab"
            variant={tab === item.value ? "default" : "outline"}
            aria-selected={tab === item.value}
            onClick={() => setTab(item.value)}
          >
            {t(item.key)}
          </Button>
        ))}
      </div>
      {tab === "summary" ? <UsageView /> : <LedgerView />}
    </div>
  );
}
