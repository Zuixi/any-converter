# 供应商页信息架构（Providers Page IA）

状态：草图 v2（2026-11-12），已吸收评审意见，待评审
范围：桌面端 `apps/desktop`，合并 Providers + Accounts 为单一「供应商」页
参照：magpie `internal/gui/assets/index.html` Providers 页 + `app.js` renderProviders/renderEditor/renderAdd/renderAccounts

v2 修订（评审意见）：
1. Zone 1 移除拖拽排序 —— 供应商顺序无路由含义，仅按配置序展示
2. 路由策略完全移出本页 —— 归 Routes 页；本页只提示"未路由"，不承载排序/策略
3. 插件免 CLI —— 点击市场 tile 即"安装 → 登录"一条链（对齐 magpie 体验），需补三件后端基础设施

---

## 0. 目标与原则

1. **供应商是唯一的一等概念。** 用户心智里没有 "provider 配置" 和 "account 凭据" 的区分，只有"我接入了哪些供应商、健康吗、路由到谁"。
2. **列表是摘要，详情是 Dialog。** 卡片回答"现在怎么样"，浮层回答"怎么改"。不跳页，列表上下文永远在场（magpie `app.js:6054` 注释的原则）。
3. **接入方式三分法统一一切 UI。** API Key / OAuth 订阅（含插件）/ 插件托管，是同一组字段的三种取值（对应 `ProviderConfig` 的 `api_key` / `accounts` / `plugin`），Add 入口、卡片徽标、详情分区全部按这三分法组织。
4. **本页只管"接入"，不管"路由"。** 哪个 provider 优先、什么策略，全部在 Routes 页表达；本页至多出现"有模型尚未路由"的提示与跳转。
5. **插件与内建订阅同权。** 点击插件 tile = 安装（自动）→ 登录，与内建订阅登录走同一 UI、同一状态机，用户不应感知"插件需要额外步骤"。

---

## 1. 概念模型：一个供应商，三种接入方式

```
ProviderCard（页面上的卡片）
├── credential: APIKey { masked }              ← base_url + api_key 直连
│             | OAuth  { accounts[] }          ← 订阅账号池（内建 + 插件登录）
│             | Plugin { spec, providers[] }   ← OpenCode 插件托管
├── wire:      format / api / auth_scheme
├── models:    model_map（暴露集）+ discovery（上游发现集）
└── routes:    model_routes 中引用它的 patterns（只读展示）
```

| 接入方式 | 用户动作 | 后端落点 | 卡片徽标 |
|---|---|---|---|
| 订阅账号 | 浏览器/设备码 OAuth 登录（内建或插件） | `credentials` store + `provider.accounts` | `OAuth ×N` + 主账号配额条 |
| 厂商 API | 粘贴 API Key | `provider.api_key` → secret store | `Key ✓/✗` |
| 插件托管 | 点击市场 tile（自动安装+登录） | `provider.plugin` | `插件 · spec` |

---

## 2. 页面总览（线框）

