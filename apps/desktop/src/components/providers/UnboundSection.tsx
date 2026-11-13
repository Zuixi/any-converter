import { useEffect, useState } from "react";

import { Button, Input } from "@any-converter/ui";

import { management, useServerRequest, type AccountEntry } from "../../lib/management";
import { ErrorBanner } from "../layout/ErrorBanner";
import { Field } from "../layout/Field";
import { Modal } from "./Modal";
import { useProvidersStrings } from "./strings";

/**
 * Zone 2: accounts stored but not bound to any provider. magpie has no
 * such state (its sign-ins are providers); ac keeps accounts standalone,
 * so they need a home between signing in and being routed — with a one
 * click attach that creates the provider the old page only told you to
 * go write into TOML.
 */
export function UnboundSection({
  unbound,
  onChanged,
}: {
  unbound: AccountEntry[];
  onChanged: () => Promise<void>;
}) {
  const s = useProvidersStrings();
  const [openId, setOpenId] = useState<string>();
  if (unbound.length === 0) {
    return null;
  }
  const open = unbound.find((account) => account.id === openId);
  return (
    <section className="grid gap-2" data-testid="unbound-section">
      <div className="flex items-baseline gap-2">
        <h2 className="text-sm font-semibold">{s.unboundTitle}</h2>
        <span className="text-xs text-muted-foreground">{s.unboundBody}</span>
      </div>
      <ul className="grid gap-2">
        {unbound.map((account) => (
          <li
            key={account.id}
            className="flex flex-wrap items-center gap-3 rounded-md border border-dashed px-3 py-2 text-sm"
          >
            <code className="min-w-0 flex-1 truncate" title={account.id}>
              {account.id}
            </code>
            <Button size="sm" variant="outline" onClick={() => setOpenId(account.id)}>
              {s.attach}
            </Button>
          </li>
        ))}
      </ul>
      {open && (
        <AttachDialog
          account={open}
          onClose={() => setOpenId(undefined)}
          onChanged={async () => {
            await onChanged();
            setOpenId(undefined);
          }}
        />
      )}
    </section>
  );
}

/** The attach flow: a name, the account's models, provider + routes. */
function AttachDialog({
  account,
  onClose,
  onChanged,
}: {
  account: AccountEntry;
  onClose: () => void;
  onChanged: () => Promise<void>;
}) {
  const s = useProvidersStrings();
  const { describeError } = useServerRequest();
  const [name, setName] = useState(account.provider);
  const [models, setModels] = useState<string[]>([]);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [createRoutes, setCreateRoutes] = useState(true);

  useEffect(() => {
    management
      .accountModels(account.id)
      .then((found) => setModels(found.models.map((model) => model.id)))
      .catch(() => setModels([]));
  }, [account.id]);

  const attach = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const current = await management.getConfig();
      const modelMap = Object.fromEntries(models.map((id) => [id, id]));
      const routes =
        createRoutes && current.model_routes.length === 0
          ? models.map((pattern) => ({
              pattern,
              providers: [name],
              upstream_model: null,
              strategy: "priority" as const,
            }))
          : [];
      await management.patchConfig(current.revision, {
        upsert_providers: [{ name, accounts: [account.id], model_map: modelMap }],
        upsert_model_routes: routes,
      });
      await onChanged();
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`${s.attach.replace(/…$/, "")} · ${account.id}`} onClose={onClose}>
      <div className="grid gap-4">
        <Field label={s.attachName} help={s.attachHint}>
          <Input value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <p className="text-sm text-muted-foreground">
          {models.length} {s.modelsAvailable}
        </p>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="h-4 w-4 accent-primary"
            checked={createRoutes}
            onChange={(event) => setCreateRoutes(event.target.checked)}
          />
          {s.createRoutes}
        </label>
        {error && <ErrorBanner message={error} />}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {s.cancel}
          </Button>
          <Button disabled={!name || busy} onClick={() => void attach()}>
            {s.addProvider}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
