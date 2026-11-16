# PROTOCOL — dsh 协议面笔记

调研所得的 dsh 对外协议现状。**这是设计笔记，非官方文档**；
wire 细节以后续对 `deepseek-harness` 源码的核对为准（版本：0.2.x 系，2026-11 查证）。

dsh 对客户端暴露两套协议：

| 协议 | 启动方式 | 文档化程度 | 本项目用法 |
| --- | --- | --- | --- |
| HTTP / WebSocket host API | host 常驻（Web/dash 同款） | 低（上游内部 API） | **第一传输层** |
| SDK JSON-RPC 2.0（NDJSON over stdio） | `dsh --profile sdk` | 高（官方 wire 文档） | 备选 / 引导期使用 |

另有 ACP（Agent Client Protocol，JSON-RPC stdio）可作第三入口，暂不使用。

## 启动与握手（客户端接入 host）

### 启动

```text
dsh --profile dsh-client-rs --from-default-profile web --no-open --port 0
```

首次运行：`--from-default-profile web` 使 `prepareProfile` 用 web 模板
（`['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']`）初始化 profile，
**同一次调用即完成初始化与启动**。profile 已存在时**去掉该 flag**——
它是 replace 语义，重复带会抹掉用户配置。

`web` 模板是唯一提供 HTTP/WS host API 的模板（`session/*`、`workspace/*`
均在此）。

**preflight**：读 `$DSH_HOME/profiles/dsh-client-rs/package.json`，
确认 `dsh.profile.bundles` 含 `@deepseek-ai/dsh-web-app`。
`dsh plugin --profile X ...` 会用 base-only 的 `DEFAULT_PROFILE_BUNDLES`
静默初始化，那种 profile 没有 host API。

`--port 0` 让 OS 分配端口，避开 3080 冲突。

其他 flags：`--host <literal>`（只能一个具体本机地址，通配地址被拒）、
`--port N`、`--public-url <url>`（公告根，不授信）、`--trusted-host`、
`--tls-cert` / `--tls-key`（必须成对）、`--no-open`。

### 打印的入口 URL

就绪后 WebApp 按 `label: url` 逐行打印（`printUrl` 配置，默认 true）：

```text
dsh web: http://127.0.0.1:54321/?token=<process-token>
```

该 URL 本身就是凭据，**只能给预期用户看**；它不会出现在模型上下文或
`DSH_WEB_URL` 中（后者是不带凭据的形式）。

### token → 签名 cookie

```text
GET <url>                     → 303 + Set-Cookie: dsh-auth-<id>=<signed>…
                                Location: ./
                                Max-Age=<30天>; Path=/; Expires=…; HttpOnly; SameSite=Strict
                                （TLS 时再加 Secure）
POST /api/<ns>/<method>       → 带 Cookie
WS   ws://<host>:<port>/…    → 同一 Cookie
```

实现要点：

- **不要跟随重定向**，只需要 `Set-Cookie` 的第一对 `name=value`；
- 状态码是 **303 See Other**（不是 302），`Location` 是相对路径 `./`；
  我们只读响应头，所以对状态码宽容（非 2xx 也可，只要有 `Set-Cookie`）更稳；
- 401 = host 拒绝 token；无 `Set-Cookie` = 握手失败；
- cookie 名是 `dsh-auth-<base64url(SHA-256(authority))>`，而 authority 在非 TLS 时
  取 `url.host`——**含端口**。所以不同端口的实例天然得到不同 cookie 名，不会互相覆盖。
  客户端仍按实例隔离保存（不依赖默认 cookie 存储），但理由是"不污染、好清理"，
  不是"会覆盖"。

#### 两条交换路径：浏览器会话 与 API operator

| 路径 | 请求 | 响应 | cookie payload |
| --- | --- | --- | --- |
| 浏览器 | `GET /?token=…` | **303**，`Location: ./` | `version: 1` |
| API 客户端 | `GET /connect?token=…[&interactions=answer\|none]` | **302**，`Location: ./api/remote.mux` | `version: 2`，多一个 `id` |

两者都带 `cache-control: no-store`（`/connect` 另加 `referrer-policy: no-referrer`）。

**本项目新启动的 host 走 `/connect`**，三条理由：

- v2 payload 带 `id` = 独立的 API operator。`authenticateApi` **只认 v2**，
  所以浏览器 cookie 不能当 API 凭据用，身份也不与浏览器会话混在一起。