```
┌──────────────────────────────────────────────────────────────────┐
│ 供应商                                [🔍 筛选…] [👁 隐藏账号]      │
│ 3 个供应商 · 14 个模型已暴露 · 11 已路由 · 1 未路由 ⚠               │
├──────────────────────────────────────────────────────────────────┤
│ ZONE 1 · 已接入（按配置顺序展示，无排序语义）                        │
│                                                                  │
│ ┌────────────────────────────────────────────────────────────┐   │
│ │ [G] Gemini 订阅            [code_assist] [OAuth ×2]          │   │
│ │     me@gmail.com 78% ▓▓▓▓▓▓░░ · you@gmail.com 12% ▓░░░░░░   │   │
│ │     12 模型 · 12 已路由                                       │   │
│ └────────────────────────────────────────────────────────────┘   │
│ ┌────────────────────────────────────────────────────────────┐   │
│ │ [D] DeepSeek               [openai_chat] [Key ✓ 34ms]        │   │
│ │     api.deepseek.com · 6 模型 · 1 未路由 ⚠                    │   │
│ └────────────────────────────────────────────────────────────┘   │
│                                                                  │
│ ▸ 待绑定账号（2）   已登录，尚未接入任何供应商                       │
├──────────────────────────────────────────────────────────────────┤
│ ZONE 3 · 接入供应商                                                │
│ ┌────────────────────────────────────────────────────────────┐   │
│ │ [🔍 搜索厂商，或粘贴 base_url…]      [＋ 自定义]              │   │
│ │                                                            │   │
│ │ 订阅账号 · 登录即用，无需 Key                                  │   │
│ │ [Gemini] [Antigravity] [Codex] [ChatGPT] [Copilot] [Claude]  │   │
│ │ [Cursor 登录…] [Factory 登录…] [更多插件登录…]  ← 未安装也可点 │   │
│ │                                                            │   │
│ │ 厂商 API · 填入 API Key                                      │   │
│ │ [OpenAI] [Anthropic] [Google Gemini] [DeepSeek] [Moonshot] …  │   │
│ └────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────┘
```

导航变化：`Providers` + `Accounts` 两 tab → **一个 `供应商` tab**。Routes 保留独立页并承接全部路由/策略编辑。

---

## 3. Zone 1 · 供应商卡片

### 3.1 卡片解剖

```
┌──────────────────────────────────────────────────────────────┐
│ [logo]  名称                          [wire 徽标] [凭据徽标] [⋯] │
│         健康摘要行（按接入方式变化）                              │
│         模型 · 路由摘要行（含未路由警告）                          │
└──────────────────────────────────────────────────────────────┘
```

- **无拖拽、无排序语义**：列表按 config 中 providers 声明顺序展示。`⋯` 菜单：探测刷新、删除（二次确认，提示将解绑哪些路由）。排序/优先级/策略 → Routes 页。
- **凭据徽标**（卡片右上）：
  - OAuth：`OAuth ×N`，N=绑定账号数；全部失效 → 红色 `已登出`
  - Key：`Key ✓`（绿，含上次探测延迟）/ `未配置`（灰）
  - 插件：`插件 · spec`
- **健康摘要行**：
  - OAuth → 每个账号一行配额条（复用 AccountsPage 现有渲染：颜色阈值 70/95、resets 时间），最多显示 2 个 + `+N`
  - Key → `base_url · 上次探测: N 模型 (34ms)` 或探测失败摘要 + 重试按钮
- **模型摘要行**：`N 模型 · M 已路由`；`M < N` 时显示 `K 未路由 ⚠`（点击跳 Routes 页并过滤该 provider——提示存在、编辑不在本页）。
- **点击卡片任意处** → 打开详情 Dialog（见 §6）。

### 3.2 数据来源（全部现有 API）

| 卡片字段 | 来源 |
|---|---|
| 名称/format/base_url/api_key_configured | `getConfig().providers[]` |
| 暴露模型数、已路由数 | `exposedModels()` + `getCatalog().models[].routed` |
| 绑定账号 + 配额 | `accountsOverview()`（前端按 `provider.accounts` join） |
| 探测状态/延迟 | `getProviderModels(name)` 缓存 |
| logo | preset id → 本地静态资源映射（前端新增，不上传） |

---

## 4. Zone 2 · 待绑定账号（ac 特有，magpie 无此问题）

magpie 的账号天然属于内建 provider；ac 的账号是独立凭据库，可先登录后绑定。合并后这些账号不能消失，进入专属折叠区：

```
▸ 待绑定账号（2）
   ┌───────────────────────────────────────────────┐
   │ copilot:me@github.com   16 模型   [接入为供应商…] │
   │ claude:me@x.com         —        [接入为供应商…] │
   └───────────────────────────────────────────────┘
```

