import type React from "react";
import { useEffect, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Button, Label, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@any-converter/ui";

import { ModelCombobox } from "./ModelCombobox";
import { RouteMemberList } from "./RouteMemberList";
import type { ProviderLite, RouteDraft, UpstreamOption } from "./types";

interface RouteEditorProps {
  mode: "create" | "edit";
  draft: RouteDraft;
  onChange: (draft: RouteDraft) => void;
  providers: ProviderLite[];
  patternOptions: string[];
  allUpstream: UpstreamOption[];
  collidesWith?: string | null;
  busy: boolean;
  onCancel: () => void;
  onSave: () => void;
}

function EditorField({ label, help, children }: { label: string; help?: string; children: React.ReactNode }) {
  return (
    <div className="grid content-start gap-2">
      <Label>{label}</Label>
      {children}
      {help && <span className="text-xs leading-5 text-muted-foreground">{help}</span>}
    </div>
  );
}

export function RouteEditor({
  mode,
  draft,
  onChange,
  providers,
  patternOptions,
  allUpstream,
  collidesWith,
  busy,
  onCancel,
  onSave,
}: RouteEditorProps) {
  const { t } = useI18n();
  const [confirmedCollision, setConfirmedCollision] = useState(false);
  useEffect(() => setConfirmedCollision(false), [collidesWith]);
  const patternValid = draft.pattern.trim().length > 0;
  const hasEnabledMember = draft.members.some((member) => member.enabled);
  const canSave = patternValid && hasEnabledMember && !busy && (!collidesWith || confirmedCollision);
  const set = (patch: Partial<RouteDraft>) => onChange({ ...draft, ...patch });

  return (
    <div className="grid gap-4 rounded-lg border bg-card p-4 shadow-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-base font-semibold">
          {mode === "create" ? t("desktop.routes.addTitle") : t("desktop.routes.editTitle")}
        </h2>
        {mode === "create" && <span className="text-xs text-muted-foreground">{t("desktop.routes.upsertHint")}</span>}
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <EditorField
          label={t("desktop.routes.pattern")}
          help={
            patternValid
              ? t("desktop.routes.patternHelp")
              : `${t("desktop.routes.patternHelp")} ${t("desktop.routes.patternRequired")}`
          }
        >
          <ModelCombobox
            id="route-pattern"
            value={draft.pattern}
            onChange={(pattern) => set({ pattern })}
            options={patternOptions.map((value) => ({ value }))}
            allowCustom
            placeholder={t("desktop.routes.patternPlaceholder")}
            searchPlaceholder={t("desktop.routes.patternSearch")}
            emptyText={t("desktop.routes.patternEmpty")}
            noMatchesText={t("desktop.routes.patternNoMatches")}
            customLabel={t("desktop.routes.patternUseCustom")}
            ariaLabel={t("desktop.routes.pattern")}
          />
        </EditorField>

        <EditorField label={t("desktop.routes.strategy")}>
          <Select
            value={draft.strategy}
            onValueChange={(strategy) =>
              set({
                strategy: strategy === "round_robin" || strategy === "weighted" ? strategy : "priority",
              })
            }
            disabled={busy}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="priority">{t("desktop.routes.priority")}</SelectItem>
              <SelectItem value="round_robin">{t("desktop.routes.roundRobin")}</SelectItem>
              <SelectItem value="weighted">{t("desktop.routes.weighted")}</SelectItem>
            </SelectContent>
          </Select>
        </EditorField>
      </div>

      {collidesWith && (
        <div
          role="alert"
          className="grid gap-2 rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm text-amber-700 dark:text-amber-300"
        >
          <p>{t("desktop.routes.upsertWarning").replace("{pattern}", collidesWith)}</p>
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={confirmedCollision}
              onChange={(event) => setConfirmedCollision(event.target.checked)}
              disabled={busy}
              className="mt-0.5 accent-primary"
            />
            <span>{t("desktop.routes.confirmOverwrite")}</span>
          </label>
        </div>
      )}

      <EditorField label={t("desktop.routes.members")}>
        {providers.length === 0 ? (
          <p className="rounded-md border border-dashed px-3 py-4 text-sm text-muted-foreground">
            {t("desktop.routes.noProviders")}
          </p>
        ) : (
          <RouteMemberList
            providers={providers}
            members={draft.members}
            strategy={draft.strategy}
            upstreamOptions={allUpstream}
            onChange={(members) => set({ members })}
            disabled={busy}
            labels={{
              help: t("desktop.routes.membersHelp"),
              empty: t("desktop.routes.membersEmpty"),
              addProvider: t("desktop.routes.addMember"),
              remove: t("desktop.routes.removeFromPool"),
              moveUp: t("desktop.routes.moveUp"),
              moveDown: t("desktop.routes.moveDown"),
              first: t("desktop.routes.first"),
              fallback: t("desktop.routes.fallback"),
              primary: t("desktop.routes.primary"),
              disabled: t("desktop.routes.disabled"),
              enabled: t("desktop.routes.enabled"),
              model: t("desktop.routes.memberModel"),
              auto: t("desktop.routes.memberAuto"),
              search: t("desktop.routes.upstreamSearch"),
              noModels: t("desktop.routes.upstreamEmpty"),
              noMatches: t("desktop.routes.upstreamNoMatches"),
              useCustom: t("desktop.routes.upstreamUseCustom"),
              weight: t("desktop.routes.weight"),
              disable: t("desktop.routes.disableMember"),
              enable: t("desktop.routes.enableMember"),
              makeFallback: t("desktop.routes.makeFallback"),
              makePrimary: t("desktop.routes.makePrimary"),
            }}
          />
        )}
      </EditorField>

      <p className="text-xs leading-5 text-muted-foreground">
        {draft.strategy === "priority"
          ? t("desktop.routes.priorityHelp")
          : draft.strategy === "round_robin"
            ? t("desktop.routes.roundRobinHelp")
            : t("desktop.routes.weightedHelp")}
      </p>

      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" disabled={busy} onClick={onCancel}>
          {t("desktop.routes.cancel")}
        </Button>
        <Button type="button" disabled={!canSave} onClick={onSave}>
          {busy ? t("desktop.routes.saving") : t("desktop.routes.save")}
        </Button>
      </div>
    </div>
  );
}
