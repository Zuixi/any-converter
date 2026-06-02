# any-converter Desktop Client 设计方案

## 一、定位与目标

**any-converter Desktop** = 可视化配置管理中心 + Server 控制台 + 请求调试器 + 日志监控面板

核心价值：让用户无需手写 `config.toml`、无需记 CLI 命令，通过 GUI 完成所有操作。

---

## 二、功能模块设计

### 模块总览

```
┌─────────────────────────────────────────────────────────────┐
│  Sidebar Navigation                                          │
│  ├─ 📊 Dashboard        (Server 状态、今日请求统计)           │
│  ├─ 🔌 Providers      (上游 Provider 增删改查)               │
│  ├─ 🛣️ Routes         (客户端格式 → Provider 路由规则)       │
│  ├─ 🧪 Playground     (请求格式转换调试器)                    │
│  ├─ 📋 Logs           (请求日志、错误追踪)                    │
│  └─ ⚙️ Settings       (应用设置、主题、数据管理)              │
└─────────────────────────────────────────────────────────────┘
```

### 1. Dashboard — 状态总览

| 功能 | 说明 |
|------|------|
| Server 状态卡片 | 运行中/已停止，一键 启动/停止/重启 |
| 监听信息 | 显示当前 host:port，点击复制 |
| 今日统计 | 请求总数、成功数、错误数、平均延迟 |
| Provider 健康 | 各 Provider 在线状态（带最后心跳时间） |
| 最近请求 | 最近 10 条请求的简略信息 |

### 2. Providers — 上游服务商管理

| 功能 | 说明 |
|------|------|
| Provider 列表 | 卡片/表格展示，支持搜索、排序 |
| 新增 Provider | Form：名称、格式(dropdown)、Base URL、API Key、模型映射表 |
| 编辑 Provider | 弹窗编辑，API Key 密文显示（可切换可见） |
| 模型映射 | 内嵌表格：客户端模型名 → 上游模型名，支持 `*` 通配符 |
| 测试连接 | 发送一个简单请求验证 Provider 是否可用 |
| 删除 Provider | 二次确认，检查是否被 Route 引用 |

**表单字段**：
- 名称（唯一标识）
- 格式：`openai_chat` / `claude` / `openai_responses` / `gemini`
- Base URL：`https://api.moonshot.cn`
- API Key：密文输入框
- 模型映射：动态键值对表格

### 3. Routes — 路由规则管理

| 功能 | 说明 |
|------|------|
| Route 列表 | 展示所有路由规则 |
| 新增 Route | 客户端格式(dropdown) → Provider(dropdown) |
| 路由可视化 | 简单的流向图：Client Format → Provider → Upstream Format |
| 删除 Route | 二次确认 |

### 4. Playground — 请求调试器

> 技术人员最常用的功能，类似 Postman + 格式转换预览。

| 功能 | 说明 |
|------|------|
| 输入面板 | JSON 编辑器（选择源格式） |
| 转换预览 | 显示转换为 Canonical IR 的中间结果 |
| 目标输出 | 显示转换为目标格式的请求体 |
| 实时发送 | 选择 Provider，直接发送并展示完整响应链路 |
| 格式选择 | Source Format ↔ Target Format 双向切换 |
| 历史记录 | 保存最近调试的请求 |

### 5. Logs — 请求日志监控

| 功能 | 说明 |
|------|------|
| 日志列表 | 时间、客户端格式、Provider、模型、状态码、耗时 |
| 筛选 | 按 Provider、格式、状态码、时间范围筛选 |
| 详情面板 | 点击行展开：完整请求体、响应体、转换前后的对比 |
| 导出 | 导出为 JSON/CSV |
| 清空 | 清空日志（二次确认） |

### 6. Settings — 应用设置

| 功能 | 说明 |
|------|------|
| Server 默认配置 | 默认 host、port、客户端 API Key |
| 外观 | Light / Dark / System 主题切换 |
| 数据管理 | 数据目录位置、备份、恢复 |
| 关于 | 版本号、开源协议、检查更新 |

---

## 三、技术架构