- 判定：`account.id` 的 provider 前缀未出现在任何 `provider.account_ids()` 中（前端可算，无需后端）。
- `接入为供应商…` 打开预填好的 mini 表单：名称 + accounts 绑定 + 模型选择（`accountModels(id)` 已有）→ 创建 provider + 可选建路由。
- 折叠区默认展开当且仅当存在待绑定账号。

---

## 5. Zone 3 · 接入供应商（Add Sheet）

### 5.1 布局

常驻页底的卡片，搜索框 + 常驻出口：

- **搜索框**：匹配厂商名 / base_url host / 别名（中文"智谱"命中 zhipu）/ 插件名。**粘贴以 `http(s)://` 开头 → 切换为自定义模式并自动探测 format**。
- **＋ 自定义**："任何 OpenAI / Anthropic / Gemini 兼容地址" → 完整表单（现 ProvidersPage 的 add Card 迁移）。

### 5.2 分区（组织原则 = 接入方式三分法）

| 分区 | 副标题 | tile | 点击行为 |
|---|---|---|---|
| **订阅账号（内建）** | "登录即用，无需 Key" | gemini / antigravity / codex / chatgpt-api / copilot / claude | 原位内嵌 OAuth 流（URL/设备码/粘贴回调，即现 AccountsPage sign-in 卡整体迁入）；antigravity 显示封号风险警示 |
| **订阅账号（插件）** | 同上，tile 角标 `插件` | 内置目录中的插件登录项（Cursor、Factory、Grok 等），**未安装也列出** | **点击 = 一条链：安装（进度可见）→ 自动启动 OAuth 流 → 完成**。安装对用户透明，仅在失败时露出详情 |
| **厂商 API** | "填入 API Key" | OpenAI / Anthropic / Google / DeepSeek / Moonshot / MiniMax / 智谱 / xAI …（扩展 `PROVIDER_PRESETS`） | 原位展开 mini 表单：key + `探测` → 成功后 ModelPicker → 添加 |
| 兼容网关 | "一个 Key，多家厂商" | OpenRouter / OneAPI / 各家 `-cn` 端点 | 同上 |
| 本机 | "无需 Key" | Ollama / LM Studio (Phase 2) | 同上 |

tile 选中后**不关 sheet、不跳页**，在 sheet 原位展开对应的表单/登录流（magpie 7097 行的原位原则）。

### 5.3 插件一键链（本版核心增量）

```
点击 [Cursor 登录…] tile
  ↓ POST /v1/plugins/install { spec }        ← 新增后端（§10）
  ↓ 安装进度（npm install，失败显示手动指引）
  ↓ 完成后自动 load → providers/methods 就绪
  ↓ startSignIn("plugin:cursor")             ← 现有，零改动
  ↓ 同一 OAuth UI（URL/设备码/粘贴）           ← 现有，零改动
  ↓ done → accountModels 发现 → 收尾链
```

- 已安装的插件 tile 直接进入登录，不重复安装。
- 插件目录首版为**内置静态清单**（spec + 名称 + 登录方式 + 简介 + 官网），随 app 版本更新；`更多插件登录…` 打开完整目录列表（Phase 2 动态化）。

### 5.4 添加完成的统一收尾

三种接入方式共用同一条收尾链（现 `create()` 的强化）：
`凭据就绪 → 模型选择(ModelPicker/自动全选) → [✓ 同时创建默认路由] → 完成并打开详情 Dialog`

---

## 6. 详情 Dialog（点击卡片）

单 Dialog 内分区滚动（不做 tab）：

