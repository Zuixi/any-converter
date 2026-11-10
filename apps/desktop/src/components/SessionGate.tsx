import type { FormEvent, ReactNode } from "react";
import { useCallback, useEffect, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Button, Card, CardContent, Input } from "@any-converter/ui";

type GateState = "checking" | "open" | "locked" | "forbidden" | "unconfigured" | "offline";

/**
 * Browser-only page session gate. The desktop webview is authenticated by
 * the management bearer over IPC and never renders this component.
 *
 * The page key lives only in the controlled input while the user types it;
 * it is never put in the URL, DOM attributes, or storage.
 */
export function SessionGate({ children }: { children: ReactNode }) {
  const { t } = useI18n();
  const [state, setState] = useState<GateState>("checking");
  const [key, setKey] = useState("");
  const [error, setError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);

  const check = useCallback(async () => {
    setState("checking");
    try {
      const response = await fetch("/v1/session", { credentials: "include" });
      if (response.status === 204) {
        setState("open");
      } else if (response.status === 401) {
        setState("locked");
      } else if (response.status === 403) {
        setState("forbidden");
      } else if (response.status === 503) {
        setState("unconfigured");
      } else {
        setState("offline");
      }
    } catch {
      setState("offline");
    }
  }, []);

  useEffect(() => {
    void check();
  }, [check]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!key || submitting) {
      return;
    }
    setSubmitting(true);
    setError(undefined);
    try {
      const response = await fetch("/v1/session", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key }),
      });
      if (response.status === 204) {
        setKey("");
        setState("open");
      } else if (response.status === 401) {
        setError(t("session.invalid"));
      } else if (response.status === 429) {
        setError(t("session.rateLimited"));
      } else if (response.status === 403) {
        setState("forbidden");
      } else if (response.status === 503) {
        setState("unconfigured");
      } else {
        setState("offline");
      }
    } catch {
      setState("offline");
    } finally {
      setSubmitting(false);
    }
  };

  if (state === "checking") {
    return null;
  }
  if (state === "open") {
    return <>{children}</>;
  }
  if (state === "locked") {
    return (
      <CenteredCard>
        <h1 className="text-xl font-semibold">{t("session.title")}</h1>
        <p className="text-sm text-muted-foreground">{t("session.hint")}</p>
        <form onSubmit={(event) => void submit(event)} className="grid gap-4">
          <Input
            type="password"
            autoComplete="off"
            spellCheck={false}
            aria-label={t("session.key")}
            placeholder={t("session.key")}
            value={key}
            onChange={(event) => {
              setKey(event.target.value);
              setError(undefined);
            }}
          />
          {error && <p className="text-sm text-destructive">{error}</p>}
          <Button type="submit" disabled={submitting || !key}>
            {t("session.submit")}
          </Button>
        </form>
      </CenteredCard>
    );
  }
  if (state === "forbidden") {
    return <CenteredCard>{t("session.forbidden")}</CenteredCard>;
  }
  if (state === "unconfigured") {
    return <CenteredCard>{t("session.unconfigured")}</CenteredCard>;
  }
  return (
    <CenteredCard>
      <p className="text-sm text-muted-foreground">{t("session.offline")}</p>
      <Button variant="outline" onClick={() => void check()}>
        {t("session.retry")}
      </Button>
    </CenteredCard>
  );
}

function CenteredCard({ children }: { children: ReactNode }) {
  return (
    <div className="grid min-h-screen place-items-center p-6">
      <Card className="w-full max-w-md">
        <CardContent className="grid gap-4 pt-6">{children}</CardContent>
      </Card>
    </div>
  );
}
