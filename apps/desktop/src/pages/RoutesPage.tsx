import { useMemo, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Badge, Button, Card } from "@any-converter/ui";

import { DeleteRouteDialog } from "../components/routes/DeleteRouteDialog";
import { RouteEditor } from "../components/routes/RouteEditor";
import {
  draftFromRoute,
  emptyDraft,
  isWildcard,
  type ProviderLite,
  type RouteDraft,
  type UpstreamOption,
} from "../components/routes/types";
import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Header } from "../components/layout/Header";
import { useAsyncState } from "../hooks/useAsyncState";
import {
  management,
  useServerRequest,
  type AdminConfigView,
  type CatalogModel,
  type CatalogView,
  type ConfigPatch,
} from "../lib/management";

type EditorState = { mode: "create"; draft: RouteDraft } | { mode: "edit"; original: string; draft: RouteDraft };

export function RoutesPage() {
  const { t } = useI18n();
  const { describeError } = useServerRequest();
  const [config, setConfig, loadError, , configLoading] = useAsyncState<AdminConfigView>(() =>
    management.getConfig().catch((cause) => {
      throw new Error(describeError(cause));
    }),
  );
  const [catalog, , catalogError, , catalogLoading] = useAsyncState<CatalogView>(() => management.getCatalog());

  const [editor, setEditor] = useState<EditorState | null>(null);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string>();

  const routes = config?.model_routes ?? [];
  const providers: ProviderLite[] = (config?.providers ?? []).map((provider) => ({
    name: provider.name,
    format: provider.format,
  }));

  // Route patterns match the model ids clients send. Offer the ids the
  // gateway already exposes, so a pattern is never typed blind.
  const patternOptions = useMemo(() => {
    const ids = new Set<string>();
    for (const model of catalog?.models ?? []) {
      if (model.exposed) {
        ids.add(model.id);
      }
    }
    for (const provider of config?.providers ?? []) {
      for (const model of Object.keys(provider.model_map)) {
        if (!isWildcard(model)) {
          ids.add(model);
        }
      }
    }
    return [...ids].sort();
  }, [catalog, config]);

  // Upstream ids are associated with their providers so each route member
  // can pick a model valid for that provider.
  const allUpstream = useMemo<UpstreamOption[]>(() => {
    const owners = new Map<string, Set<string>>();
    const add = (upstream: string | null | undefined, provider: string) => {
      if (!upstream) {
        return;
      }
      const set = owners.get(upstream) ?? new Set<string>();
      set.add(provider);
      owners.set(upstream, set);
    };

    for (const provider of config?.providers ?? []) {
      for (const upstream of Object.values(provider.model_map)) {
        add(upstream, provider.name);
      }
    }
    const catalogModels: CatalogModel[] = catalog?.models ?? [];
    for (const model of catalogModels) {
      if (model.source === "upstream") {
        add(model.upstream, model.provider);
      }
    }

    return [...owners.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([value, set]) => ({ value, providers: [...set].sort() }));
  }, [catalog, config]);

  // A save is keyed by pattern, so a colliding pattern silently replaces
  // another route. Surface that before the write, never after.
  const editorOriginal = editor?.mode === "edit" ? editor.original : null;
  const collidesWith =
    editor && editor.draft.pattern.trim()
      ? (routes.find((route) => route.pattern === editor.draft.pattern.trim() && route.pattern !== editorOriginal)
          ?.pattern ?? null)
      : null;

  const updateDraft = (draft: RouteDraft) => {
    setEditor((current) => {
      if (!current) {
        return current;
      }
      return current.mode === "create"
        ? { mode: "create", draft }
        : { mode: "edit", original: current.original, draft };
    });
  };

  const startCreate = () => {
    setActionError(undefined);
    setEditor({ mode: "create", draft: emptyDraft() });
  };

  const startEdit = (route: AdminConfigView["model_routes"][number]) => {
    setActionError(undefined);
    setEditor({ mode: "edit", original: route.pattern, draft: draftFromRoute(route) });
  };

  const save = async () => {
    if (!editor) {
      return;
    }
    const pattern = editor.draft.pattern.trim();
    if (!pattern || !editor.draft.members.some((member) => member.enabled)) {
      return;
    }
    setBusy(true);
    setActionError(undefined);
    try {
      // The mutation's precondition must be the revision observed right
      // before the write, not one cached when the page first loaded.
      const current = await management.getConfig();
      const patch: ConfigPatch = {
        upsert_model_routes: [
          {
            pattern,
            members: editor.draft.members.map((member) => ({
              provider: member.provider,
              model: member.model.trim() || null,
              weight: member.weight,
              enabled: member.enabled,
              fallback: member.fallback,
            })),
            strategy: editor.draft.strategy,
          },
        ],
      };
      // Renaming a route means its pattern — the server-side identity —
      // changes; drop the old entry in the same patch. Skip the removal if
      // another client already deleted it, so the save still lands.
      if (
        editor.mode === "edit" &&
        editor.original !== pattern &&
        current.model_routes.some((route) => route.pattern === editor.original)
      ) {
        patch.remove_model_routes = [editor.original];
      }
      const next = await management.patchConfig(current.revision, patch);
      setConfig(next);
      setEditor(null);
    } catch (cause) {
      setActionError(describeError(cause));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (pendingDelete === null) {
      return;
    }
    setBusy(true);
    setActionError(undefined);
    try {
      const current = await management.getConfig();
      const next = await management.patchConfig(current.revision, {
        remove_model_routes: [pendingDelete],
      });
      setConfig(next);
      setPendingDelete(null);
    } catch (cause) {
      setActionError(describeError(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.routes")} subtitle={t("desktop.routes.subtitle")} />
      {(loadError || actionError) && <ErrorBanner message={loadError ?? actionError ?? ""} />}
      {config?.restart_required && (
        <p className="text-sm text-muted-foreground">{t("desktop.config.restartRequired")}</p>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm text-muted-foreground">
          {config ? `${routes.length} ${t("desktop.routes.routeCount")}` : "—"}
        </span>
        <span className="grow" />
        <Button onClick={startCreate} disabled={editor !== null || configLoading || !config}>
          {t("desktop.routes.add")}
        </Button>
      </div>

      {catalogLoading && <p className="text-xs text-muted-foreground">{t("desktop.routes.catalogLoading")}</p>}
      {catalogError && <p className="text-xs text-muted-foreground">{t("desktop.routes.catalogUnavailable")}</p>}

      {editor?.mode === "create" && (
        <RouteEditor
          mode="create"
          draft={editor.draft}
          onChange={updateDraft}
          providers={providers}
          patternOptions={patternOptions}
          allUpstream={allUpstream}
          collidesWith={collidesWith}
          busy={busy}
          onCancel={() => setEditor(null)}
          onSave={() => void save()}
        />
      )}

      <p className="flex items-start gap-2 rounded-md border bg-muted/30 px-3 py-2 text-xs leading-5 text-muted-foreground">
        <span aria-hidden="true">ℹ</span>
        {t("desktop.routes.matchingHint")}
      </p>

      {routes.length === 0 ? (
        <Card className="px-5 py-8 text-center">
          <p className="text-sm text-muted-foreground">
            {configLoading
              ? t("desktop.routes.loading")
              : loadError
                ? t("desktop.routes.loadError")
                : t("desktop.routes.empty")}
          </p>
        </Card>
      ) : (
        <div className="grid gap-3">
          {routes.map((route) => {
            const editing = editor?.mode === "edit" && editor.original === route.pattern;
            if (editing) {
              return (
                <RouteEditor
                  key={route.pattern}
                  mode="edit"
                  draft={editor.draft}
                  onChange={updateDraft}
                  providers={providers}
                  patternOptions={patternOptions}
                  allUpstream={allUpstream}
                  collidesWith={collidesWith}
                  busy={busy}
                  onCancel={() => setEditor(null)}
                  onSave={() => void save()}
                />
              );
            }
            const displayMembers = [
              ...route.members.filter((member) => !member.fallback),
              ...route.members.filter((member) => member.fallback),
            ];
            const memberSummary = displayMembers
              .filter((member) => member.enabled)
              .map((member) => `${member.provider}/${member.model || "auto"}`)
              .join(" → ");
            return (
              <Card key={route.pattern} className="grid gap-3 p-4 transition-colors hover:border-primary/30">
                <div className="flex flex-wrap items-start gap-3">
                  <div
                    className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-muted text-sm font-semibold"
                    aria-hidden="true"
                  >
                    {route.members.length}
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <code className="font-mono text-sm font-semibold">{route.pattern}</code>
                      <Badge variant="outline">
                        {isWildcard(route.pattern) ? t("desktop.routes.wildcard") : t("desktop.routes.exact")}
                      </Badge>
                      <Badge variant="secondary">
                        {route.strategy === "priority"
                          ? t("desktop.routes.priority")
                          : route.strategy === "round_robin"
                            ? t("desktop.routes.roundRobin")
                            : t("desktop.routes.weighted")}
                      </Badge>
                    </div>
                    <p className="mt-1 truncate text-sm text-muted-foreground" title={memberSummary}>
                      {memberSummary || t("desktop.routes.membersEmpty")}
                    </p>
                  </div>
                  <div className="flex shrink-0 gap-1">
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={editor !== null || busy}
                      onClick={() => startEdit(route)}
                    >
                      {t("desktop.routes.edit")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="text-muted-foreground hover:text-destructive"
                      disabled={editor !== null || busy}
                      onClick={() => {
                        setActionError(undefined);
                        setPendingDelete(route.pattern);
                      }}
                    >
                      {t("desktop.routes.delete")}
                    </Button>
                  </div>
                </div>
                <div className="grid gap-2 border-t pt-3">
                  {displayMembers.map((member, index) => (
                    <div
                      key={`${member.provider}-${index}`}
                      className={`flex flex-wrap items-center gap-2 text-sm ${member.enabled ? "" : "opacity-50"}`}
                    >
                      <span className="grid h-6 w-6 place-items-center rounded-full bg-muted text-xs font-semibold">
                        {index + 1}
                      </span>
                      <span className="font-medium">{member.provider}</span>
                      <span className="text-muted-foreground">/</span>
                      <code className="font-mono text-xs">{member.model || t("desktop.routes.autoBadge")}</code>
                      <span className="text-xs text-muted-foreground">×{member.weight}</span>
                      {member.fallback && <Badge variant="outline">{t("desktop.routes.fallback")}</Badge>}
                      {!member.enabled && <Badge variant="outline">{t("desktop.routes.disabled")}</Badge>}
                    </div>
                  ))}
                </div>
              </Card>
            );
          })}
        </div>
      )}

      {pendingDelete !== null && (
        <DeleteRouteDialog
          pattern={pendingDelete}
          busy={busy}
          onCancel={() => {
            if (!busy) {
              setPendingDelete(null);
            }
          }}
          onConfirm={() => void remove()}
        />
      )}
    </section>
  );
}