```
┌──────────────────────────────────────────────────┐
│ [logo] Gemini 订阅                 [官网↗] [删除] │
├──────────────────────────────────────────────────┤
│ ① 接入方式                                        │
│   OAuth 账号 ×2                    [＋ 登录另一个]  │
│    ● me@gmail.com   78% ▓▓▓▓▓▓░░  [置顶][移除]     │
│    ○ you@gmail.com  12% ▓░░░░░░░  [置顶][移除]     │
│   （账号间顺序 = 路由池内尝试顺序 · provider_pool 语义，│
│     置顶按钮调整，不做自由拖拽）                       │
│   ── 或改为 ── [API Key ▾]（接入方式可切换）         │
│ ② 基本信息                                         │
│   format / api / base_url / auth_scheme / proxy    │
│   （探测按钮 + 结果：N 模型 · 延迟 · 错误 hint）       │
│ ③ 模型                                            │
│   [↻ 刷新] ModelPicker + 手动 ID + 保留 wildcard 提示 │
│ ④ 路由状态（只读）                                  │
│   引用它的 patterns chips → 「去 Routes 页管理」      │
│   （空时："暴露了 N 个模型但没有路由，请求不会到达" + 跳转）│
└──────────────────────────────────────────────────┘
```

- **①账号区** = 现 AccountsPage 表行的全部能力迁入：配额条、置顶（`saveAccountOrder`）、模型发现（行内展开）、codex adopt/release、移除。**provider 间路由优先级不在此处**——那是 Routes 页 model_routes 的 providers 顺序。
- **①接入方式切换**：OAuth ↔ Key 互斥（`ProviderConfig` 语义：设置 accounts 则忽略 api_key），切换时提示影响。
- **②探测**复用 `discoverProvider` / `getProviderModels(refresh=true)`。
- Dialog 宽度 ~720px，ESC/遮罩关闭；有未保存草稿时拦截。

---

## 7. 关键流程

### 7.1 冷启动（零供应商）
Zone 1 显示三张引导大卡：`登录订阅账号` / `填入 API Key` / `登录插件订阅`——锚点滚动到 Add Sheet 对应分区。

### 7.2 内建订阅接入（全流程不出页）
点 `Codex` tile → sheet 原位展开设备码/登录说明 → 轮询 `pollSignIn` → done → 收尾链（§5.4）。

### 7.3 插件订阅接入（v2 新增）
点 `Cursor 登录…` tile → 自动安装（进度）→ OAuth → done → 收尾链。用户全程不需要知道 CLI 存在。

### 7.4 厂商 Key 接入
点 `DeepSeek` tile → mini 表单（名称/base_url 预填）→ 粘贴 key → `探测` → 绿色 `N 模型 (34ms)` → ModelPicker 全选 → 收尾链。

### 7.5 账号登出恢复
上游 401 被 `failover.rs` 标记 revoked → 卡片健康行变 `已登出` 红 → 详情账号行显示 `Sign in again`（复用 sign-in 流 + `hintFor` 文案）。

---

## 8. 状态矩阵

| 状态 | 卡片表现 | 详情内 |
|---|---|---|
| 加载中 | 骨架屏 ×3 | — |
| Key 未配置 | 灰 `未配置` + 行内"打开并粘贴 Key" | ②高亮 key 字段 |
| 探测失败 | 红 `探测失败` + hint 摘要 | 完整错误 + 重试 |
| 配额耗尽 | 该账号配额条满红，账号沉底不重排（provider_pool 语义） | 同 + resets 时间 |
| 账号全部登出 | 卡片红 `已登出` | 每行 Sign in again |
| 模型未路由 | 黄 `K 未路由 ⚠` | ④区提示 + 跳转 |
| 插件加载失败 | 黄 `插件无法加载` | plugin error + 手动修复指引（含 CLI 兜底命令） |
| 插件安装中 | tile 进度态（Add sheet 内） | — |

---

## 9. 组件与数据流

