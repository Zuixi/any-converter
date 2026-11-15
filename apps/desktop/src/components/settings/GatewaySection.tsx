import { useEffect, useState } from "react";
import { useApiClient, useI18n } from "@any-converter/core";
import { Button, Input } from "@any-converter/ui";

import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { useAsyncState } from "../../hooks/useAsyncState";
import { management, useServerRequest, type AdminConfigView } from "../../lib/management";
import { isDesktopApp } from "../../lib/platform";

/**
 * Gateway section of the settings dialog: the config editor the retired
 * Settings page hosted — listen host/port, log disk cap, request logging,
 * and the inference key. The browser cannot restart the gateway, so
 * host/port editing stays desktop-only.
 */
export function GatewaySection() {
  const { t } = useI18n();
  const { describeError } = useServerRequest();
  const apiClient = useApiClient();
  const hostManaged = isDesktopApp();
  const [config, setConfig, loadError] = useAsyncState<AdminConfigView>(() =>
    management.getConfig().catch((cause) => {
      throw new Error(describeError(cause));
    }),
  );
  const [recentLogs] = useAsyncState(() => apiClient.getLogs(1), []);
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
      // Fetch the current revision instead of trusting the cached view; a
      // stale or empty If-Match turns a normal save into a 412/428.
      const current = await management.getConfig();
      let next = await management.patchConfig(current.revision, {
        ...(hostManaged ? { server: { host, port: Number(port) } } : {}),
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
    <div className="grid gap-4">
      {(loadError || actionError) && <ErrorBanner message={loadError ?? actionError ?? ""} />}
      {config?.restart_required && (
        <p className="text-sm text-muted-foreground">{t("desktop.config.restartRequired")}</p>
      )}
      <div className="flex w-full items-center gap-2 text-sm text-muted-foreground">
        <span>{t("desktop.settings.last_client")}</span>
        <code className="rounded bg-muted px-2 py-1 text-xs font-medium">{lastClientId}</code>
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        {hostManaged && (
          <>
            <Field label={t("desktop.settings.host")} help="">
              <Input value={host} onChange={(event) => setHost(event.target.value)} />
            </Field>
            <Field label={t("desktop.settings.port")} help="">
              <Input value={port} onChange={(event) => setPort(event.target.value.replace(/\D/g, ""))} />
            </Field>
          </>
        )}
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
      <Button
        className="w-fit"
        onClick={() => void save()}
        disabled={!maxDiskMb || (hostManaged && (!host || !port))}
      >
        {t("desktop.settings.saveConfig")}
      </Button>
    </div>
  );
}
