import { useEffect, useState } from "react";
import { useI18n } from "@any-converter/core";
import { Button, Card, CardContent, Input } from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Field } from "../components/layout/Field";
import { Header } from "../components/layout/Header";
import { useAsyncState } from "../hooks/useAsyncState";
import { api } from "../lib/api";
import { management, useServerRequest, type AdminConfigView } from "../lib/management";

export function SettingsPage() {
  const { t } = useI18n();
  const { describeError } = useServerRequest();
  const [config, setConfig, loadError] = useAsyncState<AdminConfigView>(() =>
    management.getConfig().catch((cause) => {
      throw new Error(describeError(cause));
    }),
  );
  const [recentLogs] = useAsyncState(() => api.listRequestLogs(1), []);
  const [host, setHost] = useState("");
  const [port, setPort] = useState("");
  const [maxDiskMb, setMaxDiskMb] = useState("");
  const [requestLogging, setRequestLogging] = useState(false);
  const [inferenceKey, setInferenceKey] = useState("");
  const [initialized, setInitialized] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const configuration = config ?? undefined;

  useEffect(() => {
    if (!configuration || initialized) {
      return;
    }
    setHost(configuration.server.host);
    setPort(String(configuration.server.port));
    setMaxDiskMb(String(configuration.logging.max_disk_mb));
    setRequestLogging(configuration.logging.request_log_enabled);
    setInitialized(true);
  }, [configuration, initialized]);

  const lastClientId = recentLogs?.[0]?.client_id ?? t("desktop.settings.no_client");

  const save = async () => {
    try {
      let next = await management.patchConfig(configuration?.revision ?? "", {
        server: { host, port: Number(port) },
        logging: {
          max_disk_mb: Number(maxDiskMb),
          request_log_enabled: requestLogging,
        },
      });
      if (inferenceKey) {
        await management.putInferenceSecret(next.revision, inferenceKey);
        next = await management.getConfig();
      }
      setConfig(next);
      setInferenceKey("");
      setActionError(undefined);
    } catch (cause) {
      setActionError(describeError(cause));
    }
  };

  return (
    <section className="grid gap-6">
      <Header title={t("nav.settings")} subtitle={t("desktop.settings.subtitle")} />
      {(loadError || actionError) && <ErrorBanner message={loadError ?? actionError ?? ""} />}
      {config?.restart_required && (
        <p className="text-sm text-muted-foreground">{t("desktop.config.restartRequired")}</p>
      )}
      <Card>
        <CardContent className="grid gap-4 pt-6">
          <div className="flex w-full items-center gap-2 text-sm text-muted-foreground">
            <span>{t("desktop.settings.last_client")}</span>
            <code className="rounded bg-muted px-2 py-1 text-xs font-medium">{lastClientId}</code>
          </div>
          <div className="grid gap-4 md:grid-cols-3">
            <Field label={t("desktop.settings.host")} help="">
              <Input value={host} onChange={(event) => setHost(event.target.value)} />
            </Field>
            <Field label={t("desktop.settings.port")} help="">
              <Input value={port} onChange={(event) => setPort(event.target.value.replace(/\D/g, ""))} />
            </Field>
            <Field label={t("desktop.settings.maxDisk")} help="">
              <Input value={maxDiskMb} onChange={(event) => setMaxDiskMb(event.target.value.replace(/\D/g, ""))} />
            </Field>
          </div>
          <Field label={t("desktop.settings.requestLogging")} help="">
            <Button variant="outline" onClick={() => setRequestLogging((current) => !current)}>
              {requestLogging ? t("desktop.settings.enabled") : t("desktop.settings.disabled")}
            </Button>
          </Field>
          <Field
            label={t("desktop.settings.inferenceKey")}
            help={
              config?.server.inference_key_configured
                ? t("desktop.config.configured")
                : t("desktop.config.notConfigured")
            }
          >
            <Input type="password" value={inferenceKey} onChange={(event) => setInferenceKey(event.target.value)} />
          </Field>
          <Button className="w-fit" onClick={() => void save()} disabled={!host || !port || !maxDiskMb}>
            {t("desktop.settings.saveConfig")}
          </Button>
        </CardContent>
      </Card>
    </section>
  );
}
