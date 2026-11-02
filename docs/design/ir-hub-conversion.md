# IR Hub 转换架构（对标 magpie）

状态：提案（未实施）
日期：2026-11-02
对标参照：magpie `internal/gateway/ir.go` 及其 reader/builder（`anthropic.go`、`chat.go`、`responses.go`）

## 1. 目标与非目标

**目标**

- 把 `converters/` 下 12 对两两直转（约 5500 行）折叠为 4 reader + 4 builder 的 IR hub
- 移植 magpie 经大规模实践验证的三层结构：wire 解析 → 中间表示（IR）→ wire 构造
- 对外 API（`FormatConverter` trait、`get_converter`、server/web-bridge）零改动
- 行为保持：现有 converter_matrix / roundtrip / property / fuzz 测试全绿

**非目标**

- 不移植 magpie 的 agent-bridge 概念（Claude 订阅桥、branch 会话、leanPrompt、envElsewhere、remoteCaller）——any-converter 是纯协议网关，不驱动 agent 二进制
- 不移植 prompt-cache TTL 管理（`clientCache`/`hourMarks`）——`cache_control` 作为透传字段处理（见 §6）
- 不新增 IR 能力以满足"将来可能"的需求：IR 只建模现有 12 对实际传递的内容

## 2. 三层架构

```
Claude Code ──/v1/messages──────┐                                          ┌── /v1/messages ──────→ Anthropic 上游
Codex ────────/v1/responses─────┤     ┌────────────────────────────┐       ├── /v1/responses ─────→ OpenAI 上游
OpenAI 客户端 ─/v1/chat/comp─────┼────→│  reader(A)  →  IrRequest   │──────→├── /v1/chat/comp ─────→ 兼容上游
Gemini 客户端 ─/v1beta/*────────┘     │  (wire→IR)   →  builder(B)  │       └── /v1beta/* ─────────→ Gemini 上游
                                      └────────────────────────────┘
   流式（已是此形状，不动）:  A 的 SSE → StreamAdapter.parse → CanonicalStreamEvent → StreamAdapter.emit → B 的 SSE
```

加第 5 个格式的成本：1 个 types.rs + 1 reader + 1 builder + 1 StreamAdapter（现状：8 个转换文件）。

## 3. IR 类型（`crates/core/src/ir/request.rs`）

```rust
pub struct IrRequest {
    pub model: String,
    pub system: Vec<IrPart>,               // 系统提示统一为 parts（Claude 的 blocks、Gemini 的 systemInstruction 都能装）
    pub messages: Vec<IrMessage>,
    pub tools: Vec<IrTool>,
    pub tool_choice: IrToolChoice,         // Auto | None | Required | Named(String)
    pub sampling: Sampling,                // max_tokens, temperature, top_p, top_k, stop
    pub stream: bool,
    pub reasoning: Reasoning,              // Off | Budget(u32) | Effort(String)
    pub extra: NamespacedExtra,            // 见 §6
}

pub struct IrMessage { pub role: Role, pub parts: Vec<IrPart> }   // Role: User | Assistant

pub enum IrPart {
    Text(String),
    Image  { url: String, media_type: String, data: Option<String>, detail: Option<String> },
    ToolCall   { id: String, name: String, args: serde_json::Value },
    ToolResult { call_id: String, is_error: bool, content: Vec<IrPart> },
    Thinking   { text: String, signature: Option<Signature> },
    Sealed     { by: SealApi, raw: serde_json::Value },   // 原样字节回放，见下
}

pub struct IrTool { pub name: String, pub description: String,
                    pub schema: serde_json::Value, pub strict: bool }

pub struct Signature { pub by: SealApi, pub value: String }
pub enum SealApi { Anthropic, Responses, Gemini }   // 谁签的名，决定回给谁
```

### 从 magpie ir.go 移植的关键语义（每条都有实战出处）

| magpie 符号 | 移植为 | 出处/理由 |
|---|---|---|
| `Part.Sealed/SealedBy` | `IrPart::Sealed` | Anthropic `redacted_thinking`、Responses `encrypted_content`：**只有签发它的 API 的 builder 能原样写回**，其它格式视为无文本的 reasoning。字节级保留，禁止解释 |
| `Part.Signature/SignedBy` | `Thinking.signature` | Claude thinking 签名跨轮回放，丢签名下一轮被拒 |
| `joinSplitCalls`（ir.go:130） | claude builder 内 | Anthropic 上游把连续 assistant 消息当一个 turn；agent 拆开发的并行 tool_use 必须并回（magpie #1275） |
| `pairToolMessages`（chat.go:637） | openai_chat builder 内 | OpenAI 要求 tool 消息紧跟 tool_calls 并按 id 配对；孤儿/失序在 builder 修补 |
| `thinkingOffUnlessAsked` | resp/chat reader + builder 约定 | 部分厂商 Anthropic 端点默认开思考，不显式关就白烧 token（magpie 经验） |
| `Usage`（cache_read/write、reasoning） | 扩展现有 `ir::Usage` | 现有 Usage 缺 cache 字段，补齐 |

### 明确不移植

`Hits`/`WebSearch` part（现有 12 对未传递搜索结果，等有格式需要再加）、`Deferred` 工具（Claude Code ToolSearch 专属）、`Loads`、`Standalone`。fixtures 若证明某个"没传递"判断错了，补进 IR 并记录出处。

## 4. 每格式 reader + builder（`formats/<fmt>/`）

wire 类型（`types.rs`）不动。每格式目录新增：

