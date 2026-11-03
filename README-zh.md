# any-converter

`any-converter` 是一个 LLM API gateway，用来把一种客户端协议接入到另一种上游模型厂商协议。

它适合这些实际场景：

- Claude Code -> OpenAI-compatible 或 Gemini 后端
- Codex CLI / Responses 客户端 -> Claude 后端
- OpenAI-compatible 应用 -> Anthropic 或 Gemini
- 在一个本地统一入口后面做多 provider 路由、故障切换和观测

## 支持内容

### 客户端与上游格式

| 格式 | 接口路径 |
|------|----------|
| OpenAI Chat Completions | `/v1/chat/completions` |
| OpenAI Responses | `/v1/responses` |
| Claude Messages | `/v1/messages` |
| Google Gemini | `/v1beta/models/{model}:generateContent` |

### 网关能力

- 支持所有已实现格式之间的请求和响应转换
- 支持流式 SSE 转换
- 支持基于 `model_routes` 的多 provider 路由
- 支持 `priority` 和 `round_robin` 两种故障切换策略
- 支持精确匹配和通配符的模型映射
- 支持带脱敏的请求日志
- 支持 SQLite 主存储 + JSONL fallback 的双写日志
- 提供 Web UI：playground、logs、usage、status、config
- 提供 Desktop 应用：本地管理 provider、route 和内嵌 server

## 快速开始

### 1. 安装

```bash
cargo install --path crates/cli
```

### 2. 准备配置

从示例配置开始：

```bash
cp config.example.toml config.toml
```

最小可用示例：

```toml
[server]
host = "127.0.0.1"
port = 1228
api_key = "hello-any"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-xxx"

[[model_routes]]
pattern = "*"
provider = "anthropic"
```

### 3. 启动网关

```bash
any-converter serve --config config.toml
```

启动后，本地网关地址就是 `http://127.0.0.1:1228`。

## 一键接入编码 Agent

`agents` 命令会把编码 Agent 自己的配置文件接上网关——地址、每个 Agent 专属的网关
密钥、以及完整模型目录——断开时**逐字节还原**原配置（每个被修改的文件旁都有一个
`.before-any-converter` 备份，可作离线教急副本）：

```bash
any-converter agents list                    # 看谁已安装、谁已接入
any-converter agents connect claude-code     # 接入 Claude Code（需网关在运行）
any-converter agents connect pi --model deepseek/deepseek-chat
any-converter agents disconnect claude-code  # 还原接入前的配置
any-converter agents sync                    # 配置变更后刷新模型目录
any-converter agents rotate-key pi           # 重发该 Agent 的专属密钥
```

支持的 Agent：**Claude Code**、**Codex**、**Gemini CLI**、**Pi**。
Web 界面（`/agents` 页）提供同样能力，并带漂移检测：接入期间若有其他工具改写了
Agent 配置，页面会提示「配置被外部修改」并可一键重新应用。

每个接入的 Agent 拿到自己的网关密钥（`acw_<agent>_…`），请求归因不依赖
User-Agent，密钥可独立轮换；Agent 密钥只能调推理接口，不能调 agents 管理接口。

注意：

- 网关必须在运行（`any-converter serve`）；CLI 只是个薄客户端。若网关已停又急需
  断开，可直接用 Agent 配置旁的 `.before-any-converter` 文件手动还原。
- Codex 接的是 `~/.codex`（主 `CODEX_HOME`）；不碰 OAuth 登录——网关密钥写进
  `auth.json` 的 `OPENAI_API_KEY` 字段。
- Claude Code：项目级 `settings.local.json` 优先于用户级配置，个别项目里自行设置
  的 `env` 会覆盖接入配置。

## 常见客户端接入（手动）

以下为没有 `agents` 集成的客户端（或偏好手动改配置时）的手动接入方式：

### Claude Code

