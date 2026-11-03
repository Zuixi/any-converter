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

/** `GET /v1/accounts/plugins`. */
interface PluginEntry {
  spec: string;
  providers: { id: string; name: string; methods: { type: string; label: string }[] }[];
  error: string;
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
    providerHelp:
      "gemini and antigravity use the browser; antigravity may cost you the account. " +
      "codex signs in as Codex CLI and writes back to its auth.json. " +
      "chatgpt-api ends by pasting the address the browser landed on. " +
      "copilot shows a device code. claude needs the `claude` command line.",
    start: "Start sign-in",
    openUrl: "Open URL",
    copy: "Copy",
    copied: "Copied",
    code: "Code",
    paste: "Paste callback URL",
    pasteCodeLabel: "Paste the code or the address",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "Finish sign-in",
    cancel: "Cancel",
    state: "State",
    stateWaiting: "waiting…",
    error: "Error",
    empty: "No accounts stored yet. Pick a provider above and sign in to add one.",
    providerCol: "Provider",
    accountCol: "Account",
    kindCol: "Kind",
    idCol: "ID",
    quotaCol: "Quota windows",
    quotaNone: "—",
    remove: "Remove",
    removed: "Removed",
    antigravityRisk:
      "Google may suspend an Antigravity account it sees used outside Antigravity. " +
      "Use one you can afford to lose.",
    siwcHint:
      "OpenAI registers this app as you sign in, so the address the browser ends on " +
      "carries the code and the client id. Paste the whole address.",
    pasteCode: "Paste the code Claude's page shows, or the whole address it ended on",
    unreachable: "The server is not reachable (is it running?).",
    plugins: "Plugins",
    pluginsNone: "No plugins installed. `any-converter plugin install <spec>` adds one.",
    pluginFailed: "won't load",
    order: "Order",
    orderHint: "Which account routing tries first. Unnamed accounts follow, in config order.",
    moveUp: "Move up",
    adopt: "Adopt",
    release: "Release",
    adopted: "Written back where Codex reads it",
    released: "No longer written back",
    quotaMore: "Windows",
    quotaEmpty: "No allowance read yet",
    signInFailed: "Sign-in failed",
    hintKeyring:
      "Codex keeps this sign-in in the system keychain. Set cli_auth_credentials_store = \"file\" in its config.toml, or sign in with Codex itself and use Discover instead.",
    hintClientId:
      "Paste the whole address the browser ended on — it carries the code and the client id OpenAI issued.",
    hintInstallClaude: "Install Claude Code first: the `claude` command line (the desktop app is not it).",
    hintEligible:
      "OpenAI did not grant this account the use of its API; a ChatGPT plan that includes the API is needed.",
    hintRevoked: "This sign-in was turned away for good. Sign in again to restore the account.",
    orderSaved: "Order saved",
  },
  "zh-CN": {
    title: "账户",
    subtitle: "服务端本地保存的 OAuth 登录。令牌永不离开账户存储。",
    add: "添加账户",
    provider: "服务商",
    providerHelp:
      "gemini 与 antigravity 走浏览器；antigravity 可能导致账号被封。codex 以 Codex CLI 身份登录并写回它的 auth.json。" +
      "chatgpt-api 最后要粘贴浏览器落地的整条地址。copilot 显示设备码。claude 需要本机装有 `claude` 命令。",
    start: "开始登录",
    openUrl: "打开链接",
    copy: "复制",
    copied: "已复制",
    code: "设备码",
    paste: "粘贴回调地址",
    pasteCodeLabel: "粘贴 code 或整条地址",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "完成登录",
    cancel: "取消",
    state: "状态",
    stateWaiting: "等待中…",
    error: "错误",
    empty: "暂无账户。在上方选择服务商并登录即可添加。",
    providerCol: "服务商",
    accountCol: "账户",
    kindCol: "类型",
    idCol: "ID",
    quotaCol: "配额窗口",
    quotaNone: "—",
    remove: "移除",
    removed: "已移除",
    antigravityRisk:
      "Google 可能停用它在 Antigravity 之外看到被使用的账号。请用一个你承受得起损失的账号。",
    siwcHint:
      "登录过程中 OpenAI 会现场注册本应用，所以浏览器最终落到的地址里同时带着 code 与 client id。请粘贴整条地址。",
    pasteCode: "粘贴 Claude 页面显示的 code，或浏览器最终落到的整条地址",
    unreachable: "服务器不可达（是否已启动？）。",
    plugins: "插件",
    pluginsNone: "尚未安装插件。`any-converter plugin install <spec>` 可安装一个。",
    pluginFailed: "无法加载",
    order: "顺序",
    orderHint: "路由优先尝试哪个账户。未列出的账户按配置顺序排在后面。",
    moveUp: "上移",
    adopt: "接管写回",
    release: "取消写回",
    adopted: "已写回 Codex 读取的位置",
    released: "已停止写回",
    quotaMore: "窗口",
    quotaEmpty: "尚未读取额度",
    signInFailed: "登录失败",
    hintKeyring:
      "Codex 把这次登录放在系统钥匙串里。可在它的 config.toml 里设置 cli_auth_credentials_store = \"file\"，或先用 Codex 本身登录再使用 Discover。",
    hintClientId: "请粘贴浏览器最终落到的整条地址——它同时带着 code 与 OpenAI 签发的 client id。",
    hintInstallClaude: "请先安装 Claude Code：`claude` 命令行（桌面应用不是它）。",
    hintEligible: "OpenAI 未授予该账户使用其 API 的权限；需要一个包含 API 的 ChatGPT 套餐。",
    hintRevoked: "这次登录已被永久拒绝。重新登录即可恢复该账户。",
    orderSaved: "顺序已保存",
  },
} as const;

