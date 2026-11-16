# MODEL — 领域数据模型

`dsh-protocol` crate 的权威类型定义。**本文件是写代码时的唯一参照**；
与 dsh 源码冲突时以源码为准，并立刻修正本文件。

依据：`deepseek-ai/deepseek-harness`（`master`，2026-11 核对）
`packages/api/session-controller/src/types.ts`、
`packages/llm/llm/src/{types,message,content}.ts`、
`packages/llm/token-meter/src/projection.ts`、
`packages/interaction/permission-presets/src/types.ts`、
`docs/subsystems/{session,session-projection}.zh.md`、
`docs/persistence-catalog.zh.md`。

本文件讲「是什么、怎么处理」；「为什么这样定」见
[DECISIONS.md](DECISIONS.md) #8–#12。

## 类型地图（找东西用这张表）

| 要写的东西 | 去哪节 | Rust 落点 |
| --- | --- | --- |
| 事件反序列化、未知事件处理 | 层 1、层 2、三条开放轴 | `WireEvent` `SurfaceOp` |
| 时间线渲染 | 层 3、表层折叠 | `Surface` `SurfaceNode` `MessageBase` |
| 流式输出 | 层 4 | `AssistantStreamFrame` `StreamChunk` |
| statusline / 侧栏状态 | 层 5 | `Projected<T>` `ProjectionUpdate` |
| 模型下拉、effort 档位 | 层 6 | `ModelCatalog` `ModelReasoning` |
| mode 控件 | 层 5 `permissions` + 层 6 权限目录 | `PermissionCatalog` `PermissionSelection` |
| 错误上报 | 层 6 | `ProtocolError` |
| 工具行、UiBlock | 层 3 `tool-call` + 表层折叠「工具行配对」 | `SurfaceNode` + `tool/result.data.meta` |
| 侧栏分组（工作区） | 层 7 | `WorkspaceView` `WorkspaceRecord` |

## 三条开放轴（最重要的一节）

dsh 的三个维度全部是**合并可扩展（merge-extensible）**的开放集合，
不是封闭枚举。任何实现都必须按「已知 + 未知降级」处理，不得穷举。

| 轴 | 已知成员数 | 扩展机制 | Rust 表示 |
| --- | --- | --- | --- |
| 事件 `type` | 63 | `SessionEventMap` declaration merging | `String` + 惰性 JSON 载荷 |
| 投影 `key` | 22 | `SessionProjectionMap` declaration merging | `String` 键 + 惰性 JSON 值 |
| 内容块 `type` | 7 | `ContentBlockMap` declaration merging | `String` 标签 + 惰性 JSON 载荷 |

### 开放集合的实现规则

1. **MUST NOT** 在事件、投影、内容块三处使用 `#[serde(deny_unknown_fields)]`。
2. **MUST NOT** 用无 fallback 的 `#[serde(tag = "...")]` 封闭枚举承接三者；
   遇到未见成员时必须有明确分支。
3. **MUST** 保留原始 `serde_json::Value`，不做「解析后丢弃」。
4. **MUST** 按下方「未知事件判定」处理事件，不得静默丢弃。

### 未知事件判定（协议硬规则）

`SessionWireEvent.ignorable` 决定未知 `type` 的处置：

| 情况 | 处置 |
| --- | --- |
| 未知 `type` + `ignorable: true` | **跳过**，不影响 fold；记一条 debug 日志 |
| 未知 `type` + `ignorable` 缺失 | **拒绝 fold 该会话**，返回 `ProtocolError::UnknownRequiredEvent`，UI 显示「需要更新客户端」，**MUST NOT** 静默丢弃或猜测 |

理由（dsh 源码原话）：未知的必需事件可能改变其余日志的解释方式；
静默丢弃等于让一个被掏空的会话继续运行。

## 层 1：wire 信封