```
pages/ProvidersPage.tsx            页面骨架 + zone 编排（唯一数据入口）
hooks/useProvidersOverview.ts      聚合 getConfig + getCatalog + accountsOverview
  → { cards: ProviderCardView[], unbound: AccountEntry[], summary }
components/providers/
  ProviderCard.tsx                 卡片（无拖拽）
  ProviderDetailDialog.tsx         详情浮层（分区编排）
  AccountRows.tsx                  账号行 ← 从 AccountsPage 迁移
  SignInFlow.tsx                   OAuth 流 UI ← 从 AccountsPage 迁移
  AddProviderSheet.tsx             Zone 3（搜索/分区/tile/原位表单）
  ProviderTile.tsx                 tile + 原位 mini-form
  PluginInstallGate.tsx            插件 tile 的 安装→登录 一条链（v2 新增）
  PluginCatalog.tsx                内置插件目录清单（v2 新增）
  UnboundAccountsSection.tsx       Zone 2
  ModelPicker.tsx                  （已有）
lib/constants.ts                   PROVIDER_PRESETS 扩展 + 分区字段 kind + PLUGIN_CATALOG
lib/management.ts                  + installPlugin / uninstallPlugin / pluginInstallStatus
```

迁移原则：AccountsPage 的 `STRINGS` 双语字典按区块拆入各组件；`hintFor`、`accountForTask` 等纯函数迁 `lib/`。

---

## 10. 后端缺口与分期

### Phase 1 — 纯前端重组（现有 API 全覆盖）
- 页面合并、卡片列表、详情 Dialog、Add Sheet 分区、待绑定区
- 账号置顶/配额/模型发现/移除（全部现有端点）
- 插件分区仅列**已安装**插件的登录项（现状能力，体验已对齐 magpie 的"点击登录"）

### Phase 2 — 插件免 CLI 三件套（核心后端工作）
1. **服务端安装 API**：`POST /v1/plugins/install { spec }`、`DELETE /v1/plugins/{spec}`、安装状态查询。实现 = 把 CLI `plugin_install`（`crates/cli/src/main.rs:1065`，`npm install --prefix` 子进程）逻辑挪进 server crate（admin 鉴权下）。CLI 命令保留，共享同一实现。
2. **Node 运行时自举**：桌面端 Tauri **sidecar 内置 Node**（`host.rs` 的 `default_launch` 已支持 `ANY_CONVERTER_NODE` 覆盖，指向 sidecar 即可，服务端零改动）；无 sidecar 的场景 fallback 系统 node 并给出引导文案。
3. **内置插件目录**：随 app 分发的静态清单（spec/名称/登录方式/简介），前端 `PLUGIN_CATALOG` 常量起步；动态市场（搜索/README/版本）Phase 3。
4. preset 目录扩展（MiniMax/智谱/xAI/OpenRouter…）。
5. provider 启用/停用开关（可选）。

### Phase 3 — 打磨
- 动态插件市场（对齐 magpie market.go：搜索/README/版本/star）、上游状态页轮询徽标、供应商成本徽标、从其他工具导入。

---

## 11. 开放问题（需决策）

1. **Node 自举方式**：已决策（2026-11-12）—— Tauri sidecar 内置。`scripts/fetch-node.mjs` 下载 Node v22.14.0 到 `src-tauri/binaries/node-<triple>`，`tauri.conf.json` 的 `bundle.externalBin` 打包；desktop 启动时经 `plugins::host::set_node_override` 指向 sidecar（desktop crate forbid unsafe，不能 env::set_var，故走 OnceLock 注入）。无 sidecar 的发行版维持系统 node。
2. **插件目录的来源**：内置静态清单（推荐起步）vs 直接对接 magpie-community/plugins 仓库动态拉取（更快丰富，但引入外部依赖与版本耦合）。
3. **账号"置顶"够不够**：账号间顺序用置顶按钮（现 AccountsPage 语义）vs 自由拖拽。推荐置顶（改动小、语义清晰），拖拽留待反馈。
4. **AccountsPage 是否彻底删除**：推荐 Phase 1 直接删（i18n 迁移）；保守方案是先隐藏后删。
5. **logo 资产**：静态 SVG 打包进前端（参考 lobehub/icons）；无 logo 的 custom provider 用首字母色块。