- `interactions` 决定这个 peer 是否参与**转发的人类交互**——审批请求与 user question
  都走这条通道。**默认 `none`，即收不到审批；我们要处理审批必须传 `interactions=answer`。**
- `Location` 指向的 `./api/remote.mux` **不要跟随**：段内带点，对我们是 404。

实测 payload（`id` 由 host 生成，此处脱敏）：

```json
{"version": 2, "authority": "127.0.0.1:56416",
 "id": "83070429-…", "issuedAt": 1794674973793, "expiresAt": 1797266973793}
```

`authority` 含端口，所以不同端口的实例 cookie 名天然不同。

#### 第三条路径：自签 v1 cookie attach（host 侧事实；客户端 #41 起不再使用）

> **客户端不再使用**：#41 对齐官方 desktop，永远 own host。以下是 host 协议
> 事实笔记（#40 曾据此实现 attach，后删除）。见 [DECISIONS #40](DECISIONS.md)
> 与 [DECISIONS #41](DECISIONS.md)。Token 是进程级、只打印在该进程 stdout 上，
别人开的 dsh web 我们拿不到；但 `$DSH_HOME/.credentials.yaml` 的
`client-connection/browser-session` 签名密钥是 home 级持久的。

```text
读 .credentials.yaml 的 32 字节 secret
  → 签 v1.<base64url(body)>.<base64url(HMAC-SHA256(secret, body))>
  → Cookie: dsh-auth-<base64url(sha256(authority))>=<上述值>
  → POST /api/session/list（只读探活，200 才算 attach 成功）
```

| 键 | 值 |
| --- | --- |
| `body` | `{"version":1,"authority","issuedAt","expiresAt"}`（整数毫秒） |
| `authority` | HTTP = `url.host`（含端口）；TLS = `url.origin`（含 scheme） |
| 有效期 | 官方 30 天；我们签 29 天，`issuedAt` 回退 1s |
| 身份 | `browserPeer`（`principal=owner`，`interactions=answer`），**不带 `id`** |

`decodeCookie` 对 `v1` 不要求 `id`；`authenticateApi` 才要求 v2。所以自签 v1
**不能也不需要**伪造 operator `id`——它落在 `browserPeer`，不在 `peers` 表里，
`admitConnection` 直接放行，因此不会撞 #35 的 409（一 peer 一 socket）。
签错密钥 = 401，正好当作「是不是同一个 `$DSH_HOME`」的校验。

发现端口的方式（#40 的实现，#41 已删）：`lsof` 取 loopback
LISTEN 端口 → 只留脚本运行时进程 → `GET /` 无 cookie 必须是 401 且带官方
`dsh web authentication required` 文案 → 带自签 cookie 的 `GET /` 必须是 200。
dsh 不把端口写进 `$DSH_HOME`，所以没有别的权威来源。

**401 自愈**：`Connection::call()` 遇 401 会 `refresh()` 用同一 token 重握手一次；
若仍 401，说明 host 重启过（token 与密钥都换了），
报 `HostError::Unauthorized`，不静默重试。

### 端点准入

Web 的 `/api` 有浏览器信任栅栏：除绑定 IP 本身外，其他 authority 需要
`--trusted-host`。客户端直接访问绑定地址（默认 loopback）即可，不需要额外配置。

### 上游 Electron 的替代路径（本项目不用）

Electron 走 `@deepseek-ai/dsh-desktop-host` + Node 子进程 IPC（fd 4）：

```text
host → parent : { type: 'starting', runId }
parent → host : { type: 'desktop-auth', secret }
host → parent : { type: 'ready', url, injections? }
                { type: 'shutdown-complete' } | { type: 'fatal', message, diagnostic? }
```

凭据为 32 字节 base64url 随机值，cookie 名 `dsh-desktop-session`；
协议版本常量 `DESKTOP_HOST_PROTOCOL_VERSION = 5`。
**内部协议且包不随普通 npm 安装，本项目不采用**（理由见 DESIGN.md）。

## HTTP / WebSocket host API

来自上游 Electron desktop 与 rustdsh `apps/rdsh-desktop` 的实际调用面。

### Wire 封装（2026-11 实测，rc.3）

一次一元调用就是一个 HTTP 请求，**端点写在 URL 路径里**，不是 body 字段：

```text
POST /api/<namespace>/<method>
Content-Type: application/json
Cookie: dsh-auth-<hash>=v1.<payload>.<sig>

{"type":"client-request","rpcId":"<调用方生成>","method":"<namespace>/<method>","payload":{"args":{…}}}
```

响应：

```text
{"type":"server-response","rpcId":"<原样回显>","result":{"ok":true,"value":…}}
{"type":"server-response","rpcId":"…","result":{"ok":false,"error":{"code":"…","message":"…","details":{…}}}}
```

三个容易踩的点：

1. **`payload` 必须恰好含一个 `args` 对象**，否则 `gateway/internal`：
   `Remote payload must contain exactly one plain-object args field`。
2. **`args` 的键是 TypeScript 形参名**，不是位置参数。`session/list` 的形参叫
   `_request`，所以是 `{"args":{"_request":{}}}`；写成 `{"args":{}}` 会被拒，
   错误码 `gateway/arguments-invalid`，`details.endpoint` 指出是哪个端点。
3. `rpcId` 由调用方生成、响应原样回显，用来给并发请求配对。

端点段有校验（`ENDPOINT_SEGMENT_PATTERN`）：段内不允许 `.`，所以 `/api/remote.mux`
永远是 404——那是给浏览器 Web UI 的跳转落点，不是 RPC 端点。

未认证访问 `/api/*` 一律 **401**；`Content-Type` 不是 `application/json` 则 **415**。

### 已知端点

| 端点 | 方向 | 用途 |
| --- | --- | --- |
| `session/create` | 请求 | 新建会话 |
| `session/follow` | 订阅 | 助手流式输出（assistant-stream deltas） |
| `session/cancel` | 请求 | 停止生成 |
| `session/selectModel` | 请求 | 选择模型（写会话配置；持久真源是 `request/header` 请求头快照） |
| `session/modelCatalog` | 请求 | 模型目录（含 per-model effort 档位、默认选择、provider 级故障） |
| `session/projections` | 请求 | 会话投影快照（标题 / modelSelection / contextPressure / tokenUsage） |
| `session/list` | 请求 | 会话列表（形参名 `_request`） |
| `session/events` | 请求 | 反向分页历史。**`session/page` 已 deprecated**，两者请求与结果类型相同，新代码只用 `events` |
| `workspace/create` `delete` `rename` `insertBefore` | 请求 | 工作区（侧栏分组）增删改与重排 |
| `workspace/follow` | 订阅（流） | 工作区列表、顺序、归档、置顶的 baseline + 增量 |
| `permissionPresets/catalog` | 请求 | 权限 preset 目录（mode 下拉选项） |
| `$events` | 订阅（瀑布流） | 审批请求、user question 等事件 |
| `$events/result` | 请求 | 回应审批 |
| `userQuestions/answer` | 请求 | 回答用户提问 |

`session/cancel` 的形状（从 `dsh-api-session-controller` 的 zod schema 查到，已实测）：

```text
请求  {request: {sessionId}}
响应  {accepted: true}
```

语义是**只取消当前那一轮**，不清 inbox（排队中的消息）。客户端收到
`accepted:true` 不等于结束：真正的结束还是流里的 `turn/end` / `assistant-stream`
的 `end{committed}`，界面应等那一帧再回空闲（DECISIONS #37）。

### 已知数据面

| 数据面 | 端点 / 流 | 内容 |
| --- | --- | --- |
| 会话投影 | `session/projections`（快照）+ `session/follow` 增量 | 真实标题、modelSelection、contextPressure、tokenUsage 等 22 个开放键 |
| 模型目录 | `session/modelCatalog` | 模型选择器（含 effort 档位、默认选择、provider 级故障） |
| 权限目录 | `permissionPresets/catalog` | mode 下拉选项（进程级，不随会话） |
| **工作区注册表** | `workspace/follow`（流）+ 一元变更 | **侧栏分组、顺序、归档、置顶** |
| 首屏快照 | `session/follow` 的 `snapshot` 帧 | 单会话打开窗口（`maxMessages` 默认 40） |
| 会话列表 | `session/list` | 分组内会话行（带 `projections` 提示，`kind: cached\|sequenced`） |

**注意**：侧栏分组不来自会话投影，而来自工作区注册表（第五通道）。
存储位置与字段见 [MODEL.md](MODEL.md) 层 7。

### 一次对话的最小链路（2026-11 活流量实测）

四步，**顺序不能换**：先订阅再投递，否则丢掉开头几帧。

| 步骤 | 调用 | `args` 键与值 | 实测响应 |
| --- | --- | --- | --- |
| 1 | `POST /api/session/create` | `request` = `{cwd}` | `{sessionId, agentPreset:"standard"}` |
| 2 | WS `open` `/api/remote.mux` 的 `session/follow` | `request` = `{address:{kind:'session',sessionId}, assistantStream:true, activate:true}` | 一条 `snapshot`，随后 `assistant-stream` + `event` |
| 3 | `POST /api/session/prompt` | `request` = `{requestId, sessionId, mode:'queue', content:[{type:'text',text}]}` | `{accepted:true}` |
| 4 | 读流至 `end{committed}` | — | 9 帧 `assistant-stream` + 持久 `assistant/message` |

要点：

- `prompt(request, signal)` 的 `signal` 形参**不要传**，只给 `request`；传不传 host 都不认。
- `requestId` 由客户端生成（uuid），host 只回接受回执。
- 实测两字回答：`inputTokens=5678 / outputTokens=2`（全新会话四步链）；
  同一套 harness 在已预热上下文下可到 14.7k——**ctx 圆环分子只能是 `inputTokens`**。
- 端到端断言在 `crates/dsh-host/examples/follow.rs`：流式折叠出的文本必须与
  持久 `assistant/message` 的文本块**逐字相同**，否则 fail。

### 逻辑流帧形状（源码核实 + 实测）

上行（客户端 → host）：

| `type` | 字段 | 说明 |
| --- | --- | --- |
| `open` | `streamId`, `endpoint`, `payload` | `payload` 就是 `{args:{…}}`，与一元调用同形 |
| `item` | `streamId`, `value?` | 上行推流（本项目不用） |
| `end` | `streamId` | 上行半关闭 |
| `cancel` | `streamId` | 取消该流 |

下行（host → 客户端）：`item{streamId,value?}` / `error{streamId,error:{code,message,details}}` / `end{streamId}`。

`streamId` 由客户端生成，同一 WebSocket 上多路复用；实现里丢弃非本流 id 的帧
（`dsh-host/src/stream.rs`）。

### 客户端必须处理的交互语义

1. **审批（approval）**：agent 工具调用前可能挂起等待人；回应 `allowed-once` / `rejected` / `cancelled` 语义（SDK 协议同款词汇）；
2. **user question**：宿主可取消提问（`cancelled`），迟到的回应应被忽略；
3. **流式顺序**：命令/生命周期通知先于响应到达；同一传输上保序；
4. **断线**：重连后从投影/快照恢复，不假设事件流可补发。

### statusline（composer 控制条）数据源

控制条三块数据（左 mode、右 model | effort | ctx 圆环）没有单一接口，由三个
RPC/投影面拼成：`session/modelCatalog`（目录）、`permissionPresets/catalog`
（权限目录）、会话投影（当前值 + 实时增量）。以下结构已核对 dsh 源码
（master，2026-11）：`packages/api/session-controller/src/catalog.ts`、
`packages/llm/token-meter/src/projection.ts`、
`packages/interaction/permission-presets/src/{types,index}.ts`。

**model + effort —— `session/modelCatalog`（读）**，同一次返回：

```jsonc
{
  "default": { "provider": "deepseek", "model": "deepseek-v4-flash" },
  "routableProviders": ["deepseek"],
  "groups": [{
    "id": "deepseek",
    "name": "DeepSeek",
    "models": [{
      "id": "deepseek-v4-pro",
      "name": "DeepSeek-V4-Pro",
      "description": "…",
      "badge": "…",
      "reasoning": {
        "efforts": [{ "id": "low", "name": "Low" }, { "id": "high", "name": "High" }],
        "defaultEffort": "high"
      }
    }]
  }],
  "failures": [{ "id": "…", "name": "…", "message": "provider 级隔离故障" }]
}
```

目录语义（dsh 决策笔记 `2026-07-15-llm-model-catalog-and-acp-selection`）：

- 目录是**建议性**的：缺失 = 未展示，不是无效；未列出的 model ID 原样透传，
  真正校验属于被选中的适配器；
- `default` 是部署默认（session 未选时的初始值，正好填控制条初始态）；
- provider 级故障隔离进 `failures`，不拖垮整个目录。

effort 语义（`2026-07-24-adapter-owned-reasoning-effort-capabilities`）：

- `efforts[].id` 是适配器持有的不透明 ID；DeepSeek 原生适配器声明
  `off` / `low` / `high` / `max`，默认 `high`（`thinking: disabled` 部署只声明 `off`）；
- 客户端必须精确匹配列表中的 ID，**禁止自动降级或别名**；不支持的档位在
  provider I/O 前报 `UNSUPPORTED_REASONING_EFFORT`。

model 选择的持久真源是 `request/header`（最后一个已消费的请求头快照）：前端
由此初始化选择，回退到 catalog 的 `default`；从未被实际使用的 UI 选择只留
内存，不产生持久事实。写路径 `session/selectModel` 更新会话配置。

**ctx 圆环 —— 会话投影**（`session/projections` 快照 + `session/follow` 增量，
字段注册于 `packages/llm/token-meter/src/projection.ts`）：

```jsonc
{
  "contextPressure": {
    "pressureTokens": 42000,   // 上一发请求的 provider 实测 prompt 体积（不含 output）
    "projectedTokens": 45300,  // 下一发请求将花的量：pressureTokens + 表层增量重定价
    "contextWindow": 128000    // 最近路由的容量（adapter resolveModel 提供，可缺失）
  },
  "tokenUsage": {              // 全会话日志累计，四桶互斥（reasoning 计入 outputTokens）
    "uncachedInputTokens": 38000,
    "outputTokens": 4200,
    "cacheReadTokens": 120000,
    "cacheWriteTokens": 8000
  },
  "contextBreakdown": { "systemTokens": 3000, "toolsTokens": 900, "messageTokens": 39000 }
}
```

投影语义（源码注释归纳）：

- 圆环用 **`projectedTokens / contextWindow`**：`pressureTokens` 是上一发采样，
  streaming 期间不动、不感知 compaction；`projectedTokens` 锚定 provider 实测
  加增量重定价，compaction 落盘即反应；
- 字段刻意**非原子**（last-wins、不同时刻）：换模型瞬间可能是「新容量 + 旧
  压力」。这是用户参考值，不是计费或闸门输入，UI 接受瞬态错位；
- `contextBreakdown` 是下一请求组成的启发式估算（系统提示词 / 工具 schema /
  消息），不与 `projectedTokens` 对账，只做组成展示。

**mode —— 服务端有状态，走投影 + 权限目录。**

`permissions` 投影（`packages/interaction/permission-presets/src/types.ts`）：

```jsonc
{ "permissions": { "currentValue": "workspace-write" } }
```

`currentValue` 是已配置 preset key、`auto`（会话级临时档），或派生的 `custom`
（当前 sandbox × approval 组合不匹配任何 preset）。该投影由三个**整值旋钮事件**
折叠而来：`permission/preset`、`sandbox/mode`、`approval/policy`。

可选项来自进程级目录端点 `permissionPresets/catalog`（`@Remote('catalog')`）：

实测响应（`crates/dsh-protocol/fixtures/permission-catalog.json`，rc.3）：

```json
{"options": [
   {"value": "read-only", "name": "read-only"},
   {"value": "workspace-write", "name": "workspace-write"},
   {"value": "danger-full-access", "name": "danger-full-access"}],
 "defaultOptions": [ /* 同上三项 */ ],
 "defaultPreset": "workspace-write"}
```

注意两点：**出厂是三个预设，不是两个**（`read-only` 也在，且同样出现在
`defaultOptions` 里）；这个构建的响应里**没有 `description` 字段**，
所以 UI 不能假定每个 option 带说明文案。

硬规则（源码原话）：**`permissions` 键缺失表示未组合权限服务，客户端必须隐藏
该控件**。不得假定总有 mode 可选。

语义：preset 是 `sandbox × approval` 的组合名。默认目录含三项：
`read-only`、`workspace-write`（sandbox=workspace-write, approval=ask）、
`danger-full-access`（sandbox=danger-full-access, approval=never），
`defaultPreset` 是 **`workspace-write`**。

写路径：**没有专用 Remote**。唯一写入口是 slash 命令 `/permission <preset>`，
通过 `session/prompt` 提交（源码原话：「the one write path a web client uses …
submits the picked preset as this line」）。handler 内部 append
`permission/preset` 事件，因而跨重启保留。

```text
用户选 preset → session/prompt { content: [{type:'text', text:'/permission workspace-write'}] }
              → host append permission/preset 事件
              → permissions 投影变更 → 客户端收到 projection 帧
```

实现要点：mode 下拉的选项与当前值都来自服务端，客户端不得自行缓存为权威。

来源：dsh 仓库（master）`packages/api/session-controller/src/catalog.ts`、
`packages/llm/token-meter/src/projection.ts`、
`packages/interaction/permission-presets/src/{types,index}.ts`，及
`.agents/notes/archived/architecture/`
下 `2026-07-15-llm-model-catalog-and-acp-selection.zh.md`、
`2026-07-24-adapter-owned-reasoning-effort-capabilities.zh.md`、
`2026-07-15-replay-token-meter-service.zh.md`、
`2026-07-10-after-call-compaction-pressure-and-overflow-recovery.zh.md`；
wire 旁证 rustdsh `apps/rdsh-desktop/src/{api.rs,app.rs}`。端点名均已代码搜索核实。

完整领域类型与折叠规则见 [MODEL.md](MODEL.md)。

## 会话列表与历史回放（实测形状）

**`session/list` 的形参名是 `_request`**，不是 `request`：传 `request` 会得到
`gateway/arguments-invalid: missing "_request"`。这是本项目第二次踩同一个坑
（另一次是 `_request` 反过来的 `session/prompt`）。

一条列表项（省略无关字段）：

```json
{
  "sessionId": "session-…",
  "cwd": "/Users/…",
  "updatedAt": 1794680338274,
  "running": false,
  "blank": false,
  "projections": {
    "kind": "sequenced",
    "asOfSeq": 25,
    "values": { "title": "收到确认回复", "workingDirectory": "/Users/…", "…": "…" }
  }
}
```

标题在 `projections.values.title`（空白会话为 `null`）；侧栏分组用
`values.workingDirectory`，回落 `cwd`。这两处访问器落在
`dsh_protocol::session::SessionSummary::{title, working_directory}`。

**历史回放**不需要额外端点：`session/follow` 打开时的第一帧就是 `snapshot`，
带 `records`（`maxMessages` 可控制窗口大小，客户端传 60）。每条记录形如：

```json
{ "type": "event", "event": { "type": "user/message", "seq": 8, "time": …, "surfaceOp": "append", "data": { … } } }
```

正文位置按事件类型分叉，**两者不同**：

| 事件 | 正文路径 | 过滤 |
| --- | --- | --- |
| `user/message` | `data.content[]` 里 `type == "text"` 的 `text` | 只留 `data.source.kind == "user"`；其余是 harness 注入的 system-reminder / 指令基线 |
| `assistant/message` | `data.message.content[]` 里 `type == "text"` 的 `text` | `reasoning` 块不显示 |

不做这个过滤，一次对话里会冒出三条「用户消息」，其中两条是长达数千字的
system-reminder。

## SDK JSON-RPC 2.0（备选传输）

`dsh --profile sdk` 启动 `dsh-sdk-jsonrpc-server`，NDJSON（每行一条 JSON-RPC 2.0）over stdio。
TS/Python 官方客户端已有；Rust wire 端可按官方协议文档自实现。

### 方法表（查证于 packages/sdk/protocol/README）

| 方向 | 方法 | 用途 |
| --- | --- | --- |
| client→server | `initialize` | 握手（能力协商、模型路由 reasoningEffort、maxTokens） |
| client→server | `session/prompt` | 发消息（文字 + 图片 content blocks） |
| client→server | `session/query` | 发消息并等待命令结算（支持 slash 命令） |
| client→server | `session/resume` | 恢复持久化会话 |
| client→server | `session/wait` | 等待会话空闲 |
| client→server | `session/working-directory/get` / `set` | 工作目录 |
| client→server | `shutdown` | 关闭 |
| server→client | `approval/request` | 工具审批（`allowed-once` / `rejected` / `cancelled`） |
| server→client | `user-question/ask` | 向界面提问 |
| server→client | `user-question.cancelled` | 提问取消通知 |
| server→client | `session.event` | 全部会话事件流 |
| server→client | `session.status` | `running` / `idle` 状态翻转 |
| server→client | `session.query.completed` | 命令查询结束 |
| server→client | `subagent.started` / `subagent.finished` | 子代理生命周期 |

握手注意：`initialize` 期间 host 解析模型路由，失败则拒绝后续 `session/prompt`
（模型不可用无回退）；`approvalRequests: true` 才开启 `approval/request`。

## UiBlock 通道（本项目定义）

复用工具结果 / `$events` 通道承载，schema 对齐 `dsh-intelligent-ui` 元素目录
（表 / 图表 / 对比 / 步骤 / 输入框等）。详细 schema 见 DESIGN.md，实现期单独成文。

## 版本兼容策略

- 客户端启动时探测 host 版本，落在支持矩阵内才继续（参照 rustdsh 对
  0.2.0-rc.2 / 0.2.1-alpha.1 / 0.2.1-alpha.3 的识别方式）；
- 协议回归靠 headless dump（`--dump-ui` / `--probe` 模式），CI 常跑；
- host API 变化预期会在 `dsh-protocol` 单点吸收。

## 模型选择（实测形状）

```text
session/modelCatalog   args {}                      → {default, routableProviders, groups[], failures[]}
session/selectModel    args {request:{sessionId, provider, model, reasoningEffort?}}
                                                    → {selected:{provider, model, reasoningEffort?}}
```

当前值不在 `session/state` 里，而在 `session/list` 的投影 `modelSelection`：

```json
{ "lastUsed": { "provider": "deepseek-official", "model": "deepseek-flash" },
  "next":     { "provider": "deepseek-official", "model": "deepseek-v4-pro", "reasoningEffort": "high" } }
```

`next` 是已选、下一轮生效；`lastUsed` 是上一轮实际用的。强度 id 不透明
（`off`/`low`/`high`/`max` 由适配器声明，不得自行合成），默认档在
`groups[].models[].reasoning.defaultEffort`。

## 上下文占用（实测形状）

`contextPressure` 与 `contextBreakdown` 都标了 `SessionProjectionSummaryExcluded`，
**不在** `session/list` 的投影摘要里，必须单独读：

```text
session/projections   args {request:{sessionId}}   → {kind:"sequenced", asOfSeq, values:{…}}
```

```json
{"contextPressure":{"contextWindow":1048576,"pressureTokens":14731,"projectedTokens":15218},
 "contextBreakdown":{"messageTokens":5521,"systemTokens":1541,"toolsTokens":7740}}
```

`pressureTokens` = 最近一次请求上报的提示长度（未命中输入 + 缓存读 + 缓存写）；
`projectedTokens` = 下一次请求的预估值；`contextWindow` = 最近一次记录的模型容量。

## 工作区注册（实测形状）

```text
workspace/create   args {request:{path}}  → {workspace:{...}, created:bool}
directoryPicker/pick   args {}            → path | null（host 本机弹系统框）
```

```json
{"workspace":{"workspaceId":"…","path":"/tmp/dsh-ws-demo","title":"dsh-ws-demo",
  "sessionIds":["…"],"createdAt":"2026-11-15T…","updatedAt":"…"},"created":true}
```

要点：

- 形参键 `request`（与 `session/list` 的 `_request` 相反，同命名空间里并存）；
- 只接受**已存在**的目录，幂等——重复注册返回同一 workspace 且 `created=false`；
- 注册不等于建会话，侧栏要看见还得 `session/create`；
- `directoryPicker/pick` 在 **host 机器**上弹框，`cancellable`；取消回 `null`。
  同机客户端用本地原生框即可，不必绕这一跳。

## 补全数据源（实测形状）

```text
commands/list            args {agentId}                        → [{definitionId?, name, description?, input?{hint?,attachments?}}]
workspaceFiles/list      args {workspaceFileScopeId, path}     → {path, entries:[{name,type,size?}], truncated}
workspaceFiles/search    args {workspaceFileScopeId, request}  → {matches:[{path,type}], truncated, incomplete}
```

```json
{"matches":[{"path":"Documents/动漫201 2020042104陈润龙.docx","type":"file"},
            {"path":"Library/Application Support/CloudDocs","type":"directory"}],
 "truncated":false,"incomplete":true}
```

要点：

- `commands/list` 的形参键是 **`agentId`**（scope 的 wire 名）；返回的 `name`
  **不带**前导斜杠；`input.hint` 是给补全行用的参数提示；
- `workspaceFiles/search` 的作用域形参叫 **`workspaceFileScopeId`**，值是 SessionId；
  `request = {query, excludedPaths, includeHidden, respectGitignore}`，`query` 为空（或纯空白）
  会被拒：`gateway/bad-request: query must not be blank`；含 `/` 时匹配工作区相对路径；
- 目录是 host 从命中文件的祖先**推断**出来的，`type` 给 `file` / `directory`；
- 结果按深度再按路径排序、有上限；`incomplete=true` 表示文件集本身被截断（不是错误）。

`workspaceFiles/list` 是另一个接口，**列一个目录的直接子项**（`@` 补全的空查询用）：

```json
{"path":"crates","entries":[{"name":"dsh-client-ui","type":"directory"},
                            {"name":"dsh-protocol","type":"directory"}],"truncated":false}
```

要点：

- 形参只有两个，**`path` 是平铺的**（不是套在 `request` 里）：
  `{workspaceFileScopeId, path}`；套成 `request` 会回
  `gateway/arguments-invalid: … missing "path"; unexpected "request"`；
- `path` **必填**，空串被拒（`gateway/bad-request: path is required`）；根目录要写 `.`，
  写 `/` 回 `workspace-file/outside-workspace`；工作区内的绝对路径也可以（结果 `path` 仍是相对值）；
- 结果的 `path` 是**被枚举的目录**（根目录时空串），子项路径 = `path` + `/` + `name`；
- 子项按主机名字序，**不排除**隐藏项与 `node_modules`（那两条是客户端按官方清单过滤的）；
- `type` 可能是 `other`（既不是普通文件也不是目录），补全里直接丢掉。

## 工作区注册表（实测形状）

```text
workspace/create   args {request:{path}}                → {workspace, created}
workspace/delete   args {request:{workspaceId}}         → {deleted:true}
workspace/follow   args {}                              → 基线 + 增量帧（WS）
```

```json
{"type":"baseline","value":{"items":[{"workspaceId":"…","path":"/private/tmp/dsh-ws-demo",
  "title":"dsh-ws-demo","sessionIds":[],"createdAt":"2026-11-14T20:20:32.559Z",
  "updatedAt":"2026-11-14T20:20:32.559Z"}],"archivedSessionIds":[],"pinnedSessionIds":[]}}
{"type":"upsert","workspace":{…同一个 WorkspaceView…}}
{"type":"order","workspaceIds":["w2","w1"]}
{"type":"remove","workspaceId":"w2"}
```

要点：

- **一个 peer 只准一条 WebSocket**。Gateway 的 `admitConnection` 在
  `record.ownsSocket` 为真时直接回 `409 Conflict`，所以所有逻辑流共用一个 socket
  （DECISIONS #35）；
- `delete` 只删注册：目录与会话都保留，会话重新落到「未分组」；
- 删除时先来 `order`（不带被删 id）再来 `remove`，两条都要处理；
- `path` 是 host 侧**规范化**过的（`/tmp/x` → `/private/tmp/x`），客户端比对要先抹平
  `/private`。

## 会话归档（实测形状）

```text
workspace/archiveSession   args {request:{sessionId, stopActivity?}} → {archivedSessionIds}
workspace/unarchiveSession args {request:{sessionId}}                → {archivedSessionIds}
```

```json
{"archivedSessionIds":["session-5e4c48a7-…","session-155e3e98-…"]}
{"type":"archived","archivedSessionIds":["session-155e3e98-…"]}
{"type":"pinned","pinnedSessionIds":["session-…"]}
```

要点：

- **没有删会话的接口**。`workspace/*` 里只有归档/取消归档、置顶/取消置顶、
  `insertSessionBefore`、`insertBefore`、`rename`；归档后 `session/list` **照样列出**
  该会话（记录与目录都在），只是不再进分组界面；
- 两个接口回的都是**整份**归档集合，直接当基线用；`workspace/follow` 的
  `archived` / `pinned` 帧也是整份集合，不是增量；
- `stopActivity: true` 才会停掉跑着的那一轮；缺省时 host 用
  `workspace/session-active` 拒掉归档；
- **自己发起的归档不会变成 follow 增量帧**（follow 只推别人的变更），所以回执
  必须由调用方写回本地注册表，否则下一条无关帧会用旧集合整份覆盖
  （DECISIONS #36）。
