# any-converter

一个用 Rust 编写的 LLM API 格式互转换工具，支持 CLI、流式管道和 HTTP 代理服务器模式。

## 支持的格式

| 格式 | 接口路径 | 适用场景 |
|------|----------|----------|
| OpenAI Chat Completions | `/v1/chat/completions` | OpenAI SDK、兼容服务 |
| Claude Messages | `/v1/messages` | Claude Code、Anthropic SDK |
| OpenAI Responses | `/v1/responses` | Codex CLI、OpenAI SDK |
| Google Gemini | `/v1beta/models/{model}:generateContent` | Gemini CLI |

## 安装

```bash
cargo install --path crates/cli
```

## 使用方式

### CLI — 离线转换

```bash
# OpenAI Chat → Claude
echo '{"model":"gpt-4","messages":[{"role":"user","content":"你好"}]}' \
  | any-converter convert --from openai-chat --to claude

# 流式 SSE 转换
curl -N ... | any-converter stream --from openai-chat --to claude
```

### HTTP 代理服务器

让任意 LLM CLI 工具透明使用任意后端：

```bash
# 使用配置文件启动
any-converter serve --config config.example.toml

# 快速启动（单个 provider）
any-converter serve --port 8080 \
  --provider openai --format openai-chat \
  --base-url https://api.openai.com \
  --upstream-key sk-xxx
```

配置 CLI 工具指向代理：

```bash
# Claude Code 使用 OpenAI 后端
export ANTHROPIC_BASE_URL=http://localhost:8080

# Codex CLI 使用 Claude 后端
export OPENAI_BASE_URL=http://localhost:8080
```

### 配置文件

参见 [config.example.toml](config.example.toml)。支持：
- 多 upstream provider 配置（含 API key）
- 模型名映射（精确匹配 + 通配符）
- 路由规则（客户端格式 → upstream provider）
- 客户端 API key 认证

## 架构

核心采用**类型化中间表示（IR）**，每种格式只需实现一对适配器，而非 N² 种两两转换。

## 开发

```bash
cargo test --workspace          # 运行全部 156 个测试
cargo build --release           # 构建发布版本
```

## 许可证

MIT