```
formats/claude/{types.rs, read.rs, build.rs, stream.rs, mod.rs}
```

- `read.rs`：`ClaudeRequest → IrRequest`（消费式，move 不 clone）
- `build.rs`：`&IrRequest → ClaudeRequest`，**目标格式的方言约束全部在这里**：tool 配对、split-call 合并、system 装配、默认值（如 Claude 必须有 `max_tokens`）
- `reasoning.rs`（现有 150 行 effort↔budget 映射）归位：reader 侧 `Reasoning::Budget → Effort` 的换算放 IR 层一次性做，各 builder 不再各自换

`converters/` 12 个文件逐个删除，`get_converter` 改为组合：

```rust
pub fn get_converter(from: Format, to: Format) -> Option<&'static dyn FormatConverter> {
    // 每对仍是静态分发的 thin wrapper：read(from) → build(to)
    // convert_stream_event 维持现有 StreamAdapter 组合，不动
}
```

## 5. 流式事件补齐（`ir/stream.rs`）

`CanonicalStreamEvent` 已是 hub，但对照 magpie `EventKind`（ir.go:353）缺三个事件，都是签名回放的流式半边：

```rust
pub enum CanonicalStreamEvent {
    // 现有: Start, TextDelta, ToolCallStart, ToolCallDelta, ThinkingDelta, Done
    ThinkingStart { id: Option<String> },          // KThinkStart：新 reasoning 块开始
    ThinkingSignature { value: String, by: SealApi }, // KSig：块结束时收到签名
    SealedThinking { raw: Value, by: SealApi },    // KSealed：redacted/encrypted reasoning 原样过路
}
```

不加 `KAlive`/`KSearch`/`KImage`（网关场景无搜索轮次、无模型生图中转；keepalive 由 server 层负责）。

## 6. `extra` 透传袋规则（防表达力倒退，也防串味）

```rust
pub struct NamespacedExtra(Map<String, Value>);   // 键: "<seal_api>::<field>"
```

- reader 把 IR 不建模的字段按 `厂商::字段名` 入袋（如 `anthropic::metadata`）
- builder 只取**同厂商前缀**的条目；异厂商条目丢弃并 `tracing::debug!` 记录
- 例外白名单显式列出（预计只有 `stream_options`、`user` 这类无语义冲突的通用字段）
- `cache_control`（Claude prompt cache 标记）：随 part 走还是入袋，在实现 reader 前用 fixtures 验证——现有 12 对若透传它，则给 `IrPart` 加 `cache: Option<&'static str>`（magpie 的 `cache string` 字段）

## 7. 迁移计划（绞杀者，一次一对）

| 阶段 | 内容 | 门槛 |
|---|---|---|
| P0 | 跑 `conversion_bench` 记录基线；跑 converter_matrix 把 12 对输出固化为 golden fixtures（部分已有） | 基线数字入 PR 描述 |
| P1 | 写 `ir/request.rs`、扩 `ir/stream.rs`、扩 `Usage` | 编译 + 单测 |
| P2 | 每格式实现 read/build，先以单测 + `read(A)→build(A)` 同格式往返恒等测试验证 | 单格式通过 |
| P3 | 逐对翻转 `get_converter`：换 IR 路径 → matrix + golden 比对 → 一致才删对应 pairwise 文件。建议顺序：chat↔claude（fixtures 最多）→ gemini 对 → resp 对 | 每对一个 PR |
| P4 | 删 `converters/`（只剩 mod.rs）、`shared.rs`/`reasoning.rs` 中已归位部分；`docs/architecture.md` §1.1/§3.1 改写 | 全测试绿 + bench 回归 ≤5% |

**Triage 规则**：golden 比对不一致时禁止改 fixture 凑通过。每个不一致进清单，人工裁决"IR 错（改 IR）"还是"原来就错（改 fixture 并在 PR 里写明）"。12 对代码不对称（如 claude_to_chat 559 行 vs chat_to_claude 516 行）说明分歧存在，此步骤最耗人力。

## 8. 验收标准

1. 全部现有测试（converter_matrix、stream_matrix、roundtrip、property、fuzz、fixtures）不修改断言地通过——被裁决为"原来就错"的除外，逐条列名
2. 新增 property test 不变量：
   - `read(A) → build(A)` 语义恒等（允许字段顺序与默认值填充分量差异）
   - 任何 reader 产出的 `IrRequest` 喂任何 builder 不 panic（fuzz）
3. `conversion_bench`：1MB 典型载荷回归 ≤5%
4. 代码量：`converters/` ~5500 行 → IR + 8 个 read/build 预计 ~2200 行
5. Code review 红线：reader/build 路径禁止 `serde_json::Value::clone`（除 `extra` 入袋/出袋的一次），禁止 `String::clone` 于大文本

## 9. 风险与对策

| 风险 | 对策 |
|---|---|
| 行为漂移（12 对里的隐含 bug 被当成规范） | P0 golden + §7 triage 规则 |
| IR 表达力不足 → 字段静默丢失 | `extra` 袋 + roundtrip/property 测试 + 丢弃时 debug 日志 |
| IR 腐烂成厂商旗标堆 | 每个进 IR 的 vendor 字段必须带注释注明属于哪条边（magpie ir.go 的 `clientCache` 等先例：这不可避免，但要留痕） |
| 错误信息退化（不知哪层错） | IR 携带 `provenance: Format`；错误信息格式 `"<from>→IR→<to>: <阶段>失败"` |
| 性能回归 | bench 门槛 + §8.5 红线 |