```rust
/// 一条会话事件的信封。载荷不透明，按需解析。
pub struct WireEvent {
    pub type_name: String,                      // JSON "type"，开放集合
    pub seq: SessionSeq,
    pub time: i64,                              // epoch ms
    pub data: serde_json::Value,                // 不透明载荷
    pub ignorable: Option<bool>,                // true 才允许跳过
    pub source_event_seqs: Option<serde_json::Value>,  // 仅 surface 事件
    pub surface_op: Option<SurfaceOp>,          // 仅 surface 事件
}

/// 表层操作。wire 形态是 `"append"` 或 `{"op":"replace","startSeq","endSeq"}`。
pub enum SurfaceOp {
    Append,
    Replace { start_seq: SessionSeq, end_seq: SessionSeq },
}
```

注意：

- `surface_op` 的 wire 形态是**字符串与对象混合**的 untagged 联合，serde 需手写
  反序列化（`"append"` 是裸字符串，不是 `{"op":"append"}`）。
- 非 surface 事件**禁止**携带 `surface_op` / `source_event_seqs`；
  收到即为协议违例。

## 层 2：事件类型

### surface 事件（仅 5 个，产生模型可见历史）

| type | 载荷要点 |
| --- | --- |
| `system/message` | `{ turn, step, message: SystemMessage }` |
| `developer/message` | `{ turn, step, message: DeveloperMessage, headerSeq? }` |
| `user/message` | `UserMessage`（直接就是 message，无外层） |
| `assistant/message` | `{ turn, step, message: AssistantMessage, stream: Vec<StreamRecord>, usage?: TokenUsage, interrupted?: true }` |
| `tool/result` | `{ turn, step, message: ToolResultMessage, error?: {name,code,reason?}, meta?: JsonValue }` |

只有这 5 个进入时间线。

### 日志专用事件（不产生历史，不进时间线）

| 类别 | type |
| --- | --- |
| 循环边界 | `turn/start` `turn/end` `step/start` `step/end` `session/end-seed` |
| 模型尝试 | `tool/call` `assistant/attempt` |
| 请求状态 | `request/header` `request/context` |
| 压缩 | `compaction/start` `compaction/summary` `compaction/end` `compaction/prune` |
| 审批权限 | `approval/asked` `approval/decided` `approval/policy` `permission/preset` `sandbox/mode` |
| 会话配置 | `model/selection` `session/title` `working-directory/change` `agent-preset/selected` `agent/inbox/spliced` |

其余（`goal/change` `todo/write` `plan/mode` `command/*` `job/*` `hook/*`
`llm/retry*` `image/offload` `deliverables/presented` `feedback/*` `schedule/change`
`subagent/*` `team/*` `tool-workflow/*` `tool/ptc-*` `web/*` `workspace/changes`
`session-log-deepseek/*`）按同类处理：**观察但不常驻**。

### `tool/call` 的特殊待遇

`tool/call` 是日志专用事件，但工具行渲染需要它的 `time` 做耗时。
**保留一个极小索引**：`HashMap<ToolCallId, i64>`（只存时间）。
工具名与参数从 assistant 消息的 `tool-call` 内容块取，不重复存储。

## 层 3：消息与内容块

```rust
pub struct MessageBase {
    pub id: MessageId,
    pub role: MessageRole,               // system | developer | user | assistant | tool
    pub content: Vec<serde_json::Value>,  // 开放集合，惰性
    pub source: serde_json::Value,        // 溯源信息，UI 不用
}

/// tool 角色消息额外字段
pub struct ToolResultExtra {
    pub tool_call_id: ToolCallId,
    pub is_error: Option<bool>,
}
```

### 内容块（7 个已知，开放）

| type | 字段 | 用途 |
| --- | --- | --- |
| `text` | `text` | 可见正文 |
| `reasoning` | `text` | 思考内容，与正文分开渲染 |
| `image` | `attachment`, `offloaded?` | 图片引用（不是字节） |
| `file` | `attachment` | 文件引用 |
| `tool-call` | `id`, `name`, `arguments`（**原始 JSON 字符串，未解析**） | 工具调用 |
| `tool-addition` | `toolName` | 动态加工具 |
| `tool-removal` | `toolName` | 动态删工具 |

`arguments` 保持原始字符串：模型产出的就是字符串，解析属于渲染期，
且解析失败必须降级为原文而不是错误。

## 层 4：流式帧

实时增量**不走** session 事件。走独立的进程内帧，重连靠基线补。

