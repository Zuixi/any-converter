import { useEffect, useRef, useState } from "react";

import { Button, Input } from "@any-converter/ui";

import { PLUGIN_CATALOG, SUBSCRIPTION_PRESETS } from "../../lib/constants";
import { useServerRequest, management, type AccountEntry, type SignInTask } from "../../lib/management";
import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { Modal } from "./Modal";
import { accountForTask, hintFor, useProvidersStrings } from "./strings";

/**
 * One sign-in as a modal: the login URL front and centre with copy and
 * reopen beside it, a paste-back row for server / container users, and the
 * browser opened automatically — falling back to explicit clicks whenever
 * the webview refuses. Polls until the task settles; closing the modal
 * cancels a still-waiting task.
 */
export function SignInFlow({
  provider,
  spec,
  autoStart,
  accounts,
  onDone,
  onClose,
}: {
  /** Sign-in provider id, or `plugin:<id>` for a plugin's provider. */
  provider: string;
  /** npm spec, when `provider` is a plugin's. */
  spec?: string;
  /** Begin as soon as mounted (a tile click already chose this). */
  autoStart?: boolean;
  /** Current accounts, to name the one that finished. */
  accounts: AccountEntry[];
  /** Called with the settled task; the parent refreshes its own state. */
  onDone?: (task: SignInTask) => void;
  /** Closes the modal; a waiting task is cancelled first. */
  onClose: () => void;
}) {
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const [task, setTask] = useState<SignInTask | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [pastedUrl, setPastedUrl] = useState("");
  const [copied, setCopied] = useState(false);
  const [autoOpenFailed, setAutoOpenFailed] = useState(false);
  // One automatic open per URL: polling re-sets the task every second, so
  // a plain flag would re-open the browser on every tick.
  const openedFor = useRef<string | null>(null);

  useEffect(() => {
    if (autoStart) {
      void start();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Poll an in-flight sign-in every second until it settles.
  useEffect(() => {
    if (!task || (task.state !== "waiting" && task.state !== "installing")) return;
    const timer = window.setInterval(() => {
      management
        .pollSignIn(task.id)
        .then((latest) => {
          setTask(latest);
          if (latest.state === "done" || latest.state === "failed") {
            onDone?.(latest);
          }
        })
        .catch(() => {
          /* transient poll errors keep the task waiting */
        });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [task, onDone]);

  // Open the login page by default. The URL arrives one fetch after the
  // click, so popup blockers may refuse — then the notice asks the user
  // to use the link or the copy button instead. (No `noopener` in the
  // features: it makes window.open return null even on success. The
  // opener is cleared by hand instead.)
  useEffect(() => {
    const url = task?.url;
    if (!url || task.state !== "waiting" || openedFor.current === url) return;
    openedFor.current = url;
    const opened = window.open(url, "_blank");
    if (opened) {
      opened.opener = null;
    } else {
      setAutoOpenFailed(true);
    }
  }, [task]);

  const start = async () => {
    setBusy(true);
    setError(undefined);
    setPastedUrl("");
    setAutoOpenFailed(false);
    openedFor.current = null;
    try {
      const payload: { provider: string; spec?: string } = { provider };
      if (spec) {
        payload.spec = spec;
      }
      setTask(await management.startSignIn(payload));
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    if (!task) return;
    setBusy(true);
    setError(undefined);
    try {
      const finished = await management.submitSignIn(task.id, pastedUrl);
      setTask(finished);
      if (finished.state === "done" || finished.state === "failed") {
        onDone?.(finished);
      }
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (!task) return;
    try {
      await management.cancelSignIn(task.id);
    } catch {
      // best effort
    }
    setTask(null);
  };

  /** ESC / backdrop / ✕: cancel what is still running, then let go. */
  const handleClose = () => {
    if (task && (task.state === "waiting" || task.state === "installing")) {
      void cancel();
    }
    onClose();
  };

  const copyUrl = async () => {
    const url = task?.url;
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      flashCopied();
    } catch {
      // clipboard API unavailable (some webviews): the deprecated
      // execCommand path still works, and the text stays selectable.
      const area = document.createElement("textarea");
      area.value = url;
      document.body.append(area);
      area.select();
      try {
        document.execCommand("copy");
        flashCopied();
      } catch {
        /* leave the URL selected for manual copy */
      }
      area.remove();
    }
  };

  const flashCopied = () => {
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  const title = flowTitle(provider, spec);

  return (
    <Modal title={`${title} · ${s.signIn}`} onClose={handleClose}>
      {provider === "antigravity" && (
        <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
          {s.antigravityRisk}
        </p>
      )}
      {provider === "chatgpt-api" && (
        <p className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          {s.siwcHint}
        </p>
      )}

      {!task ? (
        <div className="grid gap-3">
          <Button className="w-fit" onClick={() => void start()} disabled={busy}>
            {s.start}
          </Button>
          {error && <ErrorBanner message={error} />}
        </div>
      ) : (
        <div className="grid gap-4">
          {task.state === "installing" && (
            <p className="text-sm text-muted-foreground">{s.stateInstalling}</p>
          )}
          <p className="text-sm text-muted-foreground">{task.instructions}</p>

          {task.state === "waiting" && (
            <>
              {/* ① the login page */}
              <section className="grid gap-2">
                <h3 className="text-sm font-semibold">
                  <span className="mr-1.5 rounded bg-muted px-1.5 py-0.5 text-xs">1</span>
                  {s.stepOpen}
                </h3>
                {task.url ? (
                  <>
                    <div className="flex items-start gap-2">
                      <code
                        className="min-w-0 flex-1 select-all break-all rounded-md border bg-muted/50 px-3 py-2 text-xs"
                        data-testid="signin-url"
                      >
                        {task.url}
                      </code>
                      <Button
                        size="sm"
                        variant="outline"
                        className="shrink-0"
                        onClick={() => void copyUrl()}
                      >
                        {copied ? s.copied : s.copy}
                      </Button>
                    </div>
                    <a
                      className="w-fit text-sm underline"
                      href={task.url}
                      target="_blank"
                      rel="noreferrer"
                      data-testid="signin-reopen"
                    >
                      ↗ {s.reopenUrl}
                    </a>
                    {autoOpenFailed && (
                      <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
                        {s.autoOpenFailed}
                      </p>
                    )}
                  </>
                ) : (
                  <p className="text-sm text-muted-foreground">{s.waitingForUrl}</p>
                )}
                {task.code && (
                  <div className="flex items-center gap-2">
                    <span className="text-xs uppercase text-muted-foreground">{s.code}:</span>
                    <code className="select-all rounded bg-muted px-2 py-1 text-lg font-semibold tracking-wider">
                      {task.code}
                    </code>
                  </div>
                )}
              </section>

              {/* ② for server / container users */}
              <section className="grid gap-2 border-t pt-4">
                <h3 className="text-sm font-semibold">
                  <span className="mr-1.5 rounded bg-muted px-1.5 py-0.5 text-xs">2</span>
                  {s.stepAuthorize}
                </h3>
                {(task.paste_callback || task.paste_code) && (
                  <>
                    <p className="text-xs text-muted-foreground">{s.pasteSectionHint}</p>
                    <div className="flex items-end gap-2">
                      <div className="min-w-0 flex-1">
                        <Field
                          label={task.paste_code ? s.pasteCodeLabel : s.pasteCallbackLabel}
                          help=""
                        >
                          <Input
                            value={pastedUrl}
                            placeholder={task.paste_code ? s.pasteCodeLabel : s.pastePlaceholder}
                            onChange={(event) => setPastedUrl(event.target.value)}
                            data-testid="signin-paste-input"
                          />
                        </Field>
                      </div>
                      <Button
                        onClick={() => void submit()}
                        disabled={busy || !pastedUrl.trim()}
                        data-testid="signin-finish"
                      >
                        {s.submit}
                      </Button>
                    </div>
                  </>
                )}
                <p className="text-xs text-muted-foreground" data-testid="signin-waiting">
                  {s.waitingForCallback}
                </p>
              </section>
            </>
          )}

          {task.state === "failed" && (
            <div className="grid gap-2 border-t pt-4">
              <ErrorBanner message={`${s.error}: ${task.error || "unknown"}`} />
              {hintFor(task.error || "", s) && (
                <p className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm">
                  {hintFor(task.error || "", s)}
                </p>
              )}
              <Button
                size="sm"
                variant="outline"
                className="w-fit"
                onClick={() => void start()}
                disabled={busy}
              >
                {s.retry}
              </Button>
            </div>
          )}

          {task.state === "done" && (
            <div className="grid gap-3 border-t pt-4">
              <SignInResult task={task} accounts={accounts} />
              <Button className="w-fit" onClick={onClose} data-testid="signin-done-close">
                {s.close}
              </Button>
            </div>
          )}

          {error && <ErrorBanner message={error} />}

          {(task.state === "waiting" || task.state === "installing") && (
            <div className="flex items-center justify-between border-t pt-4">
              <span className="flex items-center gap-2 text-sm text-muted-foreground">
                <span className="relative flex h-2 w-2">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-primary/60" />
                  <span className="relative inline-flex h-2 w-2 rounded-full bg-primary" />
                </span>
                {s.stateWaiting}
              </span>
              <Button size="sm" variant="ghost" onClick={handleClose}>
                {s.cancel}
              </Button>
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

/** The human name of a sign-in target: preset label, plugin label, or id. */
function flowTitle(provider: string, spec?: string): string {
  const preset = SUBSCRIPTION_PRESETS.find((entry) => entry.id === provider);
  if (preset) {
    return preset.label;
  }
  if (spec) {
    return PLUGIN_CATALOG.find((entry) => entry.spec === spec)?.label ?? spec;
  }
  return provider.replace(/^plugin:/, "");
}

function SignInResult({ task, accounts }: { task: SignInTask; accounts: AccountEntry[] }) {
  const s = useProvidersStrings();
  const account = accountForTask(task, accounts);
  return (
    <div className="grid gap-1 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2">
      <span className="text-sm font-medium">{s.signedInFlowDone}</span>
      <span className="text-sm text-muted-foreground">
        {task.provider}: {task.user || "—"}
        {account ? ` (${account.id})` : ""}
      </span>
      <span className="text-[11px] text-muted-foreground">{s.routeConfigHint}</span>
    </div>
  );
}
