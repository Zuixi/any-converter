import { useI18n } from "@any-converter/core";

import type { AccountEntry, SignInTask } from "../../lib/management";

/**
 * The merged providers page's bilingual strings. Colocated like the old
 * accounts page's dict was: the page family is one feature, and its
 * strings move with it.
 */
export const STRINGS = {
  en: {
    // page
    subtitle:
      "Providers you subscribe to or hold keys for, with the accounts and models each serves.",    summaryProviders: "providers",
    summaryExposed: "models exposed",
    summaryRouted: "routed",
    unroutedWarning: "exposed but not routed",
    filter: "Filter providers…",
    empty: "No providers yet — subscribe or add a key below.",
    restartRequired: "Changes saved. Restart the server to apply them.",
    // card
    keyConfigured: "API key",
    keyMissing: "no key",
    oauthBadge: "OAuth",
    pluginBadge: "plugin",
    allSignedOut: "signed out",
    modelsCount: "models",
    routedCount: "routed",
    cardUnrouted: "unrouted",
    signedInAs: "signed in as",
    probe: "probe",
    delete: "Delete",
    deleteConfirm: "Remove this provider? Routes naming it stop matching.",
    // detail dialog
    detail: "Provider",
    website: "Website",
    close: "Close",
    sectionAccess: "Access",
    sectionBasic: "Basics",
    sectionModels: "Models",
    sectionRoutes: "Routes",
    addAccount: "Sign in another account",
    accessSwitchToKey: "Switch to an API key",
    accessSwitchToOAuth: "Switch to accounts",
    accessSwitchNote:
      "Setting accounts makes the key unused; removing them makes the key serve again.",
    format: "API format",
    authScheme: "Auth scheme",
    baseUrl: "Base URL",
    refreshModels: "Refresh",
    probeAgain: "Probe",
    probing: "Probing…",
    saveModels: "Save models",
    manualModels: "Model ids",
    manualModelsHelp: "Comma or newline separated; added to the selection.",
    noRoutes: "No route names this provider — its models answer nothing until one does.",
    gotoRoutes: "Manage in Routes",
    routePatterns: "patterns",
    // add sheet
    addTitle: "Add a provider",
    addSearch: "Search vendors, or paste a base URL…",
    addCustom: "Custom",
    addCustomHint: "Any OpenAI / Anthropic / Gemini compatible URL",
    sectionSubs: "Subscriptions",
    sectionSubsHint: "sign in, no key",
    sectionPlugins: "From plugins",
    sectionPluginsHint: "installed for you when picked",
    sectionVendors: "Vendor APIs",
    sectionVendorsHint: "fill in an API key",
    sectionRelays: "Relays",
    sectionRelaysHint: "one key, many vendors",
    pluginSpecPlaceholder: "npm spec, e.g. @scope/plugin-name",
    installPlugin: "Install",
    installing: "Installing…",
    installFailed: "Install failed",
    signIn: "Sign in",
    name: "Name",
    nameHelp: "A short unique name used by routes.",
    namePlaceholder: "e.g. my-relay",
    apiKey: "API key",
    apiKeyHelp: "Stored in the OS keychain; never written to config.",
    showKey: "Show",
    hideKey: "Hide",
    getAKey: "Get a key",
    formatHelp: "Each protocol keeps its own base URL below.",
    urlHintResponses: "OpenAI Responses API root, e.g. https://api.openai.com",
    urlHintChat: "Chat Completions compatible root; usually ends with /v1",
    urlHintClaude: "Claude API root, e.g. https://api.anthropic.com",
    urlHintGemini: "Gemini API root, e.g. https://generativelanguage.googleapis.com",
    probeFirstHint: "Probe the address to list its models, then pick what to expose.",
    advanced: "Advanced",
    authSchemeHelp:
      "How the key is sent. Most providers follow their API format; change it only for a gateway whose auth header differs.",
    adding: "Adding…",
    test: "Probe",
    testing: "Probing…",
    discoveryOk: "OK",
    discoveryFailed: "Probe failed",
    addProvider: "Add",
    createRoutes: "Also create default routes for the selected models",
    defaultRoutesExist: "Routes already exist; none will be added",
    modelsHint: "Pick the models to expose.",
    selectModelsFirst: "Select at least one model first.",
    searchModels: "Search models…",
    selectAll: "Select all",
    clearSelection: "Clear",
    noModelsFound: "No models found.",
    noMatches: "No matches.",
    selectedCount: "selected",
    savedNotice: "Saved",
    signedInFlowDone: "Account saved",
    providerCreated: "Provider added",
    // unbound
    unboundTitle: "Signed in, not attached to a provider",
    unboundBody: "These accounts are stored but no provider routes to them yet.",
    attach: "Attach as provider…",
    attachName: "Provider name",
    attachHint: "Creates a provider bound to this account, plus routes for its models.",
    attachDone: "Attached",
    // sign-in flow (from the accounts page)
    start: "Start sign-in",
    stepOpen: "Open the login page",
    reopenUrl: "Reopen in browser",
    autoOpenFailed:
      "The browser did not open automatically — click the link above or copy the address.",
    waitingForUrl: "Preparing the login page…",
    stepAuthorize: "Authorize",
    pasteSectionHint:
      "Gateway on a server or in a container? The browser cannot call back to it — paste the whole address the browser ended on and finish here.",
    waitingForCallback:
      "On the same machine? Nothing more to do — this window continues once you authorize.",
    retry: "Retry",
    openUrl: "Open URL",
    copy: "Copy",
    copied: "Copied",
    code: "Code",
    pasteCallbackLabel: "Paste callback URL",
    pasteCodeLabel: "Paste the code or the address",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "Finish sign-in",
    cancel: "Cancel",
    state: "State",
    stateWaiting: "waiting…",
    stateInstalling: "installing…",
    error: "Error",
    modelsAvailable: "models available",
    modelsLoading: "Reading models…",
    modelsUnsupported: "Discovery not supported",
    modelsEmpty: "No models reported",
    modelsError: "Model discovery failed",
    modelsRetry: "Retry",
    modelsRefreshing: "Refreshing the account list and its models…",
    routeConfigHint: "Signing in stores the credential; the provider below routes requests to it.",
    // account rows
    quotaNone: "—",
    quotaResets: "resets",
    removeAccount: "Remove",
    moveUp: "Move up",
    adopt: "Adopt",
    release: "Release",
    adopted: "Written back where Codex reads it",
    released: "No longer written back",
    pluginManaged: "Managed by plugin",
    orderHint: "The first account is tried first; Move up reorders.",
    antigravityRisk:
      "Google may suspend an Antigravity account it sees used outside Antigravity. Use one you can afford to lose.",
    siwcHint:
      "OpenAI registers this app as you sign in, so the address the browser ends on carries the code and the client id. Paste the whole address.",
    // sign-in failure hints (from the accounts page)
    hintKeyring:
      'Codex keeps this sign-in in the system keychain. Set cli_auth_credentials_store = "file" in its config.toml, or sign in with Codex itself and use Discover instead.',
    hintClientId:
      "Paste the whole address the browser ended on — it carries the code and the client id OpenAI issued.",
    hintInstallClaude:
      "Install Claude Code first: the `claude` command line (the desktop app is not it).",
    hintEligible:
      "OpenAI did not grant this account the use of its API; a ChatGPT plan that includes the API is needed.",
    hintRevoked: "This sign-in was turned away for good. Sign in again to restore the account.",
  },
  "zh-CN": {
    // page
    subtitle: "已订阅或持有 Key 的供应商，以及各自的账号与模型。",
    summaryProviders: "个供应商",
    summaryExposed: "个模型已暴露",
    summaryRouted: "已路由",
    unroutedWarning: "已暴露未路由",
    filter: "筛选供应商…",
    empty: "还没有供应商 —— 在下方登录订阅或填入 Key。",
    restartRequired: "已保存，重启服务端后生效。",
    // card
    keyConfigured: "API Key",
    keyMissing: "未配置 Key",
    oauthBadge: "OAuth",
    pluginBadge: "插件",
    allSignedOut: "已登出",
    modelsCount: "个模型",
    routedCount: "已路由",
    cardUnrouted: "未路由",
    signedInAs: "已登录",
    probe: "探测",
    delete: "删除",
    deleteConfirm: "移除此供应商？引用它的路由将不再匹配。",
    // detail dialog
    detail: "供应商",
    website: "官网",
    close: "关闭",
    sectionAccess: "接入方式",
    sectionBasic: "基本信息",
    sectionModels: "模型",
    sectionRoutes: "路由",
    addAccount: "登录另一个账号",
    accessSwitchToKey: "改为 API Key",
    accessSwitchToOAuth: "改为账号登录",
    accessSwitchNote: "设置账号后 Key 不再使用；移除账号后 Key 重新生效。",
    format: "API 协议",
    authScheme: "认证方式",
    baseUrl: "Base URL",
    refreshModels: "刷新",
    probeAgain: "探测",
    probing: "探测中…",
    saveModels: "保存模型",
    manualModels: "模型 ID",
    manualModelsHelp: "逗号或换行分隔，直接加入选择。",
    noRoutes: "尚无路由引用此供应商 —— 没有路由，它的模型不会被请求到。",
    gotoRoutes: "去 Routes 管理",
    routePatterns: "条规则",
    // add sheet
    addTitle: "接入供应商",
    addSearch: "搜索厂商，或粘贴 base_url…",
    addCustom: "自定义",
    addCustomHint: "任何 OpenAI / Anthropic / Gemini 兼容地址",
    sectionSubs: "订阅账号",
    sectionSubsHint: "登录即用，无需 Key",
    sectionPlugins: "插件订阅",
    sectionPluginsHint: "选中即自动安装",
    sectionVendors: "厂商 API",
    sectionVendorsHint: "填入 API Key",
    sectionRelays: "中转",
    sectionRelaysHint: "一个 Key 通多家厂商",
    pluginSpecPlaceholder: "npm 包名，如 @scope/plugin-name",
    installPlugin: "安装",
    installing: "安装中…",
    installFailed: "安装失败",
    signIn: "登录",
    name: "名称",
    nameHelp: "路由引用的短名，如 openai。",
    namePlaceholder: "如 my-relay",
    apiKey: "API Key",
    apiKeyHelp: "存入系统钥匙串，不写入配置文件。",
    showKey: "显示",
    hideKey: "隐藏",
    getAKey: "获取 Key",
    formatHelp: "每个协议各自记住下面的 base URL。",
    urlHintResponses: "OpenAI Responses API 根地址，如 https://api.openai.com",
    urlHintChat: "Chat Completions 兼容根地址，通常以 /v1 结尾",
    urlHintClaude: "Claude API 根地址，如 https://api.anthropic.com",
    urlHintGemini: "Gemini API 根地址，如 https://generativelanguage.googleapis.com",
    probeFirstHint: "先探测地址列出模型，再勾选要暴露的。",
    advanced: "高级",
    authSchemeHelp: "Key 的发送方式。多数供应商跟随其 API 协议；仅当网关的认证头不同时才需要改。",
    adding: "添加中…",
    test: "探测",
    testing: "探测中…",
    discoveryOk: "可用",
    discoveryFailed: "探测失败",
    addProvider: "添加",
    createRoutes: "同时为所选模型创建默认路由",
    defaultRoutesExist: "已有路由，本次不再添加",
    modelsHint: "选择要暴露的模型。",
    selectModelsFirst: "请先选择至少一个模型。",
    searchModels: "搜索模型…",
    selectAll: "全选",
    clearSelection: "清除",
    noModelsFound: "未找到模型。",
    noMatches: "无匹配。",
    selectedCount: "已选",
    savedNotice: "已保存",
    signedInFlowDone: "账号已保存",
    providerCreated: "供应商已添加",
    // unbound
    unboundTitle: "已登录，未接入供应商",
    unboundBody: "这些账号已保存在本地，但还没有供应商把请求路由过去。",
    attach: "接入为供应商…",
    attachName: "供应商名称",
    attachHint: "创建绑定此账号的供应商，并为其模型建立路由。",
    attachDone: "已接入",
    // sign-in flow
    start: "开始登录",
    stepOpen: "打开登录页",
    reopenUrl: "在浏览器中重新打开",
    autoOpenFailed: "浏览器未能自动打开，请点击上方链接或复制地址手动打开。",
    waitingForUrl: "正在准备登录页…",
    stepAuthorize: "完成授权",
    pasteSectionHint:
      "网关跑在服务器或容器里时，浏览器回调不了本机——把浏览器最终跳到的整条地址粘贴到下面，点完成登录。",
    waitingForCallback: "在同一台机器上？无需操作——授权完成后本窗口会自动继续。",
    retry: "重试",
    openUrl: "打开链接",
    copy: "复制",
    copied: "已复制",
    code: "设备码",
    pasteCallbackLabel: "粘贴回调地址",
    pasteCodeLabel: "粘贴 code 或整条地址",
    pastePlaceholder: "http://127.0.0.1:.../oauth2callback?state=...&code=...",
    submit: "完成登录",
    cancel: "取消",
    state: "状态",
    stateWaiting: "等待中…",
    stateInstalling: "安装中…",
    error: "错误",
    modelsAvailable: "个可用模型",
    modelsLoading: "正在读取模型…",
    modelsUnsupported: "不支持模型发现",
    modelsEmpty: "未返回模型",
    modelsError: "模型发现失败",
    modelsRetry: "重试",
    modelsRefreshing: "正在刷新账户列表与模型…",
    routeConfigHint: "登录只保存凭据；由下方的供应商把请求路由过去。",
    // account rows
    quotaNone: "—",
    quotaResets: "重置于",
    removeAccount: "移除",
    moveUp: "上移",
    adopt: "接管写回",
    release: "取消写回",
    adopted: "已写回 Codex 读取的位置",
    released: "已停止写回",
    pluginManaged: "由插件管理",
    orderHint: "第一个账号优先尝试；上移可调整顺序。",
    antigravityRisk: "Google 可能停用它在 Antigravity 之外看到被使用的账号。请用一个你承受得起损失的账号。",
    siwcHint:
      "登录过程中 OpenAI 会现场注册本应用，所以浏览器最终落到的地址里同时带着 code 与 client id。请粘贴整条地址。",
    // sign-in failure hints
    hintKeyring:
      'Codex 把这次登录放在系统钥匙串里。可在它的 config.toml 里设置 cli_auth_credentials_store = "file"，或先用 Codex 本身登录再使用 Discover。',
    hintClientId: "请粘贴浏览器最终落到的整条地址——它同时带着 code 与 OpenAI 签发的 client id。",
    hintInstallClaude: "请先安装 Claude Code：`claude` 命令行（桌面应用不是它）。",
    hintEligible: "OpenAI 未授予该账户使用其 API 的权限；需要一个包含 API 的 ChatGPT 套餐。",
    hintRevoked: "这次登录已被永久拒绝。重新登录即可恢复该账户。",
  },
} satisfies Record<"en" | "zh-CN", Record<string, string>>;