```rust
pub enum AssistantStreamFrame {
    Start { attempt_id: LlmAttemptId, revision: u64, turn: u32, step: u32,
            started_after_seq: Option<SessionSeq> },
    Chunk { attempt_id: LlmAttemptId, revision: u64, index: u64,
            time: i64, chunk: serde_json::Value },
    End   { attempt_id: LlmAttemptId, revision: u64, index: u64,
            outcome: StreamOutcome },
}

pub enum StreamOutcome {
    Committed { event_type: String, seq: SessionSeq },
    Abandoned,
}
```

`chunk` 的文法（`StreamChunk`）：

| type | 字段 |
| --- | --- |
| `block-start` | `index`, `blockType` |
| `text-delta` | `index`, `text` |
| `reasoning-delta` | `index`, `text` |
| `tool-call-delta` | `index`, `id`, `name?`, `argumentsDelta` |
| `block-end` | `index`, `block`（已装配的完整块） |
| `usage` | `usage` |
| `finish` | `reason`, `replayState?` |

### 实测帧形态（2026-11，rc.3）

跑通一次真实 prompt 抓下的样本：`crates/dsh-protocol/fixtures/follow-stream.jsonl`。
上面的 Rust 结构是设计稿，下面是与它的差异与补充。

**外面还包了两层。** WebSocket 上每条消息是 Gateway 的逻辑流帧，`session/follow`
的业务帧在它的 `value` 里：

```json
{"type":"item","streamId":"<客户端生成>","value":{"type":"assistant-stream","frame":{…}}}
```

`streamId` 由客户端在 `open` 时自己生成，用于一条 socket 上多路复用。

**打开一个流**（`ws://<host>:<port>/api/remote.mux`，文本 JSON）：

```json
{"type":"open","streamId":"fx1","endpoint":"session/follow",
 "payload":{"args":{"request":{"address":{"kind":"session","sessionId":"…"},
                              "assistantStream":true}}}}
```

`address` 是 `{kind:'session',sessionId}` 或 `{kind:'subagent',parentSessionId,
childSessionId,mode}`。**`assistantStream: true` 必须显式给**，否则拿不到流式帧。

**三种 value**：`snapshot` / `event` / `assistant-stream`。

`assistant-stream.frame` 一次纯文本回答的实测序列（9 帧）：

| frame.type | 关键字段 |
| --- | --- |
| `start` | `attemptId`、`revision`、`turn`、`step`、`startedAfterSeq` |
| `chunk` | `attemptId`、`revision`、`index`、`time`、`chunk{…}` |
| `end` | `attemptId`、`revision`、`index`、`outcome{…}` |

`chunk.chunk` 实测出现顺序：`block-start{index,blockType}` →
`text-delta{index,text}` → `block-end{index,block}` → `usage{…}` → `finish{reason,replayState}`。
`blockType` 见过 `reasoning` 与 `text`，且 **reasoning 块可能为空**（先 block-start
再立刻 block-end、text 为空串）——UI 必须正确处理空 reasoning 块，不能当成错误。

`usage` 字段名是 **camelCase**：`inputTokens` / `outputTokens` / `cacheReadTokens` /
`cacheWriteTokens` / `totalTokens`。实测一次两字回答的 `inputTokens` 是 14766
（系统提示占绝大部分），这说明 **ctx 圆环的分子只能是 `inputTokens`**，不能用
outputTokens 或总字节估算。

`end.outcome` 实测：`{"kind":"committed","eventType":"assistant/message","seq":18}`
——与设计稿 `Committed{event_type,seq}` 同构，线上统一 camelCase。
`finish.reason` 实测：`{"kind":"stop"}`。

`revision` 逐帧递增（实测 1,2,3…9），所以「不连续即失效」这条规则在真实流量里
是可判定的。

### 流式状态机规则

实现：`crates/dsh-protocol/src/fold.rs`（`AssistantStreamFold`）。
回归：`tests/assistant_stream_fold.rs` 回放 `fixtures/follow-stream.jsonl` 的真实帧。
GPUI 层**不应该**自己再折叠一遍流，只渲染 `fold.visible_blocks()`。

1. **连续性**：`start` 带 `revision`（实测为 1）；此后每帧 `revision + 1`，
   且 `index + 1`（`start` 不占 `index`，首个 chunk 是 `index: 0`）。
