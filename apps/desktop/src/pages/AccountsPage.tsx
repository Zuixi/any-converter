import { useCallback, useEffect, useMemo, useState } from "react";
import { useI18n } from "@any-converter/core";
import {
  Button,
  Card,
  CardContent,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@any-converter/ui";

import { ErrorBanner } from "../components/layout/ErrorBanner";
import { Field } from "../components/layout/Field";
import { Header } from "../components/layout/Header";
import { Table } from "../components/layout/Table";
import { api } from "../lib/api";

/** `GET /v1/accounts` entry (tokens are never part of the payload). */
interface AccountEntry {
  id: string;
  kind: string;
  provider: string;
  account: string;
}

/** `SignInTask` snapshot (`POST/GET /v1/accounts/signin*`). */
interface SignInTask {
  id: string;
  provider: string;
  state: string;
  url: string;
  code: string;
  instructions: string;
  paste_callback: boolean;
  paste_code: boolean;
  user: string;
  plan: string;
  error: string;
}

/** One allowance window of `GET /v1/accounts/quotas`. */
interface QuotaWindow {
  name: string;
  used: number;
  resets?: string;
}

interface QuotaEntry {
  provider: string;
  account: string;
  windows: QuotaWindow[];
}

const STRINGS = {
  en: {
    title: "Accounts",
    subtitle: "OAuth sign-ins stored locally by the server. Tokens never leave the account store.",
    add: "Add account",
    provider: "Provider",
    providerHelp: "gemini signs in through the browser; copilot uses a device code.",
    start: "Start sign-in",
    openUrl: "Open URL",
    copy: "Copy",
    copied: "Copied",
    code: "Code",
    paste: "Paste callback URL",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "Finish sign-in",
    cancel: "Cancel",
    state: "State",
    stateWaiting: "waiting…",
    error: "Error",
    empty: "No accounts stored yet. Sign in with gemini or copilot to add one.",
    providerCol: "Provider",
    accountCol: "Account",
    kindCol: "Kind",
    idCol: "ID",
    quotaCol: "Quota windows",
    quotaNone: "—",
    remove: "Remove",
    removed: "Removed",
    unreachable: "The server is not reachable (is it running?).",
  },
  "zh-CN": {
    title: "账户",
    subtitle: "服务端本地保存的 OAuth 登录。令牌永不离开账户存储。",
    add: "添加账户",
    provider: "服务商",
    providerHelp: "gemini 通过浏览器登录；copilot 使用设备码。",
    start: "开始登录",
    openUrl: "打开链接",
    copy: "复制",
    copied: "已复制",
    code: "设备码",
    paste: "粘贴回调地址",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "完成登录",
    cancel: "取消",
    state: "状态",
    stateWaiting: "等待中…",
    error: "错误",
    empty: "暂无账户。使用 gemini 或 copilot 登录添加。",
    providerCol: "服务商",
    accountCol: "账户",
    kindCol: "类型",
    idCol: "ID",
    quotaCol: "配额窗口",
    quotaNone: "—",
    remove: "移除",
    removed: "已移除",
    unreachable: "服务器不可达（是否已启动？）。",
  },
} as const;

export function AccountsPage() {
  const { language } = useI18n();
  const s = STRINGS[language] ?? STRINGS.en;

  const [accounts, setAccounts] = useState<AccountEntry[]>([]);
  const [quotas, setQuotas] = useState<Record<string, QuotaEntry>>({});
  const [task, setTask] = useState<SignInTask | null>(null);
  const [provider, setProvider] = useState("gemini");
  const [pastedUrl, setPastedUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>();
  const [copied, setCopied] = useState(false);

  // The server's address and client key come from the desktop settings.
  const [server, setServer] = useState<{ base: string; apiKey?: string } | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .getSettings()
      .then((settings) => {
        if (cancelled) return;
        const host = settings["server.host"] || "127.0.0.1";
        const port = settings["server.port"] || "8080";
        // A wildcard bind cannot be dialed back; loopback always can.
        const reachable = host === "0.0.0.0" || host === "::" ? "127.0.0.1" : host;
        const apiKey = settings["server.api_key"] || undefined;
        setServer({ base: `http://${reachable}:${port}`, apiKey });
      })
      .catch(() => {
        if (!cancelled) setError(s.unreachable);
      });
    return () => {
      cancelled = true;
    };
  }, [s]);

  const request = useCallback(
    async <T,>(path: string, init?: RequestInit): Promise<T> => {
      if (!server) throw new Error(s.unreachable);
      const headers: Record<string, string> = { "Content-Type": "application/json" };
      if (server.apiKey) headers.Authorization = `Bearer ${server.apiKey}`;
      const response = await fetch(`${server.base}${path}`, { ...init, headers });
      if (!response.ok) {
        let message = `${response.status} ${response.statusText}`;
        try {
          const data = (await response.json()) as { error?: { message?: string } };
          message = data.error?.message ?? message;
        } catch {
          // non-JSON error body: keep the status text
        }
        throw new Error(message);
      }
      return (await response.json()) as T;
    },
    [server, s],
  );

  const refresh = useCallback(async () => {
    try {
      const [data, quotaData] = await Promise.all([
        request<{ accounts: AccountEntry[] }>("/v1/accounts"),
        request<{ accounts: QuotaEntry[] }>("/v1/accounts/quotas"),
      ]);
      setAccounts(data.accounts ?? []);
      const byAccount: Record<string, QuotaEntry> = {};
      for (const entry of quotaData.accounts ?? []) {
        byAccount[entry.account] = entry;
      }
      setQuotas(byAccount);
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  }, [request]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Poll an in-flight sign-in every second until it settles.
  useEffect(() => {
    if (!task || task.state !== "waiting") return;
    const timer = window.setInterval(() => {
      request<SignInTask>(`/v1/accounts/signin/${task.id}`)
        .then((latest) => {
          setTask(latest);
          if (latest.state === "done") void refresh();
        })
        .catch(() => {
          /* transient poll errors keep the task waiting */
        });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [task, request, refresh]);

  const start = async () => {
    setBusy(true);
    setError(undefined);
    setPastedUrl("");
    try {
      const started = await request<SignInTask>("/v1/accounts/signin", {
        method: "POST",
        body: JSON.stringify({ provider }),
      });
      setTask(started);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    if (!task) return;
    setBusy(true);
    setError(undefined);
    try {
      const finished = await request<SignInTask>(`/v1/accounts/signin/${task.id}/submit`, {
        method: "POST",
        body: JSON.stringify({ input: pastedUrl }),
      });
      setTask(finished);
      if (finished.state === "done") void refresh();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (!task) return;
    try {
      await fetch(`${server?.base}/v1/accounts/signin/${task.id}/cancel`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(server?.apiKey ? { Authorization: `Bearer ${server.apiKey}` } : {}),
        },
      });
    } catch {
      // best effort
    }
    setTask(null);
  };

  const remove = async (id: string) => {
    setError(undefined);
    try {
      await request<{ outcome: string }>(`/v1/accounts/${encodeURIComponent(id)}/remove`, {
        method: "POST",
        body: "{}",
      });
      await refresh();
    } catch (cause) {
      setError(String(cause));
    }
  };

  const copyUrl = async () => {
    if (!task?.url) return;
    try {
      await navigator.clipboard.writeText(task.url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard unavailable in some webviews
    }
  };

  const rows = useMemo(
    () =>
      accounts.map((account) => {
        const windows = quotas[account.id]?.windows ?? [];
        return [
          account.provider,
          account.account || "—",
          account.kind,
          account.id,
          windows.length === 0
            ? s.quotaNone
            : windows
                .map((window) => `${window.name} ${window.used}%`)
                .join(", "),
          <Button
            key={account.id}
            size="sm"
            variant="destructive"
            onClick={() => void remove(account.id)}
          >
            {s.remove}
          </Button>,
        ];
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [accounts, quotas, s],
  );

  return (
    <section className="grid gap-6">
      <Header title={s.title} subtitle={s.subtitle} />
      {error && <ErrorBanner message={error} />}

      <Card>
        <CardContent className="grid gap-4 pt-6">
          <div className="grid gap-4 md:grid-cols-2">
            <Field label={s.provider} help={s.providerHelp}>
              <Select value={provider} onValueChange={setProvider}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="gemini">gemini</SelectItem>
                  <SelectItem value="copilot">copilot</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
          <Button className="w-fit" onClick={() => void start()} disabled={busy || !server}>
            {s.start}
          </Button>

          {task && task.state === "waiting" && (
            <Card className="border-dashed">
              <CardContent className="grid gap-3 pt-6">
                <div className="text-sm text-muted-foreground">{task.instructions}</div>
                {task.url && (
                  <div className="flex items-center gap-2">
                    <a
                      className="truncate text-sm underline"
                      href={task.url}
                      target="_blank"
                      rel="noreferrer"
                    >
                      {s.openUrl}
                    </a>
                    <Button size="sm" variant="outline" onClick={() => void copyUrl()}>
                      {copied ? s.copied : s.copy}
                    </Button>
                  </div>
                )}
                {task.code && (
                  <div className="flex items-center gap-2">
                    <span className="text-xs uppercase text-muted-foreground">{s.code}:</span>
                    <code className="rounded bg-muted px-2 py-1 text-lg font-semibold tracking-wider">
                      {task.code}
                    </code>
                  </div>
                )}
                {task.paste_callback && (
                  <div className="flex items-end gap-2">
                    <div className="flex-1">
                      <Field label={s.paste} help="">
                        <Input
                          value={pastedUrl}
                          placeholder={s.pastePlaceholder}
                          onChange={(event) => setPastedUrl(event.target.value)}
                        />
                      </Field>
                    </div>
                    <Button
                      onClick={() => void submit()}
                      disabled={busy || !pastedUrl.trim()}
                    >
                      {s.submit}
                    </Button>
                  </div>
                )}
                <div className="flex items-center gap-2">
                  <span className="text-sm text-muted-foreground">{s.state}</span>
                  <span className="text-sm">{s.stateWaiting}</span>
                  <Button size="sm" variant="ghost" onClick={() => void cancel()}>
                    {s.cancel}
                  </Button>
                </div>
              </CardContent>
            </Card>
          )}

          {task && task.state === "failed" && (
            <ErrorBanner message={`${s.error}: ${task.error || "unknown"}`} />
          )}
          {task && task.state === "done" && (
            <p className="text-sm">
              {task.provider}: {task.user || "signed in"}
            </p>
          )}
        </CardContent>
      </Card>

      <Table
        emptyText={s.empty}
        headers={[s.providerCol, s.accountCol, s.kindCol, s.idCol, s.quotaCol, ""]}
        rows={rows}
      />
    </section>
  );
}