export type ProvidersStrings = (typeof STRINGS)["en"];

export function useProvidersStrings(): ProvidersStrings {
  const { language } = useI18n();
  return STRINGS[language] ?? STRINGS.en;
}

/**
 * The account a finished sign-in refers to.
 *
 * Drivers report different `user` shapes: Gemini and Copilot often carry
 * the full account id, Codex/Claude/ChatGPT API carry the email or user,
 * and a plugin carries `provider:account`. Try the exact forms first so
 * two accounts with the same email under different providers cannot
 * collide. (From the accounts page, kept verbatim.)
 */
export function accountForTask(task: SignInTask, accounts: AccountEntry[]): AccountEntry | undefined {
  const user = task.user.trim();
  if (!user) return undefined;
  const taskProvider = task.provider.startsWith("plugin:") ? task.provider.slice("plugin:".length) : task.provider;
  const exact = accounts.find((account) => account.id === user || account.id === `${task.provider}:${user}`);
  if (exact) return exact;
  const sameProvider = accounts.find(
    (account) =>
      (account.provider === task.provider || account.provider === taskProvider) &&
      (account.account === user || account.id.endsWith(`:${user}`)),
  );
  if (sameProvider) return sameProvider;
  return accounts.find((account) => account.account === user || account.id.endsWith(`:${user}`));
}

/** What to do about a sign-in failure, from what the server said. */
export function hintFor(message: string, s: ProvidersStrings): string | undefined {
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

/** Parse the manual model-id field: commas, newlines, trimmed and unique. */
export function parseManualModels(value: string): string[] {
  return [
    ...new Set(
      value
        .split(/[\n,]/)
        .map((id) => id.trim())
        .filter(Boolean),
    ),
  ];
}