2. **空洞**（`revision` 跳号或 `index` 跳号）→ 丢弃累积、清空尝试、等待新的
   `start`。返回 `Apply::Resynced`。`outcome` 不清空——它是持久事实，不属于累积。
3. **重放**（`revision` 不前进）→ 幂等忽略，**不清空累积**。重连与多路复用都会
   重发，按空洞处理会把已显示内容抹掉。返回 `Apply::Ignored`。
4. **没有 `start` 的孤帧** → 丢弃并计入 `anomalies`，不猜。
5. **`block-end` 是权威版本**：其 `block.text` 覆盖增量累积的文本。
6. **终局**：`end { Committed { seq } }` → 用该 `seq` 的持久 `assistant/message`
   **替换**累积结果（持久版权威，流式版只是临时显示）；`end { Abandoned }` →
   标记中断并保留已显示前缀（对应 `interrupted: true`）。
7. **空 `reasoning` 块是常态**（实测 `block-start` 后立刻 `block-end`、文本为空串），
   渲染必须过滤：用 `Block::is_visible()` / `visible_blocks()`，不要自己判。
8. **未知帧与未知增量类型** → 计数后继续，不打断流（DECISIONS #8）。
9. **断线恢复**用快照里的 `assistantStream` 基线，**不要重放整轮**：
   `AssistantStreamFold::from_baseline()` 吃掉 `revision` + `activeAttempt.stream`，
   并按 `nextIndex` 对齐下标，使重连后第一帧正好接上。

锚点实例（真实采集）：`start(rev 1)` → 9 帧 → `end(rev 9, index 7)`，
`outcome = committed{eventType: 'assistant/message', seq: 18}`。

## 层 5：投影

客户端**从不折叠领域事件**。host 折叠，客户端收到成品值。

```rust
pub struct ProjectionBaseline {
    pub as_of_seq: Option<SessionSeq>,   // -1/缺失 = 空日志
    pub values: serde_json::Map<String, serde_json::Value>,  // 开放键集
}

pub struct ProjectionUpdate {
    pub session_id: SessionId,
    pub seq: SessionSeq,                 // 引起变化的那个事件的 seq
    pub changes: Vec<(String, serde_json::Value)>,  // 只含变化的键
}
```

### 存在性语义（易错点）

**键缺失 ≠ 值为 null**，两者含义不同：

| 状态 | 含义 | UI 行为 |
| --- | --- | --- |
| 键缺失 | 该投影未注册（未组合此能力） | **隐藏**对应控件 |
| 键存在且为 `null` | 能力存在但当前无值 | 显示占位/空态 |

例：`title: null` = 还没生成标题；`permissions` 键缺失 = 没组合权限服务，
statusline 左侧 mode 控件必须隐藏。

Rust 侧用「存在性 + 值」双表达：

```rust
pub enum Projected<T> {
    Absent,        // 键缺失
    Null,          // 键存在，值为 null
    Value(T),
}
```

### 已核实的投影键

| key | 形状 | 本项目用途 |
| --- | --- | --- |
| `permissions` | `{ currentValue: String }` | statusline 左侧 mode |
| `modelSelection` | `{ provider, model, reasoningEffort? }` | statusline 中段 |
| `contextPressure` | `{ pressureTokens?, projectedTokens?, contextWindow? }` | statusline ctx 圆环 |
| `tokenUsage` | `{ uncachedInputTokens, outputTokens, cacheReadTokens, cacheWriteTokens }` | token 显示 |
| `contextBreakdown` | `{ systemTokens, toolsTokens, messageTokens }` | 可选明细 |
| `title` | `String \| null` | 侧栏会话名 |
| `userQuestions` | 见下 | 提问卡片 |
| `todos` `plan` `goal` | 各自领域 | M2+ |
| `sessionStats` `turnOutline` | 各自领域 | M3 |
| `inbox` | `InboxWireState` | 队列显示 |
| `sessionListMetadata` | `{ blank, lastPromptAt }` | 侧栏排序 |
| `workingDirectory` `agentPreset` `agentTeam` `subagent*` | 各自领域 | 非 MVP |