把 Claude 侧流量指向本地网关：

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:1228
export ANTHROPIC_AUTH_TOKEN=hello-any
```

### Codex CLI 与其他 Responses 客户端

必须使用 OpenAI Responses wire API，不要按 Chat Completions 来测：

```toml
base_url = "http://127.0.0.1:1228/v1"
wire_api = "responses"
api_key = "hello-any"
```

注意：

- `base_url` 需要指向 `/v1`
- `wire_api` 需要是 `"responses"`
- 如果要让 Codex 读取 `/v1/models`，配置里的模型名建议全部小写

### OpenAI-compatible 客户端

```bash
export OPENAI_BASE_URL=http://127.0.0.1:1228/v1
export OPENAI_API_KEY=hello-any
```

## 模型到 provider 的路由

推荐使用 `model_routes`，而不是只按格式做 legacy route。

```toml
[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[[providers]]
name = "deepseek"
format = "openai_chat"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"

[[providers]]
name = "anthropic"
format = "claude"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-xxx"

[[model_routes]]
pattern = "gpt-*"
providers = ["openai", "deepseek"]
strategy = "priority"

[[model_routes]]
pattern = "claude-*"
provider = "anthropic"

[[model_routes]]
pattern = "*"
provider = "openai"
upstream_model = "gpt-4.1-mini"
```

说明：

- `priority` 按顺序依次尝试 provider
- `round_robin` 会轮换起始 provider，同时保留 failover
- `upstream_model` 可以强制指定最终发给上游的模型名

## 模型映射

每个 provider 都可以把客户端传入的模型名改写成上游真实模型名：

```toml
[[providers]]
name = "openai"
format = "openai_chat"
base_url = "https://api.openai.com"
api_key = "sk-proj-xxx"

[providers.model_map]
"claude-sonnet-4" = "gpt-4.1"
"gpt-4o*" = "gpt-4.1"
"*" = "gpt-4.1-mini"
```

匹配顺序是：精确匹配 -> 最长通配符匹配 -> 原样透传。

## 可观测性

开启请求日志：

```toml
[logging]
level = "info"
dir = "./logs"
max_disk_mb = 500
retention_days = 30          # 明细元数据
body_retention_days = 7      # 抓取的 body（体积最大）
rollup_retention_days = 0    # 小时聚合，0 表示永久          # 0 表示永久保留
# jsonl_mirror = true        # 可选：额外写 usage.YYYY-MM-DD.jsonl

[logging.request_log]
enabled = true
trace_enabled = true
```

开启后你会得到：

- `./logs` 下的 JSONL 审计日志
- `./logs/any-converter.sqlite3` 里的 SQLite 镜像
- 请求审计日志按 10 MiB 分片，例如 `requests.2026-07-22.000.jsonl`
- 自动脱敏后的请求和响应捕获
- 非流式和流式请求的 token usage 提取
- 消息、工具调用、工具结果的结构化 trace 摘要

注意：

- 只有 `logging.request_log.enabled = true` 还不够，必须同时设置 `logging.dir`
- 对于流式请求，日志中的延迟是首包时间，不是整条流结束时间

## Web UI

Web UI 会作为独立进程运行在 Rust gateway 旁边，提供：

- `/playground`：交互式转换调试
- `/logs`：请求日志查看
- `/usage`：token 和延迟统计
- `/status`：健康状态和磁盘占用
- `/config`：编辑 `config.toml`

先启动 Rust server，再启动 Web UI：

```bash
SERVER_URL=http://127.0.0.1:1228 \
LOG_DIR=../../logs \
CONFIG_PATH=../../config.toml \
  pnpm --filter @any-converter/web dev
```

默认访问地址是 `http://localhost:3000`。

## Desktop 应用

Tauri Desktop 应用提供本地控制台能力：

- 管理 provider
- 管理 model route
- 启动、停止、重启内嵌 server
- 查看本地 logs 和 usage
- 使用转换 playground

在仓库根目录运行：

```bash
pnpm --filter @any-converter/desktop tauri dev
```

## CLI 小工具

### 离线转换 JSON

```bash
echo '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}' \
  | any-converter convert --from openai-chat --to claude
```

### 转换 SSE 流

```bash
curl -N https://example.com/stream \
  | any-converter stream --from openai-chat --to claude
```

## 排障

### Codex 连不上或行为异常

重点检查：

- 客户端是否设置了 `wire_api = "responses"`
- 客户端是否指向了 `.../v1`
- 当前模型是否能命中有效的 provider route

### 已经开了日志但看不到记录

重点检查：

- 是否设置了 `logging.dir`
- 是否开启了 `[logging.request_log].enabled = true`
- server 启动后是否真的处理过请求

### Web UI 里没有 logs 或 usage

重点检查：

- `LOG_DIR` 是否和 `logging.dir` 指向同一个目录
- 该目录下是否已经生成 `any-converter.sqlite3` 或 JSONL 文件

## 更多文档

- 完整配置示例：[config.example.toml](./config.example.toml)
- 构建和运行细节：[docs/build.md](./docs/build.md)
- 架构与数据流：[docs/architecture.md](./docs/architecture.md)
- English README: [README.md](./README.md)

## 许可证

MIT