/**
 * What to do about a sign-in failure, from what the server said. The server
 * messages are specific ("OpenAI didn't finish registering …", "no readable
 * key in the system keychain"); a hint turns each into the next step rather
 * than leaving the raw text alone.
 */
function hintFor(message: string, s: { hintKeyring: string; hintClientId: string;
  hintInstallClaude: string; hintEligible: string; hintRevoked: string }): string | undefined {
  const text = message.toLowerCase();
  if (text.includes("keyring") || text.includes("keychain") || text.includes("cli_auth_credentials_store")) {
    return s.hintKeyring;
  }
  if (text.includes("registering") || text.includes("client id")) {
    return s.hintClientId;
  }
  if (text.includes("claude") && (text.includes("install") || text.includes("not found"))) {
    return s.hintInstallClaude;
  }
  if (text.includes("eligible")) {
    return s.hintEligible;
  }
  if (text.includes("sign in again") || text.includes("expired") || text.includes("revoked")) {
    return s.hintRevoked;
  }
  return undefined;
}

export function AccountsPage() {
  const { language } = useI18n();
  const s = STRINGS[language] ?? STRINGS.en;

  const [accounts, setAccounts] = useState<AccountEntry[]>([]);
  const [quotas, setQuotas] = useState<Record<string, QuotaEntry>>({});
  const [plugins, setPlugins] = useState<PluginEntry[]>([]);
  /// Per provider, the order the user saved (fully-qualified account ids).
  const [order, setOrder] = useState<Record<string, string[]>>({});
  const [notice, setNotice] = useState<string | undefined>();
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
      const [data, quotaData, pluginData] = await Promise.all([
        request<{ accounts: AccountEntry[] }>("/v1/accounts"),
        request<{ accounts: QuotaEntry[] }>("/v1/accounts/quotas"),
        request<{ plugins: PluginEntry[] }>("/v1/accounts/plugins").catch(
          () => ({ plugins: [] as PluginEntry[] }),
        ),
      ]);
      setPlugins(pluginData.plugins ?? []);
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

  /// Save a provider's account order (the first seat routing tries).
  const saveOrder = async (provider: string, ids: string[]) => {
    setError(undefined);
    try {
      await request(`/v1/accounts/order`, {
        method: "POST",
        body: JSON.stringify({ provider, accounts: ids }),
      });
      setOrder((prev) => ({ ...prev, [provider]: ids }));
      setNotice(s.orderSaved);
    } catch (cause) {
      setError(String(cause));
    }
  };

  /// Move one account up among its provider's seats.
  const moveUp = (provider: string, accountId: string, siblings: string[]) => {
    const current = order[provider] ?? siblings;
    const at = current.indexOf(accountId);
    if (at <= 0) return;
    const next = [...current];
    const above = next[at - 1];
    const here = next[at];
    if (above === undefined || here === undefined) return;
    next[at - 1] = here;
    next[at] = above;
    void saveOrder(provider, next);
  };

  const adopt = async (id: string) => {
    setError(undefined);
    try {
      await request(`/v1/accounts/${encodeURIComponent(id)}/adopt`, { method: "POST" });
      setNotice(s.adopted);
      void refresh();
    } catch (cause) {
      setError(String(cause));
    }
  };

  const release = async (id: string) => {
    setError(undefined);
    try {
      await request(`/v1/accounts/${encodeURIComponent(id)}/release`, { method: "POST" });
      setNotice(s.released);
    } catch (cause) {
      setError(String(cause));
    }
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

  /// The accounts that share a provider prefix, in the order routing uses.
  const siblingsOf = (account: AccountEntry): string[] => {
    const saved = order[account.provider];
    if (saved && saved.length > 0) return saved;
    return accounts
      .filter((other) => other.provider === account.provider)
      .map((other) => other.id);
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
          <div key={account.id} className="flex flex-wrap items-center gap-1">
            {account.provider !== "" && siblingsOf(account).length > 1 && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => moveUp(account.provider, account.id, siblingsOf(account))}
              >
                ↑ {s.moveUp}
              </Button>
            )}
            {account.provider === "codex" && (
              <>
                <Button size="sm" variant="outline" onClick={() => void adopt(account.id)}>
                  {s.adopt}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => void release(account.id)}>
                  {s.release}
                </Button>
              </>
            )}
            <Button size="sm" variant="destructive" onClick={() => void remove(account.id)}>
              {s.remove}
            </Button>
          </div>,
        ];
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [accounts, quotas, order, s],
  );

  return (
    <section className="grid gap-6">
      <Header title={s.title} subtitle={s.subtitle} />
      {error && <ErrorBanner message={error} />}
      {notice && (
        <p className="rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm">
          {notice}
        </p>
      )}

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
                  <SelectItem value="antigravity">antigravity</SelectItem>
                  <SelectItem value="codex">codex</SelectItem>
                  <SelectItem value="chatgpt-api">chatgpt-api</SelectItem>
                  <SelectItem value="copilot">copilot</SelectItem>
                  <SelectItem value="claude">claude</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
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
                {(task.paste_callback || task.paste_code) && (
                  <div className="flex items-end gap-2">
                    <div className="flex-1">
                      <Field
                        label={task.paste_code ? s.pasteCodeLabel : s.paste}
                        help=""
                      >
                        <Input
                          value={pastedUrl}
                          placeholder={
                            task.paste_code ? s.pasteCode : s.pastePlaceholder
                          }
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
            <div className="grid gap-2">
              <ErrorBanner message={`${s.error}: ${task.error || "unknown"}`} />
              {hintFor(task.error || "", s) && (
                <p className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm">
                  {hintFor(task.error || "", s)}
                </p>
              )}
            </div>
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

      <Card>
        <CardContent className="grid gap-3 pt-6">
          <div className="text-sm font-medium">{s.plugins}</div>
          {plugins.length === 0 ? (
            <p className="text-sm text-muted-foreground">{s.pluginsNone}</p>
          ) : (
            <ul className="grid gap-2 text-sm">
              {plugins.map((plugin) => (
                <li key={plugin.spec} className="grid gap-1">
                  <code className="text-xs">{plugin.spec}</code>
                  {plugin.error ? (
                    <span className="text-amber-600">
                      {s.pluginFailed}: {plugin.error}
                    </span>
                  ) : (
                    plugin.providers.map((served) => (
                      <span key={served.id} className="text-muted-foreground">
                        {served.name || served.id} ·{" "}
                        {served.methods
                          .map((method) =>
                            method.label ? `${method.type}(${method.label})` : method.type,
                          )
                          .join(", ")}
                      </span>
                    ))
                  )}
                </li>
              ))}
            </ul>
          )}
          <p className="text-xs text-muted-foreground">{s.orderHint}</p>
        </CardContent>
      </Card>
    </section>
  );
}