`SessionProjectionSummaryExcluded` 标记：`contextPressure`、`contextBreakdown`
**不进会话列表摘要**（列表行不携带），只有打开的会话才有。`tokenUsage` 进摘要。

## 层 6：目录与错误

### 模型目录

```rust
pub struct ModelCatalog {
    pub default: ModelSelection,              // 未配置会话的部署默认
    pub routable_providers: Vec<String>,
    pub groups: Vec<ModelProviderGroup>,
    pub failures: Vec<ModelCatalogFailure>,   // provider 级故障隔离
}
pub struct ModelProviderGroup { pub id: String, pub name: String,
                                pub models: Vec<CatalogModel> }
pub struct CatalogModel {
    pub id: String, pub name: String,
    pub description: Option<String>,
    pub reasoning: Option<ModelReasoning>,
    pub badge: Option<String>,
}
pub struct ModelReasoning { pub efforts: Vec<EffortInfo>, pub default_effort: Option<String> }
pub struct EffortInfo { pub id: String, pub name: String, pub description: Option<String> }
```

`failures` 是 provider 级故障隔离：某个 provider 列举模型失败时进 `failures`，
而不是让整张表变空（`packages/api/session-controller/src/catalog.ts:65-71`）。
UI **MUST** 渲染它——否则「有凭据但 listing 失败」会表现为「什么都没配」。
空态的三种成因必须可区分：无凭据 / 有凭据但列举失败 / 全被过滤。

模型可见性的前提是凭据挂在 ref **`DEEPSEEK_API_KEY`** 名下（或 DeepSeek 账号登录
token），见 DESIGN「打开即用」。

effort 规则：`efforts[].id` 是**适配器持有的不透明 ID**，客户端
**MUST 精确匹配**列表中的值，**MUST NOT** 自动降级、别名或合成 `off`。
不在列表中的选择会在 provider I/O 前被 host 以
`UNSUPPORTED_REASONING_EFFORT` 拒绝。

### 权限目录

端点 `permissionPresets/catalog`（namespace 取自
`super(ctx, 'permissionPresets')`，`@Remote('catalog')`）。

```rust
pub struct PermissionCatalog {
    pub options: Vec<PresetOption>,        // 当前可选（含 live auto）
    pub default_options: Vec<PresetOption>,
    pub default_preset: String,
}
pub struct PresetOption { pub value: String, pub name: String,
                          pub description: Option<String> }
```

`currentValue` 取值：已配置 preset key、`auto`（会话级临时）、或派生的 `custom`
（当前 sandbox/approval 组合不匹配任何 preset）。

### 权限：来源与所有权（mode）

`mode` 不是单一旋钮，是**由 preset 命名的 `sandbox × approval` 组合**。

底层枚举是封闭的（这两个不是开放集合）：

| 旋钮 | 取值 | 语义 |
| --- | --- | --- |
| `sandbox/mode` | `read-only` / `workspace-write` / `danger-full-access` | 文件系统与执行边界 |
| `approval/policy` | `ask` / `never` | `ask`=弹审批；`never`=从不询问，所有审批自动 `rejected` |

组合空间 3×2=6。`PresetSpec { sandbox, approval, name?, description? }` 给组合起名。

#### 真源：会话日志三个事件（last-wins 折叠）

| 事件 | 作用 |
| --- | --- |
| `permission/preset` | **用户意图**，仅记日志、不进模型 transcript |
| `sandbox/mode` | 执行旋钮 |
| `approval/policy` | 执行旋钮 |

执行侧只读旋钮折叠；preset 事件单独存在是为了在两个 preset 共享同一组合时
保住用户选的名字。

#### 当前值是派生的，不存储

```text
sandbox  = 日志旋钮 ?? ctx.shell.sandboxMode
approval = 日志旋钮 ?? ctx.approval.config.policy ?? 'ask'

1. 上次选的 preset 仍匹配旋钮 → 用它（保住共享 bundle 的意图）
2. 上次选的是 auto 且 sandbox 匹配、approval==='never' → 仍是 auto
3. 否则取第一个匹配旋钮的已配置 preset
4. 都不匹配 → 'custom'
```