### 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│  Frontend (React 19 + TypeScript)                           │
│  ├─ UI: shadcn/ui + Tailwind CSS                           │
│  ├─ State: Zustand (轻量，Tauri 推荐)                        │
│  ├─ Data Fetching: TanStack Query                          │
│  ├─ Routing: TanStack Router                               │
│  ├─ Form: react-hook-form + zod                            │
│  └─ JSON Editor: @monaco-editor/react                      │
├─────────────────────────────────────────────────────────────┤
│  Bridge (Tauri v2 Commands + Events)                        │
│  ├─ Commands: 前端调用 Rust 函数 (async/await)               │
│  └─ Events: Rust 推送到前端 (server 状态变更、日志流)        │
├─────────────────────────────────────────────────────────────┤
│  Backend (Rust — Tauri App)                                 │
│  ├─ Commands 层: 暴露给前端的 API                           │
│  ├─ Service 层: 业务逻辑                                    │
│  ├─ DB 层: SQLite (rusqlite/sqlx)                          │
│  ├─ Server 集成: 复用 any-converter-server crate            │
│  └─ Core 集成: 复用 any-converter-core crate (Playground)   │
└─────────────────────────────────────────────────────────────┘
```

### 技术栈明细

| 层级 | 技术 | 说明 |
|------|------|------|
| 包管理 | pnpm | |
| 构建工具 | Vite | Tauri 官方推荐 |
| 前端框架 | React 19 + TypeScript | |
| UI 组件 | shadcn/ui | 基于 Radix UI + Tailwind |
| 样式 | Tailwind CSS 3.4 | |
| 状态管理 | Zustand | 轻量，无 Provider 嵌套 |
| 数据获取 | TanStack Query v5 | 缓存、轮询、乐观更新 |
| 表单 | react-hook-form + zod | 类型安全表单验证 |
| 路由 | TanStack Router | 类型安全路由 |
| JSON 编辑 | Monaco Editor | VS Code 同款 |
| 图标 | Lucide React | shadcn/ui 默认 |
| Toast | Sonner | shadcn/ui 推荐 |
| 后端框架 | Tauri v2 | |
| 数据库 | SQLite + rusqlite | 内嵌，零配置 |
| ORM/迁移 | rusqlite + 手写迁移 | 简单项目够用 |

---

## 四、项目目录结构

```
any-converter/
├── crates/
│   ├── core/                    # 现有：格式转换核心库
│   ├── server/                  # 现有：HTTP 代理服务器
│   ├── cli/                     # 现有：CLI 工具
│   └── desktop/                 # 新增：Tauri Rust 后端
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs            # Tauri 入口
│           ├── lib.rs
│           ├── commands/          # Tauri Commands（暴露给前端）
│           │   ├── provider.rs
│           │   ├── route.rs
│           │   ├── server.rs
│           │   ├── playground.rs
│           │   ├── log.rs
│           │   └── settings.rs
│           ├── services/          # 业务逻辑
│           │   ├── provider_service.rs
│           │   ├── route_service.rs
│           │   └── server_manager.rs
│           ├── db/                # SQLite 数据层
│           │   ├── mod.rs
│           │   ├── schema.rs
│           │   └── migrations/
│           └── state.rs           # Tauri AppState
│
├── src/                         # 前端代码（React）
│   ├── main.tsx
│   ├── App.tsx
│   ├── routeTree.gen.ts         # TanStack Router
│   ├── components/
│   │   ├── ui/                  # shadcn/ui 组件（cli 自动生成）
│   │   ├── layout/
│   │   │   ├── Sidebar.tsx
│   │   │   ├── Header.tsx
│   │   │   └── AppShell.tsx
│   │   └── providers/
│   │       ├── ProviderCard.tsx
│   │       ├── ProviderForm.tsx
│   │       └── ModelMapTable.tsx
│   ├── pages/
│   │   ├── Dashboard.tsx
│   │   ├── Providers.tsx
│   │   ├── Routes.tsx
│   │   ├── Playground.tsx
│   │   ├── Logs.tsx
│   │   └── Settings.tsx
│   ├── hooks/
│   │   ├── useServerStatus.ts
│   │   ├── useProviders.ts
│   │   └── useRoutes.ts
│   ├── lib/
│   │   ├── api.ts               # Tauri Command 封装
│   │   ├── utils.ts
│   │   └── constants.ts
│   ├── stores/
│   │   └── appStore.ts          # Zustand 全局状态
│   └── types/
│       └── index.ts
│
├── src-tauri/                   # Tauri 配置文件（标准目录）
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/
│   └── icons/
│
├── index.html
├── package.json
├── vite.config.ts
├── tailwind.config.ts
├── tsconfig.json
└── components.json              # shadcn/ui 配置
```

---

## 五、数据库设计（SQLite）

```sql
-- providers: 上游服务商
CREATE TABLE providers (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    format      TEXT NOT NULL,        -- openai_chat | claude | openai_responses | gemini
    base_url    TEXT NOT NULL,
    api_key     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- model_maps: 模型名称映射
CREATE TABLE model_maps (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id     INTEGER NOT NULL,
    client_model    TEXT NOT NULL,    -- "*" for wildcard
    upstream_model  TEXT NOT NULL,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE,
    UNIQUE(provider_id, client_model)
);

-- routes: 客户端格式 → Provider 路由
CREATE TABLE routes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    client_format   TEXT NOT NULL,    -- openai_chat | claude | openai_responses | gemini
    provider_id     INTEGER NOT NULL,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE,
    UNIQUE(client_format, provider_id)
);

-- app_settings: 应用级配置
CREATE TABLE app_settings (
    key     TEXT PRIMARY KEY,
    value   TEXT NOT NULL
);

