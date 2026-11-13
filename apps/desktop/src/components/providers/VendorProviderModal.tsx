import { useMemo, useState } from "react";

import { Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@any-converter/ui";

import { AUTH_SCHEME_OPTIONS, PROVIDER_FORMATS, PROVIDER_PRESETS } from "../../lib/constants";
import {
  management,
  useServerRequest,
  type AuthSchemeOption,
  type ConfigFormat,
  type DiscoveryReport,
} from "../../lib/management";
import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { Modal } from "./Modal";
import { ModelPicker, type PickerModel } from "./ModelPicker";
import { useProvidersStrings } from "./strings";

/** Every format's own URL box, seeded empty (magpie keeps one per protocol). */
type UrlDraft = Record<ConfigFormat, string>;

const EMPTY_URLS: UrlDraft = {
  openai_responses: "",
  openai_chat: "",
  claude: "",
  gemini: "",
};

/** Which URL hint fits a format (magpie's urlHint, in ac's words). */
function urlHint(format: ConfigFormat, s: ReturnType<typeof useProvidersStrings>): string {
  switch (format) {
    case "openai_responses":
      return s.urlHintResponses;
    case "openai_chat":
      return s.urlHintChat;
    case "claude":
      return s.urlHintClaude;
    case "gemini":
      return s.urlHintGemini;
  }
}

/**
 * The custom provider editor as a modal, laid out the way magpie's is:
 * name, a protocol segmented control where each protocol keeps its own
 * base URL, the key with show/hide and a get-a-key link, a probe that
 * fills the model picker, the auth scheme under Advanced, and a footer
 * of cancel and add. Enter in any field adds.
 */
export function VendorProviderModal({
  presetId,
  onDone,
  onClose,
}: {
  presetId: string;
  /** Called after a successful create; the parent refreshes and closes. */
  onDone: () => Promise<void>;
  onClose: () => void;
}) {
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const preset = PROVIDER_PRESETS.find((entry) => entry.id === presetId) ?? PROVIDER_PRESETS[0];
  const isCustom = preset.id === "custom";

  const [name, setName] = useState<string>(preset.name);
  const [format, setFormat] = useState<ConfigFormat>(preset.format);
  const [urls, setUrls] = useState<UrlDraft>(() => ({ ...EMPTY_URLS, [preset.format]: preset.base_url }));
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [scheme, setScheme] = useState<AuthSchemeOption>("auto");

  const [report, setReport] = useState<DiscoveryReport>();
  const [selected, setSelected] = useState<string[]>([]);
  const [probing, setProbing] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string>();
  const [createRoutes, setCreateRoutes] = useState(true);

  const baseUrl = urls[format];
  const canAdd = name.trim().length > 0 && baseUrl.trim().length > 0 && selected.length > 0 && !creating;

  /** magpie's carried URL: a typed URL follows a protocol switch when the
   * target protocol has none of its own; the seed stays behind. */
  const switchFormat = (next: ConfigFormat) => {
    if (next === format) return;
    setUrls((current) => {
      const seeded = format === preset.format ? preset.base_url : "";
      if (!current[next] && current[format] && current[format] !== seeded) {
        return { ...current, [next]: current[format], [format]: seeded };
      }
      return current;
    });
    setFormat(next);
    // each protocol probes its own address; the old report is not its
    setReport(undefined);
    setSelected([]);
  };

  const discover = async () => {
    setProbing(true);
    setError(undefined);
    try {
      const next = await management.discoverProvider({
        format,
        base_url: baseUrl.trim(),
        api_key: apiKey.trim(),
      });
      setReport(next);
      if (next.status === "ok") {
        setSelected(next.models.map((model) => model.id));
      }
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setProbing(false);
    }
  };

  const create = async () => {
    if (!canAdd) return;
    setCreating(true);
    setError(undefined);
    try {
      const current = await management.getConfig();
      const modelMap = Object.fromEntries(selected.map((id) => [id, id]));
      const routes =
        createRoutes && current.model_routes.length === 0
          ? selected.map((pattern) => ({
              pattern,
              providers: [name.trim()],
              upstream_model: null,
              strategy: "priority" as const,
            }))
          : [];
      const provider: {
        name: string;
        format: ConfigFormat;
        base_url: string;
        model_map: Record<string, string>;
        auth_scheme?: AuthSchemeOption;
      } = { name: name.trim(), format, base_url: baseUrl.trim(), model_map: modelMap };
      if (scheme !== "auto") {
        provider.auth_scheme = scheme;
      }
      const next = await management.patchConfig(current.revision, {
        upsert_providers: [provider],
        upsert_model_routes: routes,
      });
      if (apiKey.trim()) {
        await management.putProviderSecret(next.revision, name.trim(), apiKey.trim());
      }
      await onDone();
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setCreating(false);
    }
  };

  /** Enter adds (magpie's key box does); ESC closes, the Modal handles it. */
  const enterAdds = (event: React.KeyboardEvent) => {
    if (event.key === "Enter" && canAdd) {
      event.preventDefault();
      void create();
    }
  };

  const pickerModels = useMemo<PickerModel[]>(() => {
    const seen = new Map<string, PickerModel>();
    for (const model of report?.models ?? []) {
      seen.set(model.id, model);
    }
    for (const model of selected) {
      if (!seen.has(model)) {
        seen.set(model, { id: model });
      }
    }
    return [...seen.values()];
  }, [report, selected]);

  return (
    <Modal
      wide
      title={
        <span className="flex flex-wrap items-center gap-2" data-testid="vendor-modal">
          <span className="grid h-7 w-7 place-items-center rounded bg-muted text-xs font-semibold uppercase">
            {(isCustom ? name || s.addCustom : preset.label).slice(0, 2)}
          </span>
          {isCustom ? s.addCustom : preset.label}
          {!isCustom && preset.keys_url && (
            <a
              className="text-sm font-normal underline"
              href={preset.keys_url}
              target="_blank"
              rel="noreferrer"
            >
              {s.getAKey} ↗
            </a>
          )}
        </span>
      }
      onClose={onClose}
    >
      <div className="grid gap-4">
        <div className="grid gap-4 md:grid-cols-2">
          <Field label={s.name} help={s.nameHelp}>
            <Input
              value={name}
              placeholder={s.namePlaceholder}
              onChange={(event) => setName(event.target.value)}
              onKeyDown={enterAdds}
              data-testid="vendor-name"
            />
          </Field>

          {/* the protocol segmented control: each keeps its own URL below */}
          <Field label={s.format} help={s.formatHelp}>
            <div className="flex flex-wrap gap-1 rounded-md border bg-muted/40 p-1" data-testid="vendor-format">
              {PROVIDER_FORMATS.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  className={`rounded px-2.5 py-1.5 text-xs transition-colors ${
                    format === option.value
                      ? "bg-background font-medium shadow-sm"
                      : "text-muted-foreground hover:bg-background/60"
                  }`}
                  onClick={() => switchFormat(option.value)}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </Field>
        </div>

        <Field label={s.baseUrl} help={urlHint(format, s)}>
          <div className="flex gap-2">
            <Input
              className="min-w-0 flex-1"
              value={baseUrl}
              placeholder={preset.base_url || "https://"}
              onChange={(event) =>
                setUrls((current) => ({ ...current, [format]: event.target.value }))
              }
              onKeyDown={enterAdds}
              data-testid="vendor-base-url"
            />
            <Button
              variant="outline"
              className="shrink-0"
              disabled={probing || !baseUrl.trim()}
              onClick={() => void discover()}
              data-testid="vendor-probe"
            >
              {probing ? s.probing : s.probe}
            </Button>
          </div>
        </Field>

        <Field label={s.apiKey} help={s.apiKeyHelp}>
          <div className="flex gap-2">
            <Input
              className="min-w-0 flex-1"
              type={showKey ? "text" : "password"}
              value={apiKey}
              onChange={(event) => setApiKey(event.target.value)}
              onKeyDown={enterAdds}
              data-testid="vendor-api-key"
            />
            <Button variant="outline" className="shrink-0" onClick={() => setShowKey((shown) => !shown)}>
              {showKey ? s.hideKey : s.showKey}
            </Button>
          </div>
        </Field>

        {(report?.status === "ok" || pickerModels.length > 0) && (
          <div className="grid gap-2">
            {report?.status === "ok" && (
              <p className="text-sm text-emerald-600 dark:text-emerald-400">
                {s.discoveryOk} · {report.count} ({report.latency_ms} ms)
              </p>
            )}
            <ModelPicker
              models={pickerModels}
              selected={selected}
              onChange={setSelected}
              labels={{
                search: s.searchModels,
                selectAll: s.selectAll,
                clear: s.clearSelection,
                empty: s.noModelsFound,
                noMatches: s.noMatches,
                selectedCount: s.selectedCount,
              }}
            />
          </div>
        )}
        {report && report.status !== "ok" && report.error && (
          <p className="text-sm text-destructive">
            {s.discoveryFailed} — {report.error.message}
            {report.error.hint ? ` (${report.error.hint})` : ""}
          </p>
        )}
        {selected.length === 0 && !probing && (
          <p className="text-xs text-muted-foreground">{s.probeFirstHint}</p>
        )}

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="h-4 w-4 accent-primary"
            checked={createRoutes}
            onChange={(event) => setCreateRoutes(event.target.checked)}
          />
          {s.createRoutes}
        </label>

        {/* Advanced: the auth header story, as magpie's Key header row */}
        <details className="rounded-md border px-3 py-2">
          <summary className="cursor-pointer select-none text-sm text-muted-foreground">{s.advanced}</summary>
          <div className="grid gap-3 pt-3">
            <Field label={s.authScheme} help={s.authSchemeHelp}>
              <Select value={scheme} onValueChange={(next) => setScheme(next as AuthSchemeOption)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {AUTH_SCHEME_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.labelKey.slice(option.labelKey.lastIndexOf(".") + 1)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
        </details>

        {error && <ErrorBanner message={error} />}

        <div className="flex justify-end gap-2 border-t pt-4">
          <Button variant="ghost" onClick={onClose}>
            {s.cancel}
          </Button>
          <Button disabled={!canAdd} onClick={() => void create()} data-testid="vendor-add">
            {creating ? s.adding : s.addProvider}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