- `custom` **永不作为切换目标或事件载荷**，只是「无名字组合」的显示态；
- `auto` 是保留名，不能被配置成 preset；只存在于
  `packages/experimental/auto-review` 调 `registerAuto(admit)` 期间，
  仅当前会话有效，带同步准入闸门。

#### 可选项是进程级的

| 字段 | 内容 | 所有权 |
| --- | --- | --- |
| `options` | 已配置 preset +（集成活着时）`auto` | 部署配置 + 插件 |
| `defaultOptions` | 仅已配置 preset | 部署配置（`auto` 永不作未来会话默认） |
| `defaultPreset` | Config 字段（volatile） | 部署配置 |

出厂默认：`workspace-write`（workspace-write + ask）、
`danger-full-access`（danger-full-access + never）。

#### 三条写路径

| 路径 | 触发者 | 行为 |
| --- | --- | --- |
| `/permission <preset>` | 用户（客户端经 `session/prompt`） | 唯一给 Web 客户端用的写入口 |
| `pinInitialPermission` | host，`agent/created` | 三旋钮均空且未 seeded → 写默认 preset 三件套 |
| `registerAuto(admit)` | auto-review 集成 | 发布固定会话级 `auto` preset |

另有 `ctx.approval.setPolicy(agent, policy)` 可运行时改审批策略。

#### 客户端四条硬规则

1. 选项来自 catalog，当前值**只**来自 `permissions` 投影；不得自行推导 preset 名。
2. `custom` 可显示不可选；`auto` 仅当 catalog 列出时渲染。
3. `permissions` 键缺失 → 整个控件隐藏。
4. **catalog 无推送通道**（只有 `@Remote('catalog')`；host 侧
   `permission-presets/catalog-changed` 是进程内 cordis 事件，不过 wire），
   客户端需在会话切换与连接恢复时重拉。

### 错误模型

`RemoteErrorDetailsMap` 约 20 个具名码。客户端映射为：

```rust
pub enum ProtocolError {
    UnknownRequiredEvent { event_type: String },
    ModelUnavailable { provider: String, model: String },
    ProviderModelsUnavailable { provider: String },
    WriterHeld { session_id: SessionId },
    AgentBusy { reason: String },
    Conflict { session_id: SessionId, requested_cwd: String, existing_cwd: Option<String> },
    MigrationRequired { session_id: SessionId },
    Other { code: String, message: String },   // 未知码必须保留原文
}
```

未知错误码 **MUST** 保留原始 code + message 展示，不得吞掉。

## 层 7：工作区注册表（侧栏分组）

侧栏的目录分组**不是**会话投影，也不是模型目录，是独立的第五个数据面。
对应 dsh 的 `workspace` 存储领域 + `workspace/follow` 流。

### 存储位置

| 项 | 值 |
| --- | --- |
| 领域名 | `workspace`（version 2） |
| 表 | `workspaces`，键 `WorkspaceId` |
| 后端 | `storage-json`（`packages/bundle/base/cordis.patch.yml`） |
| 根目录 | `dshHomePath('storages')` → 默认 `~/.dsh/storages`，`$DSH_HOME` 可覆盖 |
| 布局 | `single`（领域未声明 `layout`，取默认） |
| 物理文件 | `~/.dsh/storages/workspace.json` |

```jsonc
// ~/.dsh/storages/workspace.json
{
  "unit": { "name": "workspace", "version": 2 },
  "global": {
    "initialized": true,
    "defaultWorkspaceId": "…",            // 可选
    "workspaceIds": ["ws_1", "ws_2"],     // 权威显示顺序
    "archivedSessionIds": [],
    "pinnedSessionIds": [],
    "pendingMutation": { "operation": "create", "workspaceId": "…" }  // 仅崩溃恢复期存在
  },
  "tables": {
    "workspaces": {
      "<WorkspaceId>": {
        "path": "/abs/canonical/realpath",  // fs.realpath，创建后永不重写
        "title": "demo",
        "sessionIds": ["sess_…"],           // 有序账本 = 显示顺序
        "createdAt": "ISO-8601",
        "updatedAt": "ISO-8601"
      }
    }
  }
}
```

### 客户端读取