-- request_logs: 请求日志（用于 Dashboard 统计）
CREATE TABLE request_logs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp       TEXT NOT NULL,
    client_format   TEXT,
    provider_name   TEXT,
    model           TEXT,
    status_code     INTEGER,
    error           TEXT,
    duration_ms     INTEGER
);
```

---

## 六、Tauri Commands API 设计

```rust
// ===== Provider Commands =====
#[command]
async fn list_providers(state: State<'_, AppState>) -> Result<Vec<Provider>, Error>;

#[command]
async fn get_provider(state: State<'_, AppState>, id: i64) -> Result<Provider, Error>;

#[command]
async fn create_provider(state: State<'_, AppState>, req: CreateProviderReq) -> Result<Provider, Error>;

#[command]
async fn update_provider(state: State<'_, AppState>, id: i64, req: UpdateProviderReq) -> Result<Provider, Error>;

#[command]
async fn delete_provider(state: State<'_, AppState>, id: i64) -> Result<(), Error>;

#[command]
async fn test_provider(state: State<'_, AppState>, id: i64) -> Result<TestResult, Error>;

// ===== Route Commands =====
#[command]
async fn list_routes(state: State<'_, AppState>) -> Result<Vec<RouteWithProvider>, Error>;

#[command]
async fn create_route(state: State<'_, AppState>, req: CreateRouteReq) -> Result<Route, Error>;

#[command]
async fn delete_route(state: State<'_, AppState>, id: i64) -> Result<(), Error>;

// ===== Server Control Commands =====
#[command]
async fn start_server(state: State<'_, AppState>) -> Result<(), Error>;

#[command]
async fn stop_server(state: State<'_, AppState>) -> Result<(), Error>;

#[command]
async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, Error>;

// ===== Playground Commands =====
#[command]
async fn convert_request(body: String, from: String, to: String) -> Result<String, Error>;

#[command]
async fn convert_response(body: String, from: String, to: String) -> Result<String, Error>;

// ===== Log Commands =====
#[command]
async fn list_logs(state: State<'_, AppState>, filter: LogFilter) -> Result<Vec<LogEntry>, Error>;

#[command]
async fn clear_logs(state: State<'_, AppState>) -> Result<(), Error>;

// ===== Settings Commands =====
#[command]
async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, Error>;

#[command]
async fn update_settings(state: State<'_, AppState>, settings: AppSettings) -> Result<(), Error>;
```

---

## 七、关键实现要点

### 1. Server 生命周期管理

Tauri 后端需要管理一个 `tokio::task::JoinHandle`，实现：
- `start_server()`：从 DB 读取配置 → 构造 `ServerConfig` → `tokio::spawn(any_converter_server::run(config))`
- `stop_server()`：`handle.abort()` + 等待优雅关闭
- 通过 Tauri Event 向前端推送状态变更

### 2. 配置与 DB 的同步

- Provider/Routes 的增删改全部走 SQLite
- 启动 Server 时从 DB 动态构建 `ServerConfig`
- 无需读写 `config.toml` 文件（Desktop 场景下 DB 是单数据源）

### 3. Playground 复用 Core

Playground 的格式转换直接调用 `any_converter_core::convert::convert_request/convert_response`，零网络开销。

---

## 八、UI 风格建议

```
主色调: Zinc (shadcn/ui 默认，中性专业)

强调色: 根据运行状态动态变化
  - Server 运行中: Emerald 绿色
  - Server 停止: Slate 灰色
  - 错误状态: Red 红色

布局:
  - 左侧固定 Sidebar (w-64)
  - 顶部 Header (h-16, 显示页面标题 + 全局操作)
  - 中间 Main Content (p-6)
  - 右下角 Toast 通知区

交互:
  - 所有表单操作后有 Toast 反馈
  - 列表支持行内编辑或 Drawer 侧滑编辑
  - Server 启动/停止有加载状态和确认弹窗
```

---

## 九、开发时序建议

| 阶段 | 内容 | 预计工作量 |
|------|------|-----------|
| **Phase 1: 脚手架** | 初始化 Tauri + React + shadcn/ui + pnpm，搭好目录结构 | 1-2h |
| **Phase 2: DB + 后端 API** | SQLite schema、迁移、Tauri Commands 骨架 | 3-4h |
| **Phase 3: Providers 页面** | CRUD 完整功能（列表、表单、模型映射） | 4-6h |
| **Phase 4: Routes 页面** | 路由规则管理 | 2-3h |
| **Phase 5: Server 控制** | 启动/停止 + Dashboard 状态展示 | 3-4h |
| **Phase 6: Playground** | JSON 编辑器 + 转换调试 | 3-4h |
| **Phase 7: Logs + Settings** | 日志查看、应用设置、主题切换 | 2-3h |
| **Phase 8: 打磨** | 错误处理、空状态、加载态、快捷键 | 2-3h |

**总计：约 20-30 小时开发量**