```text
workspace/follow (stream)
  → baseline { workspaces[], order[], archivedSessionIds[], pinnedSessionIds[] }
  → upsert / remove / order / archived / pinned 增量
```

一元操作：`workspace/create` `initializeDefault` `rename` `delete`
`insertBefore` `insertSessionBefore` `archiveSession` `unarchiveSession`
`pinSession` `unpinSession`。

### 四条必守规则

1. **`WorkspaceId` 是生成的 uuid，不是路径。** 路径规范化会重写 path，
   引用锚点必须保持稳定。
2. **所有权真源是记录里的有序 `sessionIds`，绝不从会话 cwd 派生。**
3. **`workspace/delete` 只删注册记录、顺序条目和会话账本。** 目录、用户文件、
   实时会话、已持久化日志一概不动；这些会话变为 **Ungrouped**，不是被删。
4. **分组与归档/置顶互不影响。** 归档的会话保留它的 `sessionIds` 槽位
   （取消归档要恢复位置）；归档会同时移除该会话的置顶。

### 首次使用默认工作区

`workspace/initializeDefault()` 无参数。固定目录名 `default-workspace`
（Host 侧拥有），落在 `<Documents>/deepseek-harness` 下。`documentsDirectory`
可覆盖；不具备 Documents 查询能力的 Host 必须配置该值或改用文件夹选择器。
不满足条件时返回空，启动流程交给用户选目录。

### 错误码

`workspace/invalid-path`（path 不存在或非目录）、`workspace/name-conflict`、
`workspace/session-active`（归档被拒，details 按族列出未停的工作）。

## 表层折叠（surface fold）

时间线渲染的是**表层**，不是日志。表层支持区间替换（compaction 用）。

```rust
pub struct Surface {
    nodes: Vec<SurfaceNode>,
    position: HashMap<SessionSeq, usize>,   // seq → nodes 下标
    pub replace_generation: u64,            // 每次 replace +1
    pub as_of_seq: Option<SessionSeq>,
}

pub struct SurfaceNode {
    pub seq: SessionSeq,
    pub kind: NodeKind,                     // System|Developer|User|Assistant|ToolResult
    pub payload: serde_json::Value,         // 惰性，渲染时解析
    pub tool_call_id: Option<ToolCallId>,   // 仅 ToolResult
    pub is_error: Option<bool>,             // 仅 ToolResult
}
```

### fold 算法

```text
apply(event):
  if event 不是 surface 事件: 丢弃（按需提取极小索引，见 tool/call）
  node = build_node(event)
  match event.surface_op:
    Append:
      nodes.push(node); position[event.seq] = nodes.len()-1
    Replace { start_seq, end_seq }:
      start = position[start_seq]; end = position[end_seq]   // 按表层顺序定位
      若 start > end: 协议违例，拒绝 fold
      若区间覆盖 0 号节点的 system/message: 协议违例，拒绝 fold
      nodes.drain(start..=end)
      nodes.insert(start, node)
      重建受影响的 position 项
      replace_generation += 1
  as_of_seq = event.seq
```

规则来源：

- 替换端点必须**早于**替换事件，且相对顺序按**表层顺序**而非数值 seq 顺序确定；
- `start_seq == end_seq` 表示替换单个节点；
- **0 号节点的 `system/message` 不可被替换覆盖**（折叠必须拒绝）；
  后续 system 节点是普通历史，可被压缩遮蔽；
- `assistant/message` **禁止**携带 `source_event_seqs`；
- `replace_generation` 用于区分「尾部增长」与「重写」，增量消费方靠它判断。

### 工具行配对

工具行由两个来源拼成，**不新增数据结构**：

1. assistant 消息 `content` 里的 `tool-call` 块 → `{ id, name, arguments }`；
2. 表层 `tool/result` 节点 → `message.toolCallId` 匹配，`is_error` 取状态；
3. 耗时 = `tool/result.time` − `tool_calls[id].time`（层 2 的小索引）。

配不上对的情况（结果先于调用到达、或调用已被压缩遮蔽）**必须**降级为
只有结果的独立行，不得丢弃。

## 内存策略

north star 是 RAM。模型层据此设计：

| 策略 | 做法 | 收益 |
| --- | --- | --- |
| 日志专用事件不常驻 | 观察后丢弃，只留 `tool/call` 的时间索引 | 63 类事件只常驻 5 类 |
| 载荷惰性解析 | surface 节点存原始 JSON，渲染时解析可见行 | 不可见行零解析成本 |
| 视窗化 | `session/page` 反向分页（`beforeSeq`/`maxMessages`/`turnWindow`），只保留视窗+预取 | 长会话不线性涨 |
| 投影单份 | 不复制 host 已折叠的状态，直接绑定投影值 | 零重复状态 |

打开窗口用 `maxMessages: 40`（`session/follow`），历史按需向上翻页。

## 协议一致性检查清单

写/改 `dsh-protocol` 时逐条核对：

- [ ] 三处开放集合都没有 `deny_unknown_fields`；
- [ ] 未知 `type` 且无 `ignorable` → 明确错误，不是静默跳过；
- [ ] `surface_op` 的裸字符串 `"append"` 能正确反序列化；
- [ ] 非 surface 事件携带 `surface_op` 时判定为违例；
- [ ] replace 端点按表层顺序解析，且拒绝覆盖 0 号 system 节点；
- [ ] `replace_generation` 在每次 replace 后递增；
- [ ] 流式 `index` / `revision` 不连续时丢弃累积重同步；
- [ ] `end{committed}` 后用持久消息替换流式累积；
- [ ] 投影键缺失与 `null` 区分处理；
- [ ] effort ID 精确匹配，无降级路径；
- [ ] 未知错误码保留原文；
- [ ] 工具行配对失败时降级不丢弃。

## 待验证项（M0 探针）

以下未经 wire 验证，实现前必须实测确认：

| 项 | 待确认内容 | 影响 |
| --- | --- | --- |
| 插件消息投影 | `@messageProjection`（如 image offload 改写消息）是否需客户端复刻，还是 host 已投递投影后消息。**MVP 按「不复刻」实现**，作为已知缺口记录 | 带图片的长会话显示 |
| 反向分页行为 | `session/page` 在 `turnWindow` 与 `maxMessages` 同时给出时的取整规则 | 视窗预取量 |
| 权限目录刷新时机 | `permissionPresets/catalog` 无流式推送，需确定重拉触发点（会话切换 / 连接恢复 / 定时） | 插件热装后 mode 选项陈旧 |
| `contextPressure` 形状 | 实测静止态为 `{}`；是否在中途产出「已用/上限」，以及字段名 | ctx 圆环的分母（DECISIONS #21） |
| 进行中的流式基线 | 只采到静止态 `{"revision":0}`。`activeAttempt.stream` 里的元素源码标注为 "compact detached stream"，是否与线上 chunk 逐字同形未实测 | 断线重连恢复半截回答 |

### 已由源码核实（不需再探）

以下四项已核实（源码逐行 / 真实流量实测），**不再列为待验证**：

| 项 | 结论 | 证据 |
| --- | --- | --- |
| 官方模型是否免 profile 配置 | **是**，但有条件：凭据必须挂在 ref `DEEPSEEK_API_KEY`（或账号登录 token）名下 | `llm-deepseek-api-key/src/index.ts:77-80,101-115`、`llm-deepseek/src/credential.ts:14-20` |
| `permissionPresets/catalog` 端点名 | **成立**：namespace 来自 `super(ctx,'permissionPresets')`，方法名来自 `@Remote('catalog')` | `permission-presets/src/index.ts:205,292-293`、`typert/protocol/src/index.ts:240,320-326` |
| `--from-default-profile` 在 boot 路径创建 profile | **成立**，且是**无条件 replace**（目录已存在也重做） | `apps/cli/src/profile-boot.ts:137,194,230,282-306` |
| `assistant-stream` 帧形态 | **已实测**：`{type:'assistant-stream',frame:{type:start\|chunk\|end,…}}`，外包一层 Gateway `item` 帧 | `crates/dsh-protocol/fixtures/follow-stream.jsonl` |
| 流式基线形状 | **`{revision, activeAttempt?: {attemptId, startedAfterSeq, turn, step, nextIndex, stream[]}}`**；静止态是 `{revision:0}`（真实快照） | `dsh-api-session-controller/src/types.ts:501-517` |
