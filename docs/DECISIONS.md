# DECISIONS — dsh-client-rs

关键决策记录。每条含：决策、依据、否决项。

---

## #1 UI 技术栈：Rust → GPUI 自绘

**日期**：2026-11（立项）

**决策**：窗口进程纯 Rust，GPUI 自绘渲染。不引入任何脚本运行时。

**依据**：

- 项目初衷是 desktop RAM 极致；
- 实测 JS 引擎税：`node -e ''` 峰值 RSS ≈ 44MB（未跑应用代码）；常驻 TS 应用普遍 40-80MB 堆；
- 实测参照：pi-gpui（Rust+GPUI 完整 agent GUI）空闲 75MB、安装包 16MB、启动 0.4s；Electron 客户端 190-430MB；
- GPUI 生态成熟：Zed（95k⭐）生产验证，gpui-agent 提供无头 UI 自动化。

**否决项**：

| 方案 | 否决理由 |
| --- | --- |
| Electron / Tauri(WebView) | Chromium/WebView 引擎 200-400MB 级 |
| TS → GPU 自绘（GPUIX） | 渲染轻了，但 JS 引擎堆 40-80MB 常驻，违背初衷（曾认真考虑，最终放弃） |
| 原生控件（nsh / NativeScript 路线） | 编码代理界面主体是 diff / 流式 markdown / 暗色主题，全是非标控件，原生控件画不动 |
| Rune | 能力契合（headless 测试），但 pre-release 且锁 git rev，稳定性不如 GPUI |
| nsh（Native Shell） | 其全部卖点与本项目约束相反：TSX 界面 = 嵌 JS 引擎（违反禁止项）；主路线原生控件画不动 diff/markdown 非标控件；六端抽象（iOS/Android/Web/TUI）全在非目标内。若要用其 GPUI 后端 + 纯 Rust 写界面，等于隔抽象层用 GPUI，徒增复杂度。唯一回看条件：未来移动端升为一等目标 |

---

## #2 只做 desktop 表面，不做 Web

**日期**：2026-11（立项）

**决策**：不实现任何 Web UI。dsh 本身启动时即带 Web 服务，Web 表面由上游负责。

**依据**：Web 皮肤/插件生态已饱和（dsh-client-ui-skin-* 等大量社区插件）；重复实现无价值。
桌面窗口是唯一空白面（上游 Electron desktop 可被 GPUI 原生替代）。

---

## #3 协议面：HTTP/WS host API 优先，传输层可插拔

**日期**：2026-11（立项）

**决策**：第一传输层用 dsh 的 HTTP/WebSocket host API（上游 Electron app 同款）。
协议类型收拢在 `dsh-protocol` crate。传输做成 trait，未来可加 SDK JSON-RPC 传输。

**依据**：

- rustdsh `apps/rdsh-desktop` 已验证此路线（同 API 的现成 Rust 实现可参考）；
- 该 API 特性最全：会话投影、真实标题、模型目录、流式 deltas、审批/提问事件；
- 上游 SDK（`dsh --profile sdk`）的 NDJSON JSON-RPC 2.0 是正式文档化的备选（方法表见 PROTOCOL.md）。

**风险**：HTTP/WS host API 是上游内部协议，无正式文档。
**对策**：版本钉死（参考 rustdsh 兼容矩阵的做法）；wire types 单点维护；headless dump 工具做协议回归。

---

## #4 动态界面内容：UiBlock 声明式 schema，数据驱动

**日期**：2026-11

**决策**：cordis 插件对 desktop 的界面自定义，一律通过结构化 UiBlock 数据表达；
GPUI 侧做通用渲染器。客户端不执行插件代码，不嵌脚本运行时。

**依据**：

- cordis 支持动态更新（HMR + plugin-manager + ctx.effect 可逆注册），但界面热替换只作用于 Web 插件树，GPUI 窗口拿不到；
- 上游 `dsh-intelligent-ui` 插件证明「render_ui 工具 + 元素目录 + 客户端渲染」模式可行；
- 数据驱动后，插件热装/热卸 = 新 UiBlock 类型出现在 timeline，客户端零改动。

**连带约束**：客户端必须假设工具目录与事件流是动态的（HMR 生效中）。
未知工具调用做通用降级渲染，不硬编码工具集合。

---

## #5 harness 外置：dsh 作为独立 sidecar

**日期**：2026-11（立项）

**决策**：不重写 agent 运行时。dsh（Node.js）作为外部 host 进程，spawn 后复用。
Node 的引擎税记在 harness 进程上，不进客户端内存预算。

**依据**：

- rustdsh 的 PHILOSOPHY 实证：「Rust reins on the Node.js titan」—— 只把热边界（启动、guard、hook）收紧，agent 循环留给上游，兼容性零漂移；
- 重写 harness = 永久跟踪上游变更；
- 用户换后端（未来接 codex 等 Rust harness）时协议层不变。

**不追求**：把 harness 内存也算进「客户端极致」。极致的边界是窗口进程。

---

## #6 UI 组件层：GPUI 之上的 gpui-kit（gpui-base 优先）

**日期**：2026-11

**决策**：框架仍为 GPUI（#1）；组件层引入 [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit)。
依赖单 crate `gpui-kit`（pin 并 re-export GPUI）；界面以 `gpui-base`（无样式行为层）
为基础、选择性使用 `gpui-component`；**禁用 `gpui-shell`**。

**依据**：

- 消除 M1/M3 已知痛点：原生 Markdown + 语法高亮、变高行虚拟列表、Dock 布局、
  headless UI 集成测试（框架级）、tree-sitter 代码编辑器；
- 纯 Rust，零引擎税，不违反 #1 禁止项（gpui-shell 为 opt-in JS 运行时，不引入）；
- 生产验证：Longbridge Pro 商业桌面在用，20.6k⭐，持续维护；
- 许可核实：代码 Apache-2.0（LICENSE-APACHE）；LICENSE-DOCS.md 仅覆盖文档散文（CC BY 4.0），不影响代码使用。

**否决项**：

| 方案 | 否决理由 |
| --- | --- |
| 裸 GPUI（rdsh-desktop/pi-gpui 路线） | markdown/虚拟化/测试基建全部自建，工程量回到全量估算的上限 |
| gpui-shell | JS 扩展运行时，违反 #1 窗口进程禁脚本运行时 |

**代价**：两个参照实现（pi-gpui、rdsh-desktop）均裸 GPUI，UI 层代码需适配；
协议层不受影响。gpui-kit pin GPUI 版本，升级需同步。

---

## #7 视觉语言对标 fx-ui

**日期**：2026-11

**决策**：视觉语言以 [fx-ui](https://github.com/zaidmukaddam/fx-ui) 为基准，
采用其 `src/ui/theme.ts` 原始 token。

| token | 值 | 用途 |
| --- | --- | --- |
| background | `#0f0f10` | 主体 |
| sidebar | `#0a0a0b` | 侧栏 |
| raised | `#161618` | 浮起面 |
| muted / selected | `#1c1c1f` / `#232326` | 次级面 / 选中 |
| border | `#27272a` | 发丝线 |
| text → ghost | `#ededed` / `#a1a1a6` / `#8c8c92` / `#68686e` / `#56565b` | 五级灰字阶 |
| danger | `#f36356` | 破坏性 |
| added | `#6cc58a` | diff 绿 |
| radius sm/md/lg | 4 / 6 / 8 | 圆角 |
| CONTENT_WIDTH | 860 | 内容列宽 |

**规则**：

- mono 贯穿全文；层级靠字色，不靠字号/背景块；
- 工具调用**一行平铺**：图标 + 动词 + 参数 + 右对齐耗时 + `›`；
- 卡片只给交互块：1px 描边 + 8px 圆角，不给普通消息加卡；
- 白色即主色，chrome 零彩色。

**否决项**：第一版「卡片堆 + 彩色徽章」风格（AI slop 特征：渐变、彩色 pill、
每块都套卡片）。

**注**：fx-ui 本身是 GPUIX 实现（同类竞品），只借鉴视觉语言，不复用其技术路线
（见 #1）。

---

## #8 领域模型：三条开放轴，禁止封闭枚举

**日期**：2026-11

**决策**：dsh 的事件 `type`（63）、投影 `key`（22）、内容块 `type`（7）
全部可合并扩展。三者一律按「已知成员 + 未知降级」建模：字符串标签 +
惰性 `JsonValue` 载荷，只对需要渲染的成员写类型化视图。

**依据**：`SessionEventMap` / `SessionProjectionMap` / `ContentBlockMap`
均为 declaration merging 开放；插件（cordis HMR 生效中）随时可能新增成员。

**硬规则**：未知事件 `type` 且无 `ignorable: true` 标记时，dsh 规定读取方
**必须拒绝重建会话**，不得静默丢弃 —— 未知必需事件可能改变其余日志的解释。

**否决项**：

| 方案 | 否决理由 |
| --- | --- |
| 封闭 Rust enum 穷举 63 类事件 | 插件扩展即解析失败；且与上游版本强耦合 |
| 未知一律跳过 | 违反协议硬规则，会让「被掏空的会话」继续运行 |

---

## #9 渲染表层（surface），不是事件日志

**日期**：2026-11

**决策**：时间线绑定 dsh 的 **surface** 派生视图，不是原始日志。表层折叠
（append / replace）实现在 `dsh-protocol`。

**依据**：只有 5 类是 surface 事件（`system/message` `developer/message`
`user/message` `assistant/message` `tool/result`）；`surfaceOp` 支持
`replace{startSeq, endSeq}`，compaction 用区间替换遮蔽历史，
`replaceGeneration` 递增用于区分尾部增长与重写。

**推翻的初始假设**：「MVP 先假设 append-only」——错误。compaction 从第一天就
存在，append-only 建模会在第一次压缩后显示错乱。

**附带收益**：日志专用事件（58 类）观察后即可丢弃，63 类只常驻 5 类，
直接服务 north star。

---

## #10 流式绑定 assistant-stream 帧，提交时替换

**日期**：2026-11

**决策**：实时增量走 `assistant-stream` 帧（`start` / `chunk` / `end`），
不走 session 事件。`end{committed, seq}` 到达后用该 seq 的持久
`assistant/message` 替换流式累积结果；`end{abandoned}` 标记中断并保留前缀。

**依据**：dsh 流式是进程内帧，`index` 稠密递增、`revision` 单调，
重连靠快照里的 `assistantStream` 基线恢复，不重放整轮。

**规则**：`index` 或 `revision` 不连续 → 丢弃累积并重新同步，不得拼接。

---

## #11 UiBlock 挂在 `tool/result.data.meta`，不自造通道

**日期**：2026-11

**决策**：插件驱动的界面内容使用 dsh 既有的 `tool/result.data.meta` 字段承载，
不新增传输通道。

**依据**：dsh 将该字段定义为工具私有展示载荷：对核心不透明、必须 JSON
可序列化、可从持久日志复现同一张卡片（`dsh-tool-fs` 的 result-time diff
即走此处）。语义与本项目 #4 的 UiBlock 需求完全重合。

**降级**：`meta` 缺失或 schema 不识别 → 渲染普通工具行，不报错、不丢弃。

---

## #12 mode 是服务端状态（纠正早期结论）

**日期**：2026-11

**决策**：statusline 左侧 mode 绑定 `permissions` 投影 +
`permissionPresets/catalog` 端点。写路径是 slash 命令
`/permission <preset>` 经 `session/prompt` 提交。

**纠正**：本文件早期及 PROTOCOL.md 曾断言「mode 无服务端状态，是客户端本地
审批策略」—— **错误**。dsh 有完整的权限 preset 系统：`permissions` 投影由
`permission/preset`、`sandbox/mode`、`approval/policy` 三个整值事件折叠而来，
`currentValue` 取 preset key / `auto` / `custom`。

**硬规则**：`permissions` 键缺失 = 未组合权限服务，**客户端必须隐藏该控件**
（dsh 源码明文规定）。

**否决项**：客户端自己维护 mode 状态并只做本地应答策略 —— 会与 host 的
sandbox/approval 实际配置脱钩，且跨设备/重启不一致。

---

## #13 host 启动：CLI `--profile web` + token 交换，不用 Electron IPC

**日期**：2026-11

> **注**：本条里的 profile 名 `web` 已被 #15 取代为 `dsh-client-rs`；
> 本条保留的是「用 CLI + token 交换、不用 Electron IPC」这个决策本身。

**决策**：客户端 spawn `dsh --profile web --no-open --port 0`，从 stdout 提取
带 token 的入口 URL，GET 换取签名 cookie，之后所有 HTTP/WS 请求带该 cookie。

**依据**：

- `web` 是唯一提供 HTTP/WS host API 的 profile（`session/*`、`workspace/*`）；
- `--port 0` 由 OS 分配空闲端口，避开 Web 默认 3080 与端口冲突；
- URL 打印是公开行为（`printUrl` 默认 true，格式 `label: url`），
  rustdsh 已验证这条链可行；
- 握手只需一次 GET + `Set-Cookie`，无额外协议层。

**提取策略**：宽容匹配首个 `https?://(loopback):\d+/?token=` 的 URL，
不依赖 label 文案（可随版本变）。

**否决项**：

| 方案 | 否决理由 |
| --- | --- |
| `@deepseek-ai/dsh-desktop-host` + Node IPC 握手 | 内部协议（v5）、包不随普通 npm 安装、Rust 需自实现 Node IPC 帧格式；唯一收益“凭据不走 stdout”可用“不打印子进程输出”等效解决 |
| 固定端口 3080 直连 | 与用户已开的 `dsh web` 冲突；且仍需 token |
| 嵌入 Web 前端（隐藏浏览器视图） | 直接违反 #1/#2 |

**安全约束**：token / cookie 不得进入日志、崩溃报告、遥测、UI；
诊断只输出不含凭据的 host:port。

**已知风险**：与用户已运行的 host 共享 `$DSH_HOME` 时会有会话写锁竞争
（`session/writer-held`）。对策：捕获该错误并提示，不自动重试；
长期方向 attach 模式已被 #41 否决并整体删除（共享身份与生命周期）。

---

## #14 profile 复用 `profiles/web`，不新建专用 profile

> **状态：已被 #15 取代。** 保留本条是为了记住当时判定的理由与盲点：
> 「新 profile 没模型且无处配置」这个结论过于悲观——漏掉了 host 自带 Web UI
> 可以直接当配置面板这一条。

**日期**：2026-11

**决策**：客户端复用现有 `profiles/web`，不创建 `profiles/dsh-client-rs` 之类的
专用 profile。profile 名可用环境变量覆盖（默认 `web`），但**禁止**使用保留名
`desktop`（CLI 拒绝启动该名）。

**依据**（存储分层，已核对 dsh 源码）：

| 数据 | 层级 | 新 profile 是否共享 |
| --- | --- | --- |
| `sessions/` | `$DSH_HOME` | 是 |
| `storages/`（含 workspace） | `$DSH_HOME` | 是 |
| `.credentials.yaml` | `$DSH_HOME` | 是 |
| `skills/` `AGENTS.md` `.env` | `$DSH_HOME` | 是 |
| `cordis.patch.yml`（home patch） | `$DSH_HOME` | 是 |
| `profiles/<name>/cordis.patch.yml` | profile | **否** |
| `profiles/<name>/package.json` | profile | **否** |

决定性事实：dsh Settings 插件把更改持久化到**当前 profile 的 Cordis patch**，
而 `llm-pi-ai` 的 provider profiles（模型路由 / API key 引用）正是 Web 的
Models 页面写进去的。新建 profile = 随附模板的干净副本 = **无任何 provider
配置 = 无可用模型**，且此后 web 与 desktop 的模型配置各自漂移。

次要：`web` 的 bundle 集正好是 wire 客户端所需（`session-controller`、
`workspace-controller`、`gateway`、websocket carrier）；Electron 的平台插件
（文件拖拽、更新、账号后端）我们不需要。故无自建 profile 的技术必要。

**否决项**：

| 方案 | 否决理由 |
| --- | --- |
| 新建专用 profile | 丢失 provider/settings 配置；且 `sessions/` 是 home 级，换不来任何数据隔离 |
| 从 `--from-default-profile web` 播种后与 web 分离 | 仍丢失用户已有的模型与插件配置，且从此双份维护 |
| 复用保留名 `desktop` | CLI **硬拦截**（`apps/cli/src/args.ts:84`）：`profile "desktop" is managed exclusively by the Electron application`，非零码退出；`toLowerCase()` 使 `Desktop`/`DESKTOP` 同样被拦。绕过只能不调 CLI 自启 runner，等于放弃公开接口 |

**与上游 Desktop 的对比（重要）**：上游 Electron 确实使用自己的
`profiles/desktop`，不跟随 web——但初始 bundle 列表抄自共享 Web 模板，
且"CLI 与 Desktop 共享 `$DSH_HOME` 下受支持的产品数据，但绝不共享可执行包、
插件激活、锁文件或 `node_modules`"。其分离动因是两条：

| 上游动因 | 是否适用于我们 |
| --- | --- |
| 共享可执行依赖图会让 CLI 与 Desktop 相互改变 dsh/Cordis/插件/原生模块版本 | **不适用**：上游内置签名运行时（`app.asar/dsh` + `desktop-packages/*.tgz`）必须冻结；我们 spawn 用户 PATH 上的 `dsh`，本就跟随用户版本，靠 `dsh --version` 矩阵校验 |
| 两个桌面进程争用同一 profile | 不适用：我们不持有单实例锁，且不做插件操作 |

**决定性的不对称**：上游有完整设置面板 + Welcome 引导，能在新 profile 里重新
配 provider；本项目 MVP 明确不做设置面板，新开 profile 会让用户面对
「有界面但无模型、且无处配置」的死路。

**重新评估条件**：当设置面板落地（M3 或提前），且能复刻 provider 配置与
Welcome 引导流程时，`profiles/desktop` 式隔离变得更可行且更干净。
但 #15 已决定不做引导，故重议条件不再成立，本条仅作历史记录。

**客户端对 profile 是只读的**：选模型→`session/selectModel`（会话配置）、
选 mode→`/permission` 会话事件、建工作区→`$DSH_HOME/storages`、
其余全只读。只有 dsh 的 Settings 表单写 profile 的 Cordis patch，
而 MVP 不做设置面板，**故复用 `profiles/web` 零污染**。

**逃生口**：环境变量覆盖 profile 名，用于确实需要隔离调试的场景。
启用时必须同时满足：provider 配置在 home patch
（`$DSH_HOME/cordis.patch.yml`，组合顺序最外层，所有 profile 继承），
否则客户端无可用模型；并需在 UI 中提示「模型需重新配置」。

## #15 采用专用 profile `dsh-client-rs`，不做引导

**日期**：2026-11

**决策**：默认 profile 为 **`dsh-client-rs`**，不用 `web`。

```text
dsh --profile dsh-client-rs --no-open --port 0
```

首次运行且 `$DSH_HOME/profiles/dsh-client-rs` 不存在时，**一条命令完成初始化与启动**：

```text
dsh --profile dsh-client-rs --from-default-profile web --no-open --port 0
```

`prepareProfile` 被 boot 与 dump 两条路径共用（`apps/cli/src/profile-boot.ts:194`），
故 `--from-default-profile` 在 boot 时同样生效，无需两阶段。

**dsh 没有 `init` 命令**。两条隐式入口的 bundle 集不同：

| 入口 | bundles | 可用性 |
| --- | --- | --- |
| `dsh plugin --profile X <pnpm 参数>` | `DEFAULT_PROFILE_BUNDLES` = `['@deepseek-ai/dsh-base']` | 不可用，无 host API |
| `dsh --profile X --from-default-profile web` | `['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']` | 采用 |

**preflight**：每次启动前读 `package.json`，确认 `dsh.profile.bundles` 含
`@deepseek-ai/dsh-web-app`；否则该 profile 是被 `dsh plugin` 静默初始化成
base-only 的，必须用 `--from-default-profile web` 重建。

**取代 #14 的关键事实**：`--from-default-profile web` 复制出的是
`['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app']`，这是**最小可用集的硬地板**——
HTTP/WS host API 全在 `dsh-web-app` 里（见 #16）。#14 担心的「没模型」问题
不靠界面解决，而靠「模型来自 `$DSH_HOME` 级凭据」绕过：客户端**不做引导**，打开即用。

**启动流程（无引导）**：

| 步骤 | 动作 |
| --- | --- |
| 1 | `package.json` 不存在 → 带 `--from-default-profile web` 启动，一步完成初始化 |
| 2 | 握手，拉 `session/modelCatalog` |
| 3 | 进主界面。catalog 为空时只禁用发送、显示「未配置模型」，不弹引导、不阻断 |

**命名依据**：

| 名字 | 判定 |
| --- | --- |
| `desktop` | 禁止。CLI 硬拦截 `toLowerCase() === 'desktop'`，Electron 独占 |
| `desktop-rs` / `desktop-*` | 禁止（自定）。CLI 能过，但与上游 `desktop` 语义混淆；上游若改前缀匹配即断 |
| `app` | 不推荐。太泛，与上游 `apps/` 概念混淆，未来撞名风险最高 |
| **`dsh-client-rs`** | 采用。与项目同名，自解释、零碰撞 |

另：`--from-default-profile` 拒绝将 shipped 名
（`web` / `headless` / `sdk` / `sdk-minimal` / `acp`，不区分大小写）当自定义目标名。

**安全约束**：

- **`--from-default-profile` 的闸门归客户端。** dsh 侧它是 **无条件 replace**：
  只要带上这个 flag，旧目录整体 rename 走、新目录就位后删除，目录已存在也照做
  （`apps/cli/src/profile-boot.ts:90-98, 137-152`），用户 patch 一起没。
  早期文档写的「只在目录不存在时执行」描述的是**客户端策略**，不是 dsh 行为。
  因此客户端**硬规则**：仅当 `package.json` 缺失时才把这个 flag 拼进命令行。
- 导入现有 web 配置前先比较两个 profile 的 `package.json` `dependencies`；
  依赖不一致时告警并备份。**源文件 `profiles/web` 只读，绝不修改。**
- 客户端自身仍不写 profile 配置；唯一写入是首次生成与用户可选的导入。

**代价（明确接受）**：

| 代价 | 说明 |
| --- | --- |
| 配置要与 web 各一份 | 首次需配置一次；提供一次性导入缓解 |
| 两 profile 从此漂移 | 在 web 改模型不会同步到客户端，反之亦然 |
| 多一份 node_modules 磁盘占用 | 由 dsh 的 profile 机制承担，不在客户端预算内 |

**降级开关**：环境变量可把 profile 名改回 `web`，供不愿多一份配置、
或已在 `$DSH_HOME/cordis.patch.yml` 放全局 provider 配置的用户使用。

## #16 `dsh-web-app` 不可省，但不使用其 Web UI

**日期**：2026-11

**决策**：profile 的 bundles 固定为
`['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app']`，**不做任何剪裁**；
同时客户端**永不开浏览器、不做设置界面**。

**依据**（逐行核对 `packages/bundle/web-app/cordis.patch.yml`）：

| 行 | 包 | 为什么不可省 |
| --- | --- | --- |
| `web-startup` | `@deepseek-ai/dsh-web-app/startup` | 提供 `webStartup`；`webserver` / `connection` / `web-runtime` 都 `inject: [webStartup]`。没有它不绑端口，`--port` `--host` `--no-open` 也不被识别 |
| `webserver` | `dsh-host-webserver` | 绑端口、路由、gzip |
| `connection` | `dsh-client-connection` | 双面：node 半侧把 gateway 挂到 `/api`，是我们全部 RPC 的入口 |
| `authentication-token` | `dsh-host-authentication-token` | token 鉴权 |
| `web-runtime` | `@deepseek-ai/dsh-web-app` | `printUrl: true`——带 token 的 URL 是它打印的，我们的握手全靠读它 |

**关键**：`web-startup` 与 `web-runtime` 是同一 package 的两个入口
（bundle 包同时是运行时插件），所以「只取 patch、不要运行时」不可行。

**其他 bundle 全部不行**：

| bundle | 传输 | 判定 |
| --- | --- | --- |
| `sdk-app` | stdio JSON-RPC（stdout 换行分帧） | ✗ |
| `sdk-minimal` | 同上，独立 patch 树 | ✗ |
| `headless` | 一次性任务后退出 | ✗ |
| `acp-app` | stdio | ✗ |

**为什么不剪掉 `ui-*` 行**：它们是 `dsh.client` + `platform: "web"` 的浏览器
bundle。`dsh-client-modules` 的机制是「宿主半侧扫描已启用的 Loader 条目并组合
启动图」，`/plugins/<id>/client.js` 按需提供，**node 侧从不执行**。
禁用它们省下的是几十份 package.json 的解析，不是运行时内存；
而代价是维护一份要对着上游 `cordis.patch.yml` 持续复核的 disable 清单。

**不用 Web UI 的后果（明确接受）**：第三方 provider 需要用户手写
`llm-pi-ai:` 段到本 profile 的 Cordis patch。官方 DeepSeek 模型走
`$DSH_HOME/.credentials.yaml`（home 级），不受影响。

**最小性不变量（可执行）**：

| 层 | 不变量 | 判定 |
| --- | --- | --- |
| bundle 数 | 精确等于 `['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']` | preflight 精确相等断言，不用「包含」 |
| 插件依赖 | `dependencies` 为空对象 | preflight 断言 |
| host 平面行 | 默认全保留 | 只有实测收益 ≥ 5% 或 4MB 才加 disable |

用**精确相等**而非包含，是为了抓 `dsh plugin add` 悄悄塞进来的 bundle：
那种情况启动照常成功，profile 静默变大。

**disable 清单默认不发**。它需要对着上游 `cordis.patch.yml` 持续复核，
上游每加一行都要重看；只有实测证明有收益才值得背这个债。测量协议见 DESIGN.md。

**M0 待验证**：空 profile + 已有 DeepSeek 凭据时，`session/modelCatalog`
是否真的非空（即官方 provider 无需 profile 级配置即可注册）。

## #17 产品定位：code 为主，通用 work 轻度

**日期**：2026-11

**决策**：主场景是编程任务（读码、改码、跑命令、查 diff）；通用工作只需能完成，
不为其增加专用界面。

**表面优先级**（P0 未完成不加 P1，P1 未完成不加 P2）：

| 优先级 | 表面 |
| --- | --- |
| P0 | 工具行（`bash` `read` `write` `edit` `grep` `glob`）、diff 渲染、流式消息 + 等宽代码块、timeline 虚拟化、ctx 圆环 + compaction |
| P1 | 审批 UX、工作区文件树、变更文件 |
| P2 | 会话列表、搜索、历史 |
| 不做 | Office、文档预览、内嵌浏览器、定时任务、MCP 管理页、消息反馈 |

**与「极致低内存」的关系**：两者不冲突且互相加强——code 会话的工具行密度远高于
闲聊，虚拟化与便宜行渲染本来就必须做；反过来，放弃 Office/文档/浏览器三个表面
又砍掉了一大块依赖与常驻。

**对 profile 的影响**：**不改变 bundles**。工具集来自 agent preset
（`agent-preset-registry`，默认 `standard`），不来自 profile。只改变：
(a) 我们优先实现哪些工具行渲染；(b) disable 候选名单收窄
（`office-kit` `skill-office` `message-feedback` `session-log-download`
`schedule` 明确不需要；`directory-picker` `workspace-files`
`workspace-changes` `mcp-manager` 升为保留）。

**未决**：是否把默认 agent preset 从 `standard` 改为 `ptc`（程序化工具调用，
对 code 任务可能更合适）。改它属于写 profile 配置，与「客户端不写 profile」
冲突，故留待后续单独决策。

## #18 自带运行时：打包 Node + dsh，不依赖 PATH

**日期**：2026-11

> 后续修正：版本钉死、数据版本护栏与「不污染用户环境」继续有效；
> 具体打包形态改为 #52 的独立 runtime package。legacy 布局仅作兼容回退。
> 当前实际钉版为 `0.2.2-rc.5`（2026-11-19 升级）；本节的 rc.3 分析是首次打包时的历史依据。

**决策**：客户端**自带**固定版本的 Node 运行时与 dsh（含完整生产依赖树），
打包进 app bundle 的 `Resources/runtime/`。**不**从 PATH 查找 dsh，
**不**在运行期安装或升级，**不**写用户全局 npm。环境变量可指向 PATH 上的
dsh 作为开发/排障逃生口。

**背景**：原先设计是「spawn PATH 上的 dsh，启动前跑 `dsh --version` 校验矩阵」。
这条设计假设用户已经装了 dsh，与实际使用场景不符。

**依据**：

| 事实 | 影响 |
| --- | --- |
| `@deepseek-ai/dsh` 在公共 npm 上，CLI 包 76KB，依赖是 `@deepseek-ai/dsh-*` 家族 | 组装可在构建期完成 |
| 本机实测 Homebrew node 26.5.0 Cellar 90MB（`libnode.147.dylib` 71.6MB） | 运行体积数十 MB 量级，需 M0 实测总量 |
| 上游 Electron 同样自带运行时（`app.asar/dsh` + `primary-runtime`） | 有先例，且它同样不依赖用户环境 |
| dist-tag：`next` = `0.2.2-rc.3`（11-13）、`latest` = `0.2.2-rc.1`（11-04）、`alpha` = `0.2.2-alpha.3` | **钉死版本号 `0.2.2-rc.3`**，详见下节 |
| `engines.node` = `>=24.9.0` | 自带 Node **不能取 LTS 20/22** |
| `bin.dsh` = `lib/bin.js` | 组装入口 |

**版本选择：钉精确版本，不依赖 dist-tag。**

`latest` 在这里**不代表稳定**：它是 `0.2.2-rc.1`，本身就是预发布版。整个 0.2.2
线至今没有非 rc 的发布，所以两个 tag 给的都是 RC，「用 `latest` 更安全」在这里
不成立。两者真正的差别是「哪个版本被核验过」：

| 版本 | 发布日 | 仓库 tag 提交 | 与 master（39163dd）的关系 |
| --- | --- | --- | --- |
| `0.2.2-rc.1` | 11-04 | `66491b0` | 未经核验 |
| **`0.2.2-rc.3`** | **11-13** | **`38a6a5c`** | **仅早 2 小时；本文引用的关键文件零变更** |

本文所有源码级引证（303 握手、cookie authority 含端口、`--from-default-profile`
无条件 replace、`DEEPSEEK_API_KEY` 凭据 ref、catalog 的 `failures`）都是对 master
做的。在 rc.3 这个 tag 上逐个复查：`profile-boot.ts`、`browser-auth.ts`、
`llm-deepseek-api-key/src/index.ts`、`session-controller/src/catalog.ts`、
`web-app/cordis.patch.yml` 自 rc.3 以来**零提交**；`official-bundles/src/bundles.ts`
唯一的变更是 release 分支回合并，内容与 rc.3 逐字一致（`web` 模板仍是
`['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']`）。所以**选题 rc.3 时，
本文的事实基础成立**。

固定方式（写进 `Resources/runtime/runtime.json`）：

```json
{ "dsh": "0.2.2-rc.3", "integrity": "sha512-…", "node": "…" }
```

- **禁止**在组装脚本里写 `@deepseek-ai/dsh@next`——tag 是移动指针，重跑构建
  可能拉到 rc.4，而 rc.4 的 wire 细节没人核验过。
- tag 只用于**发现**新版本（发布清单里跑一次 `npm view … dist-tags`），升级是
  显式动作：改版本号 → 重跑 preflight → 复检本文引用的源码行。
- 锁定 `integrity`，构建期不联网重解析。

**收益**：真正「打开即用」；版本矩阵收敛成一个固定版本；首次启动不联网；
不污染用户环境。

**代价（已实测，比原估计大一个量级）**：直接 `npm install
@deepseek-ai/dsh@0.2.2-rc.3` 的生产依赖树是 **1.3GB**，不是原估的 150-250MB。
200 个顶层包里 `@deepseek-ai/*` 占 1.1GB、340 个包，构成如下：

| 占用 | 包 | 我们用吗 |
| --- | --- | --- |
| 158MB | `libreoffice-kit-darwin-arm64` | 不用（#17 不做 Office） |
| 152MB | `libreoffice-kit-wasm` | 不用 |
| 33MB | `sherpa-onnx-darwin-arm64` | 不用（语音） |
| 28MB | `dsh-experimental-inspector` | 不用 |
| 24MB | `dsh-client-ui-sidebar-documentpreview` | 不用（浏览器 bundle） |
| 21MB | `dsh-web-frontend` | 不用（#16 不使用其 Web UI） |
| 67MB | 其余 `dsh-experimental-*` | 不用 |

也就是说 **1.3GB 里过半是「我们明确不做」的功能**。这正是「最小性三层」要解决的
问题：裁剪是必须的工程动作，不是优化项。裁剪后的真实体积待实测，暂不写数字。
升级 dsh 版本需要发版。

**新增硬规则——数据版本护栏**：我们与用户机器上可能存在的其他 dsh 安装共用
`$DSH_HOME`。自带版本可能**低于**用户已有版本，旧版读新格式 `storages` 可能
不可逆损坏（`workspace` 存储已是 version 2）。因此启动前读
`$DSH_HOME/storages/*.json` 的 `version`，高于支持表就**拒绝启动**。
宁可启动失败，也不让旧版迁移或降级用户数据。

**连带结论**：全新机器（无 `~/.dsh`、无凭据）的真实状态是**空模型列表**——
能打开、能看历史，不能发消息。这是「不做引导」的必然结果，不是缺陷。
空态必须指出凭据来源（`$DSH_HOME/.credentials.yaml` 或环境变量）。

## #19 生命周期：detached host + cookie 自签 attach

> **已被 #39 / #41 取代**：host 现在绑定 app 生命周期（看门狗），
> 不缓存 ready URL、不 attach。本条保留作历史记录。

**日期**：2026-11

**决策**：host **不绑定窗口进程生命周期**。spawn 时用独立进程组，窗口退出不带走
host；ready URL 缓存到应用数据目录（0700/0600）；下次启动优先 **attach**
（读 `.credentials.yaml` 的秘钥自签 cookie），失败才 spawn。

**依据**：rustdsh 的实测做法（`apps/rdsh-desktop/src/host.rs`、`cookies.rs`）。
其源码注释直说动因：「The harness runtime is a Node process, so launching it
costs seconds」，以及「closing the app must not take the host down」。
复用把冷启动从秒级降到一次 HTTP 往返。

**cookie 自签机制已用 dsh 源码核实**
（`packages/host/authentication-token/src/browser-auth.ts`）：

| 项 | 值 |
| --- | --- |
| 秘钥记录 | `client-connection/browser-session:` → `secret:`（base64url，32 字节） |
| cookie 名 | `dsh-auth-<base64url(SHA-256(authority))>` |
| cookie 值 | `v1.<base64url(payload)>.<base64url(HMAC-SHA256(secret, body))>` |
| payload | `{version:1, authority, issuedAt, expiresAt}`，30 天 |
| authority | 不加密时 `url.host`（含端口），secure 时 `url.origin`（含 scheme） |

**两条握手路径**：

| 路径 | 条件 | 成本 |
| --- | --- | --- |
| A：attach | `.credentials.yaml` 已有秘钥（host 曾运行过）+ 缓存有效 | 一次 HTTP 往返 |
| B：spawn + token 交换 | **首次启动必需**（秘钥由 host 首次运行写入） | 秒级，受 90s 超时保护 |

**token 交换必须走 `/connect`，不是浏览器那条 `/`**（2026-11 实测，见 PROTOCOL）：

```text
GET /connect?token=<launch-token>&interactions=answer   → 302 + v2 cookie
```

- `interactions=answer` 不可省：它决定这个 peer 是否参与**转发的人类交互**。
  默认 `none` 意味着审批请求根本不会送到我们这里——审批 UX 依赖这个参数。
- v2 cookie 的 payload 带 host 签发的 operator `id`（`authenticateApi` 只认 v2），
  与浏览器会话（v1，无 id）是两套身份。
- `Location: ./api/remote.mux` 不要跟随，段内带点，对非浏览器客户端是 404。

**待测（M0）**：自签 attach 时能否自行生成 operator `id`，还是必须是 `/connect`
签发过的那个。保守做法：把首次 `/connect` 拿到的 `id` 与 cookie 一起缓存，
attach 时原样复用；失败再走路径 B。

> **2026-11 结案（见 #40）**：不需要 `id`。`v1` 与 `v2` 不是版本新旧，是两种
> 身份——自签 `v1` 落在 `browserPeer`（owner / `answer`），`v2` 才是 `/connect`
> 的 API operator（带 host 签发的 `id`）。下面的「ready URL 缓存 + PID 校验」
> 方案也没落地，#40 改成了启动时扫 loopback 自签 attach，#39 把「窗口退出
> 不带走 host」改成了「自己拉起的 host 随窗口退、复用的 host 不动」。

**配套细节（照抄 rustdsh 的可靠性处理）**：

- PID 复用防护：停止前用 `ps -o command= -p <pid>` 确认命令行仍是记录的调用；
- 扫日志找 ready URL，每 150ms 一次，90s 超时；
- attach 前用自签 cookie 探测一次，命主拒绝就丢弃缓存（缓存可能属于另一个 `$DSH_HOME`）；
- 环境变量可强制跳过 attach（用来测指定的 host 构建）。

**安全**：token、cookie、`.credentials.yaml` 秘钥三者都不得进日志/崩溃报告/
遥测/UI；缓存与日志文件 0600、目录 0700。

**不采纳 rustdsh 的做法**：它靠 PATH 发现 dsh，并（据其 `candidate_binaries()`
的搜索顺序推断）接管 `dsh` 命令名（源码里出现 `dsh-orig`、
`~/.local/opt/dsh-original/bin/dsh`）。我们不接管、不安装、不修改用户环境——见 #18。

## #20 UI 层只用 `gpui-base`，不启用 `gpui-component`

**日期**：2026-11

**决策**：界面只用 `gpui-base` 的原语与交互组件；`dsh-client-ui` 把
`component` / `assets` 两个 feature 从默认里去掉，需要时显式开启。

**依据是实测，不是偏好。** 空窗口、零业务代码，`probe` profile，`ps -o rss=`，
每种配置三次：

| 配置 | 稳态 RSS | 二进制 |
| --- | --- | --- |
| 仅 `gpui-base` | 61.8MB（61.6–62.1） | 6.9MB |
| + `gpui-component` + `assets` | ~73MB（72.9–86.5，中位 73.2） | 14MB |

+12MB RAM / +7MB 磁盘，是空窗口基线的 20%。north star 是「把 desktop 的 RAM
减少到极致」，20% 不是能随手送掉的量。

**依赖是单向的，可以只开图标包**：`gpui-component` 依赖 `gpui-kit-assets`，
反过来不成立。事后补测 `assets` 单独启用（同样空窗口、三次）：**60MB / 二进制
7.2MB**——也就是 0 增量。前面那 +12MB 全部来自 `component` 的样式层与控件代码，
我当时把两个 feature 合在一起测，结论记串了。据此把 `assets` 重新设为默认开启，
图标直接用 Lucide（见 #22）。

**为什么不要也不痛**：`gpui-base` 本身已是一整套行为组件——其模块表含 `input` /
`button` / `dock` / `dialog` / `scroll` / `grid_layout` / `auto_scroll` 等。它的
crate 文档明说「deliberately avoid presentation styles: Layout, positioning,
colors, sizing, and motion belong to applications or the `gpui-component`
façade」。而我们的视觉语言（#7，fx-ui）本来就是自定义的等宽文字排版，
`gpui-component` 的样式层用不上，只能反着覆盖。

**代价与回退**：若 `gpui-base` 缺必需原语（候选风险：多行富文本输入、IME 组合、
代码块语法高亮），把 `component` feature 打开即可，一行改动。可逆性高，
不必先证明「永远不用」。

**方法学（适用于以后所有内存结论）**：单次采样不可靠——同一二进制首次读到 75MB，
重复三次稳定在 61.8MB。RSS 结论至少三次取中位数，并记录二进制是否刚重新生成过。

**还必须记「怎么启动的」**：裸二进制与 `.app` + `open -a` 差 23MB（61.8MB vs
85–91MB，同一二进制、同一空窗口），因为裸二进制永远拿不到 active 窗口。真实用户
工况是后者，引用数字时两者不能混。

## #21 ctx 圆环缺分母时只显示数字，不虚构比例

**日期**：2026-11

**决策**：composer 右侧的上下文指示器在拿不到「窗口上限」时显示
`ctx 5.7k`（绝对量），**不画环形进度**。

**为什么**：fx-ui 的规格是 `model | effort | ctx(圆环)`，但圆环需要分子与分母：

| 要素 | 数据来源 | 现状 |
| --- | --- | --- |
| 分子 | `assistant-stream` 的 `usage.inputTokens` | **已有**，实测 5678 / 14766 |
| 分母 | 模型上下文窗口大小 | **无来源** |

已核实的两个候选来源都不含窗口大小：`session/modelCatalog`（fixture 全量字段里
没有 window/limit/context 类字段）、`projections.contextPressure`（实测为 `{}`，
静止态不产出）。凭空写一个 128k 常量会把「不知道」画成「已用 4%」——那是编造读数。

**触发重估的条件**：拿到 `contextPressure` 的实际形状，或 catalog 补上窗口字段。
届时把 `CtxMeter` 扩成 `{ used, limit: Option<u64> }`，`limit` 为 `Some` 时画环。

**同一原则**：模型名与 effort 在 catalog 接线前显示 `model —`，也不写死
`deepseek-flash`。

## #22 图标用 `gpui-kit-assets` 的 Lucide，不手绘、不用字形

**日期**：2026-11

**决策**：侧栏/标题栏的图标一律取 `gpui_kit::assets::IconName`（Lucide），
尺寸 14（搜索、加号、面板、设置）与 12（折叠箭头），颜色由容器
`text_color` 给、尺寸由容器 `text_size` 给。

**为什么**：先前用 `⌕ ＋ ▭ ⚙ ▾` 这些**文字字形**顶替，字形墨迹只有 6–8px，
比设计稿的 14px 图标小一圈，而且随字体度量漂移；手绘路径（`canvas` +
`PathBuilder`）虽能量准尺寸，但每个图标都要自己描，齿轮这类曲线尤其难看。
`IconName` 实现了 `RenderOnce`，直接 `.child(IconName::Search)` 就按当前字号
与字色渲染成 `svg()`，注册只需 `application().with_assets(Assets)`。

**代价**：默认内嵌 101 个组件图标共 44KiB，RSS 增量在噪声内（见 #20 的订正）。
完整 1830 个目录里的名字也在枚举里，但没注册就不进二进制——用到未内嵌的名字
（例如 `FolderPlus`）会画不出东西，所以只用默认集里有的。

## #23 插入符要求窗口 active：裸二进制永远不闪，必须打包成 `.app`

**日期**：2026-11

**决策**：光标「点了输入框也不闪」的根因不在聚焦，而在
`InputBase::show_cursor()` 的第四个条件 `window.is_window_active()`。
`is_window_active()` 读的是 `NSWindow.isKeyWindow()`，而裸二进制
（`cargo run` 直接跑）在 macOS 上无法成为前台应用：实测
`active=false focused=true` 稳定复现，输入框收到 `InputEvent::Focus` 但
caret 一次都不画。

**验证链**（都是当下这台机器上跑出来的，不是推断）：

| 步骤 | 观测 |
| --- | --- |
| 裸二进制 + 每次 0.25s 连拍 6 帧 | 光标区 6 帧哈希全同 → 从未绘制 |
| 采样 `window.is_window_active()` | 12 次全 `false`，`focused=true` |
| 打包最小 `.app`（Info.plist）+ `open -a` | `active=true`，8 帧出现 2 种状态 → 在闪 |
| caret 像素取色 | 改样式前 `rgb(10,10,10)`（近黑，深色底等于隐形）→ 改后 `rgb(237,237,237)` |

于是两件事一起做：① **打包**（DECISIONS #18 本来就要做）是插入符可见的前提，
开发期也必须 `open -a` 走 bundle 启动，否则「光标不闪」是必然现象；
② 显式投影 `InputEditorStyle { caret: INK, foreground, background, border,
selection }`——`InputBase` 不投影时 caret 取默认调色板的 foreground，深色主题下
就是那个近黑色。

## #24 侧栏 workspaces 对齐官方 desktop 的行结构

**日期**：2026-11

**决策**：侧栏不再是「当前会话占位」，而是按官方 `dsh-client-ui-workspace`
的模型重写：

| 层级 | 结构 | 交互 |
| --- | --- | --- |
| 头 | `Workspaces` + 加号 | 加号在当前工作区新建会话 |
| 工作区行 | `[折叠箭头][名称][hover 出现的新建]` | 点行切换展开 |
| 会话行 | `[状态点][标题][相对时间]` | 点行切换会话，选中行高亮 |

**数据**：`session/list` 一条不落地进侧栏，按 `cwd`（优先投影里的
`workingDirectory`）分组，组内按 `updatedAt` 倒序、组间按组内最新时间倒序；
标题取投影 `title`，没有就退回会话短 id。

**空白会话只留当前那一条**：官方是 `pinCurrentBlank`——每次启动都会建一个
空白会话，如果全列出来，侧栏就是一片「新会话」（实测 17 条里 12 条是空白）。
除当前会话外的 `blank` 行直接不渲染。

**相对时间**用官方时间列口径：`刚刚` / `Nm` / `Nh` / `Nd`。

## #25 标题栏的面板图标控制「主区内的右栏」，不是左侧栏

**日期**：2026-11

**决策**：右上角那个 `PanelRight` 图标控制的是主内容区右侧新增的一栏
（宽 300，`BG_SIDEBAR` 底、左边框 `BG_CARD_EDGE`），左侧栏常驻、不受它影响。

**为什么**：早期实现把它接到了左栏折叠上，语义反了——`PanelRight` 画的是右栏，
官方 desktop 里也是 `dsh-client-ui-sidebar-right`（会话级右栏）的开关。左栏是
全局导航，右栏是会话上下文，两者生命周期不同。

**内容**：右栏第一版只放真实有来源的字段——工作区路径、会话标题、`model`、`ctx`；
没接线的读数显示 `—`（沿用 #21 的不虚构原则）。

**左栏的开关是另一对**：侧栏顶部用 `PanelLeftClose` 收起，收起后标题栏最左出现
`PanelLeftOpen` 展开。两个方向都必须在场——只给「关」不给「开」等于把导航锁死。

## #26 mode 的写入走 `commands/execute`，不是 `session/prompt`

**日期**：2026-11

**决策**：切换权限 preset 用一元调用 `commands/execute`：

```json
{ "agentId": "session-…", "line": "/permission read-only", "submittedAttachments": [] }
```

返回 `{ commandId, result: { kind: "success", text: "preset read-only" } }`；
没有命令匹配时返回 `null`。改完从投影 `permissions.currentValue` 读回。

**为什么专门记一条**：把 `/permission read-only` 当成普通提示丢进
`session/prompt` **不会**切模式——实测它退化成一次真实问答（425 帧
`assistant-stream`、preset 仍是 `workspace-write`）。官方 UI 也不是走 prompt：
`ui-permission` 的 `select()` 调的是会话面的 `command(line)`，会话面再落到
`commands/execute`。

**实测四种读数的分工**（同一会话）：

| 用途 | 来源 | 实测值 |
| --- | --- | --- |
| 候选项 | `permissionPresets/catalog`（无参） | 3 个：`read-only` / `workspace-write` / `danger-full-access` |
| 当前值 | `session/list` 投影 `permissions.currentValue` | 改前 `workspace-write` → 改后 `read-only` |
| 变更事件 | follow 流的持久事件 `permission/preset` | `{"preset":"read-only"}` |
| 写入 | `commands/execute` | 见上 |

`PermissionCatalog` / `PresetOption` 已在 `dsh-protocol::catalog` 里，
`SessionSummary::permission_preset()` 读投影。

## #27 composer 的 mode 是下拉选择，不是循环切换

**日期**：2026-11

**决策**：composer 左侧的 `mode` 芯片点击拉起浮层（`read-only` /
`workspace-write` / `danger-full-access`），当前项打勾，点选即提交
`SetMode`。浮层向上展开、宽 232、`BG_CARD` 底 + `BG_CARD_EDGE` 边 + `shadow_lg`。

**为什么不做成「点一下换下一个」**：mode 是权限可见性的开关，误触的代价是
让 agent 拿到更大权限；三个值必须让人看清再选。同理，浮层里显示原值
（`workspace-write` 而不是「工作区写入」）——这些 key 是用户能在文档、
issue、日志里对上的字符串。

**乐观更新的边界**：点选后先把芯片切成新值，host 的投影回来再覆盖成权威值；
命令失败时界面收到 `Failed`，不谎报已生效。

## #28 首屏窗口尺寸自适应，不写死

**日期**：2026-11

**决策**：设计稿画布是 1440×920（Ardot 根 frame），但开窗时按主屏可用区域算：

| 量 | 值 | 位置 |
| --- | --- | --- |
| 设计尺寸 | 1440×920 | `DESIGN_WINDOW_SIZE` |
| 下限 | 960×640 | `MIN_WINDOW_SIZE`（同时作为 `window_min_size`） |
| 边距 | 96（两侧各 48） | `SCREEN_MARGIN` |

`initial_window_bounds()` 取 `cx.primary_display().bounds()`，尺寸夹到
「可用区域 − 96」，再在可用区域内居中；取不到屏幕信息时退回设计尺寸。

**为什么不直接把 1440×920 交给窗口服务器**：它也会夹，但夹完保留我们给的
固定起点 (80,60)，结果是窗口贴边、像跑偏了。本机实测：屏幕可用 1352×878，
旧写法开出来 1352×848（贴左、上下错位）；新写法 1256×782 居中、四周各 48。

**已知缺口**：用户拖动后的尺寸不持久化（没有配置文件），下次启动仍按上面的
规则算。要做「记住上次尺寸」得先有配置层，属于 #19 生命周期的一部分。

## #29 401 先重新握手一次，仍失败才报错

**日期**：2026-11

**现象**：点旧会话报 `订阅失败：WebSocket 失败：HTTP error: 401 Unauthorized`。
触发条件是 **host 进程换过、客户端还活着**——新进程换了签名密钥，旧 cookie
一律作废，而 ready URL 里的 token 也属于旧进程。

**决策**：

1. `Connection` 记住 `token` 与 `interactions`，cookie 放进 `RwLock` 可整体替换；
2. 一元调用或 WebSocket 握手拿到 401 → `refresh()` 重新 `/connect` → 重试**一次**；
3. 重新握手本身也是 401 → `HostError::Unauthorized`，文案写明「host 可能已重启，
   需要重新获取 ready URL」，界面显示 `订阅失败：认证失效（401）…`。

**一个前提事实**：host 的 cookie 有效期 30 天（实测 `Max-Age=2592000`），且同一
token 可以同时存在多张有效 cookie（连开两次都返回 200）。所以「cookie 坏、host 好」
这个自愈分支在真机上**造不出来**——`refresh()` 本身的可用性用
`examples/auth.rs` 验过（握手换新 cookie 后流仍可用），自愈后的重试只能靠代码审查。

**复现**（`crates/dsh-host/examples/auth.rs`）：固定端口起 host A → 客户端连上 →
杀掉 A、同端口起 host B → 唤醒客户端调用 → 三步全部报认证失效。

## #30 模型选择的三个形状：目录 / 投影 / 写入

**日期**：2026-11

| 用途 | 端点或投影 | 实测形状 |
| --- | --- | --- |
| 目录 | `session/modelCatalog`（**无参**，`{}` 即可） | `default={provider:deepseek-official, model:deepseek-flash}`；1 组 2 个模型；每个模型 4 档强度 `off/low/high/max`，`defaultEffort=high` |
| 当前值 | 会话投影 **`modelSelection`** | `{"lastUsed":{"model":"deepseek-flash",…},"next":{"model":"deepseek-v4-pro","reasoningEffort":"high"}}` |
| 写入 | `session/selectModel` | `{"request":{"sessionId","provider","model","reasoningEffort"?}}` → `{"selected":{…}}` |

**`next` 与 `lastUsed` 的分工**：`next` 是已选、**下一轮生效**的那一个（官方语义：
运行中的步骤保留启动时的模型）；`lastUsed` 是上一轮实际用的。界面显示优先
`next`，缺失退回 `lastUsed`，两者都没有才用目录的 `default`——空白会话就是这种
情况，此时 host 会按 `default` 跑，所以显示它不算编造（DECISIONS #21 的边界）。

**形参键又是个陷阱**：`session/selectModel` 是 **`request`**，而
`session/list` 是 **`_request`**。同一个 namespace 里两种写法并存，
只能逐个端点查 `typert.remote-client.d.ts`，不能照抄邻居。

**effort 不能猜**：档位 id 是不透明的，官方明确「不得自行降级或合成 `off`」。
换模型时强度取该模型目录里的 `defaultEffort`；模型没有 `reasoning` 段就不显示强度。

## #31 ctx 圆环与缓存命中率：分子分母都得是实测值

**日期**：2026-11

**数据源**：`session/projections`（形参键 `request`）。这里有个坑——
`contextPressure` 在官方类型里标了 **`SessionProjectionSummaryExcluded`**，
所以 `session/list` 的投影摘要里**没有**它，只能单独读：

```json
{"kind":"sequenced","values":{
  "contextPressure":{"contextWindow":1048576,"pressureTokens":14731,"projectedTokens":15218},
  "contextBreakdown":{"messageTokens":5521,"systemTokens":1541,"toolsTokens":7740},
  "tokenUsage":{"cacheReadTokens":8832,"cacheWriteTokens":0,"outputTokens":418,"uncachedInputTokens":5899}
}}
```

**公式与官方逐条对齐**：`dsh-token-meter` 的投影定义、
`ui-conversation/src/client/context-occupancy.ts`、`stats-usage.ts` 三处一致。

| 量 | 公式 |
| --- | --- |
| 圆环分子 | `projectedTokens ?? pressureTokens` |
| 圆环分母 | `contextWindow` |
| 百分比 | `min(100, round(used / window * 100))` |
| 命中率 | `min(1, cacheRead / (uncachedInput + cacheRead + cacheWrite))` |

自洽校验：`5899 + 8832 + 0 = 14731` 正好等于 `pressureTokens`——说明三个桶互不重叠，
所以**不能**只加 `input + cacheRead`（之前的实现漏了 `cacheWrite`）。

**缺一边就不画环**：`projectedTokens`/`pressureTokens` 与 `contextWindow` 都必须有，
否则退回绝对量文本（DECISIONS #21）。实测 15218 / 1048576 = 1%，圆环只有一丝弧——
这是真实读数，不做放大美化。

**什么时候读**：启动、切会话、以及每条 `usage` 帧之后补读一次（`usage` 帧每轮一条）。
`contextPressure` 里的各字段是不同时刻的 last-wins 记录，切模型时会短暂出现
「新容量 + 旧压力」，官方明确接受这个取舍。

**画法**：`PathBuilder::stroke(px(1.5))` 的折线弧（10° 一段），从 12 点顺时针；
`paint_path` 直接吃描边路径，不用拼环带。

## #32 侧栏品牌用官方锁版 SVG，不手绘、不用字形

**日期**：2026-11

原实现是 14px 粗体文字 `dsh`。要换 logo 时三条路：

1. **手绘鲸鱼**：官方 mark 是 3448 字节的贝塞尔路径，重画必走形；
2. **图标字体**：Lucide 里没有品牌图形，且等于引入一个字体文件；
3. **嵌入官方矢量**（选中）：从 harness 自带前端产物里取出 `BrandWordmark`
   组件的 19 条 `<path>`（鲸鱼 mark + `deepseek HARNESS` 字标），
   坐标原样就是它的 `viewBox` `0 0 182 24`。

用 `svg().data(include_bytes!(...))` 直接喂字节，不需要 AssetSource——gpui 的
`Svg::data()` 会按数据哈希缓存，`paint_svg` 把 `text_color` 传给 `currentColor`。

**踩到的坑**：第一次只取了 4 条路径，界面只显示 `deep`。原因是按函数体定长切片
再正则抓 `d:"…"`，把后 15 条（含 `HARNESS`）截掉了——看着像「SVG 被裁切」，
实则是自己的文件不全。教训：抓产物里的矢量要抓完整组件体，别用魔法长度。

尺寸：15px 高、宽 113.75（182:24 锁比）。侧栏 264 宽，头部一行放得下
「红绿灯让位 68 + 锁版 113.75 + 搜索 14 + 收起 14 + 4 个 8px 间隙」。

## #33 两个加号分工：头部加目录、行内加会话

**日期**：2026-11

侧栏头部 `Workspaces +` 原来发 `NewSession`（在当前工作区建会话），语义是错的：
用户没有任何入口把**新目录**加进侧栏。改成官方语义：

| 入口 | 动作 | 传输 |
| --- | --- | --- |
| 头部 `+` | 选目录 → 注册成工作区 | 客户端原生目录框 + `workspace/create` |
| 行内 hover `+` | 在该目录下开新会话 | `session/create` |

`workspace/create` 的形参键是 `request`，且只认**已存在的目录**（幂等：
重复注册返回同一 workspace，`created` 为 `false`）。

**目录框放客户端而不是 host**：官方 §directoryPick 是在 host 那台机器上弹框
（`directoryPicker/pick`，无参、取消返回 `null`），因为 host 可能跑在远端。
本客户端与 host 本就同机，用自己的原生框少一次往返，也不用等 host 的对话框
抢焦点。注意 gpui 的 `prompt_for_paths` 是 `Result<Option<Vec<PathBuf>>>` 套在
oneshot 里——取消、系统错误、没选中三种情况都要当「放弃」，不能中途 `unwrap`。

**注册完还要开一个空白会话**：侧栏是按会话 `cwd` 分组渲染的（#24），只注册目录
不建会话，用户点完界面毫无变化。所以顺带 `session/create` 在该目录开一条空白
会话并切过去——既让新目录立刻可见，也符合「打开即用」。

## #34 composer 的 `@` / `/` 补全

**日期**：2026-11

**数据源**（都是会话作用域，不是全局）：

| 触发 | 传输 | 说明 |
| --- | --- | --- |
| `/` | `commands/list {agentId}` | 只列**当前 agent 可见**的命令，已按作用域遮蔽 |
| `@` | `workspaceFiles/search {workspaceFileScopeId, request}` | host 用打包的 ripgrep 在会话工作区里搜 |

两个坑：`commands/list` 的形参键是 `agentId`（scope 的 wire 名，不是 `request`）；
`workspaceFiles/search` 的作用域形参叫 `workspaceFileScopeId`，**值是一个 SessionId**，
而且 `query` 为空会被 host 拒绝（所以空查询在客户端直接返回空，不打扰 host）。

**触发语法照抄官方**（`ui-input-trigger/src/core/detect.ts` + `file-reference/src/grammar.ts`），
不自己发明：

- `/` 只在草稿开头、空白之后、或标点之后开启；`//` 的第二个斜杠、`https:/` 里
  紧跟非空白字符的 `:` 之后的斜杠都不算；
- `@` 必须在开头或空白之后（邮箱不触发），`@"…` 引号内允许空白；
- 接受后的插入文本：命令补一个尾空格（如 `permission` 插成 `/permission` 加一个空格）；路径含空白就加引号，
  目录补尾斜杠且**不闭合引号**（`@"my dir/`），与控制字符/引号冲突的路径直接不插。

**键位按用户指定**：`Tab` 向下、`Shift+Tab` 向上、`Enter` 补全到输入框、`Esc` 收起；
方向键作为同义快捷键保留。

**为什么是 `intercept_keystrokes` 而不是 `capture_key_down`**：实测 `capture_key_down`
里的 `cx.stop_propagation()` 挡不住键绑定——回车照旧触发输入框的提交、Tab 照旧被焦点
遍历吃掉。`App::intercept_keystrokes` 在动作派发**之前**跑，`stop_propagation` 真的生效。
拦截器只在自己的输入框拿着焦点时才动手（否则会把全局 Tab 关掉），订阅必须存在结构体里，
丢了就失效。

**鼠标与宽度**：候选行可点（`on_mouse_down` → 选中并接受），hover 高亮与键盘选中
分开记（`hover_completion`），两者可同时亮在两行上。浮层宽度不再写死，锚点与卡片共用
`w_full + max_w(CONTENT_W)`，窄窗口下一起缩。

**浮层命中框的坑**：浮层刚开的那一帧，它的 hitbox 还没进 `rendered_frame`——
同一坐标先 hover 得到、click 却穿透。合成点击做验证时必须等面板真上屏（实测 ≥ 1s），
否则会误判成「点击不支持」。

**验证纪律（事故后立的规矩）**：

- 自动化验证**绝不允许**把草稿真的投给 agent；`DSH_DEBUG_NO_SEND` 显式声明，
  `DSH_DEBUG_KEYS`（合成按键）自动进这个模式；
- 跑 `send` 的"测试"必须用一次性工作区（`/tmp/…`）和一次性 `DSH_HOME`，
  **不许**拿用户主目录当 `cwd`——那是让 agent 能读用户全部文件的操作；
- 合成按键用 `window.dispatch_event(PlatformInput::KeyDown(...))`，`Keystroke::parse`
  只接受 `&'static str`。

## #35 移除工作区 = 只删注册（侧栏改注册表驱动）

**日期**：2026-11

**语义照 host**：`workspace/delete {request:{workspaceId}}` → `{deleted:true}`，
官方描述就是「Remove one Workspace registration while retaining files and Sessions」。
所以侧栏的垃圾桶**不删目录、不删会话**。仍然做二次确认（官方也有确认弹窗）：
第一下把行尾的两枚图标就地换成「取消 / 移除」，两枚胶囊 20pt 高、可点可 hover；
点「移除」才真发 `workspace/delete`，点行或点「取消」退回。删除虽可恢复，
但会重排侧栏、把会话挤到「未分组」，误点代价不小。
被测目录一次也没被碰过。

**分组模型必须跟着改**：原来侧栏是按会话 `cwd` 现算分组，注册表根本不参与——
那样删了注册在界面上毫无变化（组还在）。现在「有哪些组」只看 `workspace/follow`：
基线 + `upsert` / `remove` / `order` 增量合成列表，会话只负责往组里填；
没对上任何注册的会话落到最底下的「未分组」。这样删除后的效果与 host 语义一致：
行消失，会话落到「未分组」，目录与历史都在。

**踩到的坑：一个 peer 只准一条 socket**。Gateway 的 `admitConnection` 里
`record.ownsSocket` 为真时，第二次 WebSocket upgrade 直接 `409 Conflict`。
原来的 `FollowStream::open` 是「一条流开一个 socket」，于是 `session/follow`
占住 socket 后，`workspace/follow` 怎么都连不上（日志里每秒一条 409）。
改成**单 socket 多路复用**（`crates/dsh-host/src/mux.rs`）：一个后台任务持 socket，
上行发 `open`/`cancel`，下行按 `streamId` 分帧；读端被 drop（切会话时 abort reader）
就自动给 host 发 `cancel`。副作用是窗口只剩一条 WS——也顺着内存那个北极星。

**验证（实测）**：`.app` 内合成点击工作区行右侧的垃圾桶 → `dsh-ws-del` 行消失、
它的「新会话」挪到「未分组」、`/tmp/dsh-ws-del` 目录仍在、
`workspace/follow` 基线只剩另一条；磁盘上 `sessions/` 里 76 个会话目录一个没少。

## #36 会话不「删除」，只归档（host 根本没有删除接口）

`workspace/*` 里只有 `archiveSession` / `unarchiveSession`（外加 `pinSession`
与 `insertSessionBefore`），**没有任何删会话的方法**。所谓「删掉一个会话」在
host 语义里到顶就是归档：记录还在磁盘、目录还在、`session/list` 照样列出它，
只是不再出现在分组界面里。所以侧栏给的是**归档**，不是删除，也不需要吓人的
确认——它是可逆的。

**两个方向都回整份集合**：`workspace/archiveSession {request:{sessionId, stopActivity}}`
与 `workspace/unarchiveSession {request:{sessionId}}` 的响应都是
`{archivedSessionIds:[…]}`，不是增量。`stopActivity: true` 表示「会话还在跑就先
把它停掉」（turn、子 agent、后台任务、定时），否则 host 用
`workspace/session-active` 拒绝归档。界面上因此分两种：空闲会话点一下就归档；
**正在跑的会先就地确认**（复用工作区那张「取消 / 归档」胶囊），确认后才带
`stopActivity: true` 发出去。

**踩到的坑：自己发起的变更不会回流浪**。`workspace/follow` 只把**别的**写入者
造成的变更推过来；自己调 `archiveSession` 得到的只有那条 RPC 回执。第一版只在
UI 里做乐观更新、而 follow 那边还留着一份旧注册表，于是下一条无关帧（比如
`session/create` 触发的 `upsert`）带着旧的归档集合整份覆盖——侧栏刚变成 3
又跳回 2。修法是把注册表交给**一个主人**：`Arc<Mutex<WorkspaceRegistry>>`，
follow 与归档/取消归档的调用方共写同一份，回执先写回去再整份推给界面；
`UiEvent::Workspaces` 从此只有一种来源，乐观更新只是抢在回执前面的一帧。

**侧栏结构**：归档的会话不进工作区树（否则「归档」等于没做事），底部多一行
「已归档 N」，默认收起，展开后每条带 hover「取消归档」。侧栏正文顺手改成
可滚动——会话一多，最底下的已归档区原本永远在画布外。

**验证（实测，一次性 host `/tmp/dsh-home-arch`）**：
`workspace/archiveSession` 让集合 2→3、`unarchiveSession` 3→2，两个方向
`session/list` 都还能查到该会话；界面上的「已归档 N」与 host 基线始终一致
（`archived=3 matched=3` → `archived=2 matched=2`）；归档后它从树里消失、
只剩工作区自己那条会话。另一条通路也验证过：**别人**（探针进程）归档时，
`workspace/follow` 会推 `archived` 帧，界面计数 1→2。

## #37 一轮对话的状态：一个 `phase`，三处回声；回车不能被补全吞掉

**两个问题一起解**。一个是 bug：输入框里打完东西按回车不发。另一个是空白的：
消息发出后、答完之前，界面上什么都没有。

**回车不发的原因不在发送，在补全**：上一版补全面板一弹出就把第 1 条候选高亮
（`selected = 0`），于是 `Enter` 进了「接受高亮」那条分支，把 `/per` 补成
`/permission` 加一个空格，而不是把 `/per` 发出去。官方不是这么做的——`input-trigger` 的
controller 只有两种手势：没有任何键盘高亮时 `Enter` 是 **`pass`**（放行，等于
发送），按过 `↓`/`↑`/`Tab` 建立起高亮之后才是 **`pick-highlighted`**（接受）。

**改法**：`completion.selected: Option<usize>`，默认 `None`，只有 `Some` 时
`Enter` 才接受；鼠标 hover 依旧高亮，但**不算键盘选中**（和 #34 的分离一致）。
实测：`/per` + 回车 → 走 `send()`（`chars=4`）；`/per` + `Tab` + 回车 →
草稿变 `/permission` 加一个空格。

**状态模型**：`pending: bool` 换成三态 `Phase`（`Idle` / `Sending` / `Running`），
加一个 `turn_started: Option<Instant>` 供计时。`Sending` 与 `Running` 分开是有
必要的：命令通道收下不等于 host 收下，第一帧之前要有自己的话（「已送到 host…」），
不然用户看到转圈会以为已经跑起来了。

**三处回声**（DESIGN.md「一轮对话的状态」有表）：时间线尾巴一行状态、composer
右下主按钮（忙时方块＝停）、侧栏会话行状态点。主按钮在**忙且有草稿**时同时
出现方块与箭头——这就是官方的「排队」语义（`submission-policy.ts` 的 `queue`），
我们固定用 `queue`，没做 `steer`。

**停**用 `session/cancel`（只取消当前轮，不清 inbox）。为什么不在本地直接置空闲：
取消是**请求**，host 还要收尾；本地先回一条灰字「已请求停止本轮」，真正回空闲
看 `turn/end`。实测（一次性 host、`DSH_DEBUG_CANCEL`）：只有一条 `turn/start`、
一条 `turn/end`，取消后状态行与侧栏点同时消失、气泡留着。

**侧栏的运行点不会自己来**：`session/list` 的 `running` 是快照字段，不随流推。
轮次结束时发 `UiCommand::RefreshSessions` 重读一次，否则点会一直亮着（实测已
出现：答完了那个绿点还在）。

**快照晚到会抹掉进行中的一轮**：启动/切会话会拉 `session/follow` snapshot，它可能
比刚发出的那条还早到——典型现象是气泡、状态行一下全没了，只剩空时间线。
忙时直接丢掉 `UiEvent::Transcript`（忙的时候那份快照必然旧）。

**调试钩子**：`DSH_DEBUG_CANCEL=<ms>` 到点就走与方块完全相同的 `cancel_turn()`；
本机合成鼠标点击成功率约一半，所以「真点方块」那一步没验成，如实记在这里。

## #38 `@` 一个字符就列当前目录；补全的两个接口分工不能混

用户问得对：「非要`@`+内容才开始搜？」。官方不是这样——`dsh-file-reference-local`
的 `WorkspaceFileSearch::list(query)` 第一行就分流：

```ts
const slash = query.lastIndexOf('/')
if (query === '' || slash >= 0) return this.listDirectory(directory, fragment, signal)
return rankCandidates(indexed.filter(...), query, maxResults)
```

即：**空查询或带 `/` 的查询都是「列目录」**，只有不带 `/` 的片段才全库搜。
我们上一版只在 `query` 非空时才发 `workspaceFiles/search`，于是 `@` 敲下去
面板空着（空 query 还会被 host 拒：`query must not be blank`）。

**改法：两个接口分工。** 空查询与带 `/` 的走 `workspaceFiles/list`
（`{workspaceFileScopeId, path}`，根目录的 `path` 必须是 `.`，空串回
`gateway/bad-request: path is required`）；不带 `/` 的才走
`workspaceFiles/search`。两边结果都换成 `FileCandidate` 后按官方打分重排：
ripgrep 给的是「先深度后路径」序，不重排的话 `@cr` 第一条是
`target/debug/incremental`。

**顺手把官方的两张清单搬过来了**：隐藏项（`.` 开头的段，除非用户自己就在打 `.`）
与排除目录（`target` / `node_modules` / `dist` / … 共 15 个 basename）。没有它们，
`@` 的前 8 行会被构建产物占满。

**已知差别**（故意留着）：全库那一段我们用 host 的 ripgrep（子串匹配、遵守
`.gitignore`），官方是本地自建索引做**子序列**匹配。所以 `@cr` 在官方会列出
`Cargo.toml`，我们不会。换来的是不把整个工作区读进内存，也不等首次索引。

**实测**（一次性 host，会话 cwd = 项目根 / 家目录）：

| 输入 | 结果 |
| --- | --- |
| `@` | 家目录的直接子项：Applications / Desktop / Documents / … 8 条，全标「目录」 |
| `@crates/` | `crates/dsh-client-ui`、`crates/dsh-host`、`crates/dsh-protocol` 三条 |
| `@cr` | 只剩 `crates`（`target/**` 被排除清单挡掉） |

**验证坑（重要）**：窗口被 Terminal 遮住时 GPUI 会暂停重绘，`screencapture -l`
抓到的是**旧帧**——一度以为「输入框里没有 `@`、面板不弹」。`open <app>` 把窗口
提到前面再截图就能看到真实帧。以后截图前先激活。

## #39 产品形态：`cowork.app` + profile `cowork` + 自己拉起 host

第一阶段的产品形态定下来：一个能双击的 `cowork/cowork.app`，内置 dsh 运行时，
永远自己拉起 host（#41；「优先复用」曾在 #40 实现、#41 删除）。命名按用户要求：app
叫 **cowork**，profile 叫 **`cowork`**（`$DSH_HOME/profiles/cowork`），产物目录
就是 `cowork/`。

**包结构**（`scripts/build-product.sh` 生成）：

```text
cowork.app/Contents/
├─ Info.plist                     CFBundleName=cowork / dev.cowork.desktop
├─ MacOS/cowork                   target/release/cowork
└─ Resources/
   ├─ runtime/node_modules/…      dsh 运行时（APFS 克隆，`cp -Rc`）
   └─ node-path.txt               构建时的 node 绝对路径
```

**为什么 app 得自己拉 host**：#19 定的「detached host + cookie 自签 attach」只是
前提，不自己拉就意味着用户得先开终端——那就不是一个产品。现在启动路径是：
`DSH_READY_URL` 在就用它（开发期）；否则按
`$COWORK_RUNTIME → Contents/Resources/runtime → target/<profile>/runtime → .runtime-staging`
找运行时，spawn，再从输出里抽 `http://127.0.0.1:PORT/?token=…`（45s 上限）。

**踩到的坑：`web` 不是子命令**。`--from-default-profile web` 里的 `web` 是**值**，
命令里不能再跟一个位置参数 `web`，否则 CLI 直接 `too many arguments. Expected 0
arguments but got 1: web` 退出。

**踩到的坑：`[NSApp terminate:]` 不走 `Drop`**。Cmd+Q 时 gpui 调
`NSApplication terminate:`，进程直接结束，Rust 的 `Drop` 跑不到，光靠 `Drop`
会每退出一次漏下一个几百 MB 的 node。改成**看门狗**：用 `/bin/sh -c` 包一层，
node 作为它的子进程（pid 通过 `COWORK_HOST_PID=` 上报），它每秒 `kill -0 $PPID`
探父进程，父没了就 `kill -9` node。实测：`kill -9 <app>` 后 3.5s 内 node 与看门狗
都已经不在了。正常退出仍走 `Drop`（先杀 node 再杀看门狗）。

**单实例锁**：同一 profile 只允许一个实例（否则两个 host 读写同一个 `$DSH_HOME`）。
用临时目录里的 Unix socket 当锁：`bind` 原子，进程死了 `connect` 就失败；
探活失败的空壳会被删掉重来。第二次双击不弹窗，而是 `open <bundle>` 把已有窗口
提到前面。实测：连开两次，应用进程数 1、第二个实例日志打印「已经有一个 cowork 在跑」。
注意 `[NSApp terminate:]` 不会走 `Drop`，锁文件会残留一格——下次启动先探测再删除，
不影响使用（只是 `/var/folders/…/cowork-cowork.sock` 一直在）。

**node 不打包**：Homebrew 的 `node` 是 launcher（52K）+ `libnode.147.dylib`
（71.6M）+ 一堆按绝对路径找依赖的 dylib，拷进包里要逐个 `install_name_tool` 改
rpath。先退一步：构建时把 `node` 的绝对路径写进 `Resources/node-path.txt`，
运行时的查找顺序是 `$COWORK_NODE → node-path.txt → 常见绝对路径 → PATH`。
代价：Node 升大版后要重跑打包脚本（或设 `COWORK_NODE`）。

**内存读数**（release、`.app` 双击、完整界面 + 历史）：客户端两次采样
**84.2 / 98.1MB**（后一次侧栏会话更多，同一个 build），host **409 / 424MB**（工况：
预热后跑过一轮带工具的对话，五次采样波动 ±0.2MB）。host 的量级由 dsh 本体决定，
与客户端无关；宣传时仍按 #20 的规则分开写工况。

**没做**：运行时裁剪（1.3G 里大头是没被用到的包）、Developer ID 签名与公证、
自动更新、图标（现在是 GPUI 默认）。

**安装位置（后续补充）**：`build-product.sh` 默认把产物 `ditto` 安装到
`/Applications/cowork.app`（`COWORK_INSTALL_DIR` 可覆盖）；旧安装移入废纸篓
（可恢复），不直接 rm。实测：`/Applications` 版无环境变量启动，包内
`Resources/runtime` 与 `node-path.txt` 查找正常，窗口、host、`kill -9` 收尸均正常。

---

## #40 复用本机已在运行的 dsh web：自签 cookie + loopback 发现

> **已被 #41 取代**：attach / 发现子系统整体删除；本条保留作历史记录。
> 协议形状结论（v1/v2 两种身份、签名形状）仍是 host 侧事实，见 PROTOCOL。

**日期**：2026-11

**决策**：窗口启动时先看本机是不是已经有一个**同一个 `$DSH_HOME`** 的 dsh web
在跑。有就 attach 上去，**不新起第二个 host**；没有才自己拉（#39）。
用户不用记端口、不用贴 URL、不用先在终端里起服务。

优先级固定：

| 顺序 | 条件 | 走哪条路 |
| --- | --- | --- |
| 1 | `DSH_READY_URL` 带 token | `/connect` 换 v2 cookie（开发期老路） |
| 2 | `COWORK_ATTACH=<http://127.0.0.1:PORT>` | 自签 v1 cookie attach |
| 3 | 自动发现命中 | 同上 |
| 4 | 都没有，或 `COWORK_NO_ATTACH=1` | 自己 spawn（#39） |

`COWORK_NO_ATTACH=1` 是逃生口：想强制隔离、或嫌自动发现慢时用。

### 为什么 token 不够用

`dsh` 每个进程随机生成一个 launch token，**只在自己进程内存里**，ready URL
打印到它自己的 stdout。别人开的 web host，我们读不到那行输出，所以「拿 token
走 `/connect`」对已有 host 不成立。

但 cookie 的真源是持久的：`$DSH_HOME/.credentials.yaml` 里
`client-connection/browser-session` 记录的 32 字节签名密钥（同一 harness home
的所有进程共享）。照官方形状自签一枚 v1 cookie 就能进去。**这同时天然校验了
归属**：不同 `$DSH_HOME` 的 host 验签不过，回 401，直接跳过。

### 官方认证形状（逐条从上游产物核对）

权威来源：`.runtime-staging/node_modules/@deepseek-ai/dsh-client-connection/lib/index.js`。
注意官方源码包叫 `browser-auth.ts`，发布产物是打包后的 `index.js`。

| 项 | 值 |
| --- | --- |
| 秘钥记录 | `credentialKey("client-connection", "browser-session")` → `payload.secret`，base64url，32 字节 |
| cookie 名 | `dsh-auth-` + base64url(sha256(authority)) |
| cookie 值 | `v1.<base64url(body)>.<base64url(HMAC-SHA256(secret, body))>` |
| body | `{"version":1,"authority","issuedAt","expiresAt"}`，两个时间都是整数毫秒 |
| authority | 非 TLS 取 `url.host`（**含端口**）；TLS 取 `url.origin`（含 scheme） |
| 有效期 | 官方默认 `cookieMaxAgeDays = 30`；我们签 29 天避开 `>` 边界，`issuedAt` 往前退 1s 抗时钟偏差 |
| 验签 | 常量时间比较；`v2` 额外要求非空 `id` |

**两个版本不是「新旧关系」，是两种身份**（这点 #19 当初留了「待测」）：

- `v1` = 浏览器会话，`decodeCookie` 校验后落到 `HostConnectionService` 的
  `browserPeer`（`principal = {kind:"owner"}`，`interactions = "answer"`）；
- `v2` = `/connect` 签发的独立 API operator，带 host 生成的 `id`，
  `authenticateApi` 只认它。

所以**自签 v1 不需要也不能自己造 operator `id`**（#19 的待测项就此结案）。
而且 `browserPeer` 不在 `peers` 表里，`admitConnection` 对它直接放行，
不会像 `/connect` 的独立 peer 那样撞 #35 那个 409。「窗口进程里一个 socket」
的 `mux` 约束仍照旧。

### 自动发现怎么做（不猜端口文件）

查过 `~/.dsh/storages/`、`profiles/web/`、进程环境：dsh **不把监听端口落盘**。
于是 `crates/dsh-host/src/discover.rs` 走两步，全程只读：

1. `lsof -nP -iTCP -sTCP:LISTEN` 取 loopback 的 LISTEN 端口；只保留
   `127.0.0.1:` / `localhost:` / `[::1]:`，且进程名是脚本运行时
   （`node` / `bun` / `deno` / `dsh`）——不对局域网地址、不对系统服务发请求。
2. 每个候选发两次 `GET /`：
   - 不带 cookie 必须回 **401** 且 body 含官方固定文案
     `dsh web authentication required`（`writeUnauthorized`），这是 dsh 独有指纹；
   - 带自签 cookie 必须回 **200**。

两次都是 `GET /`，400ms 超时、最多读 2KB，无副作用。`Connection::attach()`
还会再发一次 `session/list` 做最终校验（只读），失败就报握手失败，不静默。

**为什么不缓存端口**：端口每次 `--port 0` 都变，缓存只会是过期数据；
loopback 扫描本机几十个 socket 是毫秒级，比维护缓存与 PID 复用防护简单。
这也是 #19 那套「ready URL 缓存到 Application Support」没落地的原因。

### 归属与生命周期（别把别人的 host 杀了）

`Startup` 里的 `process: Option<HostProcess>` 是唯一主人标记：

| 路径 | `process` | app 退出时 |
| --- | --- | --- |
| 自己 spawn（#39） | `Some` | 看门狗 / `Drop` 收掉 node |
| attach（本条） | `None` | **什么都不做**，别人的 host 继续活 |

单实例锁（#39）仍按 profile 生效，但那是「一个窗口」，不是「一个 host」：
两个不同 profile 的窗口可以 attach 到同一个 host，各有一条流，互不冲突。

### 实测（2026-11，rc.3）

先在终端开一个普通 web host：

```sh
DSH_HOME="$HOME/.dsh" NO_COLOR=1 \
  node ./.runtime-staging/node_modules/@deepseek-ai/dsh/lib/bin.js \
    --profile web --no-open --port 0
# dsh web: http://127.0.0.1:50837/?token=BJykExygF0hQNJgWOJAqVKleFYRYtliJGoP0_nQW4n8
```

窗口不带任何环境变量启动，日志：

```text
复用已在运行的 dsh host origin=http://127.0.0.1:50837 home=/Users/x1a2h1/.dsh
会话就绪 origin="http://127.0.0.1:50837/" session="session-…"
已连接 host origin=http://127.0.0.1:50837 …
```

| 检查 | 结果 |
| --- | --- |
| 独立 Node 脚本按官方公式签一枚 cookie | `GET /` 与 `POST /api/session/list` 都 **200**；不带 cookie 的 `GET /` 是 401，证明签名形状与官方一致 |
| Rust 单测固定输入复算 name / body / 签名 | `dsh-host` lib 测试通过（见 `attach.rs`） |
| `dsh_host::discover` 同 home | `["http://127.0.0.1:50837"]` |
| 换一个不同密钥的 home | `[]`（401，跳过） |
| `Connection::attach` 一元调用 | `session/list` 200，133→138 个会话（多次启动会新建会话） |
| `session/list` 流 | 1 帧正常收尾，**无 409** |
| 窗口起来后 `ps` 里的 `dsh/lib/bin.js` | 仍只有那一个 PID 70472 |
| `kill -9 <窗口>` 后 | PID 70472 与 50837 端口都还在 |
| `COWORK_ATTACH` 显式指定 / `DSH_READY_URL` 老路径 | 均实测「会话就绪 + 已连接」 |
| `COWORK_NO_ATTACH=1` 隔离 home | 自己 spawn（45s 内 ready），`kill -9` 后看门狗收掉那个 node，外部 host 不受影响 |

`attach` 失败（host 刚好在探活之后挂了）会在界面显示「握手失败」，
不会自动降级去 spawn——避免用户以为连的是自己那个 host。

### 否决项

| 方案 | 否决理由 |
| --- | --- |
| 扫所有 loopback 端口（不看进程名 / 不认 401 指纹） | 会往微信、SSH 隧道、代理等本地服务发请求；噪声大且可能读回 200 误判 |
| 缓存 ready URL 到磁盘 | token 是进程级的，host 一重启缓存即失效；还要处理 PID 复用与文件权限（0600），收益却只有毫秒级 |
| 自己造 `operator id` 冒充 v2 | `authenticateApi` 只认 host 签发的 v2；伪造只会 401，且违反 #19 的保守原则 |
| 用 host 的 stdout 抓 URL | 那是别人进程的 stdout，读不到；`DSH_READY_URL` 只能靠用户手贴 |
| attach 到不同 `$DSH_HOME` 的 host | 会话/凭据根本不是同一份，界面会显示别人的数据；签名校验正好挡住 |
| 复用时把 host 一并收掉 | 用户终端里开的服务不该被 GUI 窗口关掉 |

### 没做

- 只识别 loopback；绑定 `0.0.0.0` 的 host 不涉及（`lsof` 显示 `*:PORT`，被过滤）。
- 只识别明文 loopback HTTP。`audience()` 按官方 TLS 口径（`url.origin`）算好并
  有单测，但 workspace 的 `reqwest` 是 `default-features = false` 且没开 TLS
  feature，**https origin 目前连不上**；`COWORK_ATTACH=https://…` 不要当成可用路径。
- `lsof` 不存在时静默返回空、照常自己 spawn（Linux 也给了 `/usr/bin/lsof` 与 PATH 两个候选）。
- 不做跨机器、不做 mDNS、不读浏览器 cookie jar。
- 自动发现是同步的（启动时一次），不常驻轮询。

## #41 对齐官方 desktop：永远 own host，只共享状态

**日期**：2026-11

**决策**：删除 attach / loopback 发现子系统（#40），产品启动永远自己 spawn
host（`dsh --profile cowork`），无视本机已在跑的 dsh web。`DSH_READY_URL`
降级为开发期逃生口。host 与 app 同生共死（#39 看门狗、单实例锁、播种规则全保留）。

**第一性原则：「复用」分两层**：

| 层 | 内容 | 归属 | 理由 |
| --- | --- | --- | --- |
| 状态层 | 会话/工作区/凭据/设置（`$DSH_HOME` 顶层） | 共享 | 用户资产，跨表面互见才是真复用 |
| 运行时层 | host 进程/profile/peer 身份/连接凭据 | 独占 | 谁 spawn 谁负责；共享必然互踩 |

而 #40 复用的是进程，破坏运行时层三条不变量：

1. **身份不独占**：v1 cookie 落在共享 `browserPeer`，语义上「和用户浏览器里的
   tab 是同一个客户端」，approval/question 的 answer 路由共享。官方每次启动建
   独立 owner peer；我们的 `/connect` v2 同样是独立 operator peer。
2. **生命周期不归我**：别人的 host 死了我们断线；我们退出收不掉它；
   「app 关 = 全停」的心智模型不成立。
3. **凭据暴露面更大**：attach 读 home 级**持久**密钥（任何同用户进程可读）；
   spawn 的 launch token 走**私有 stdout pipe**、每次新生、不落盘。发现子系统
   是一整套「猜别人状态」的启发式，所有权让它失去存在理由。

**对齐官方的性质，不是机制**（官方 `dsh-desktop-host` 私有包 + Node IPC 不可用，见 #13）：

| 官方性质 | 官方机制 | 我们的等价物（spawn 路径已全部具备） |
| --- | --- | --- |
| 凭据每次新生、私通道 | Node IPC `desktop-auth` | 私有 stdout pipe 的 launch token → `/connect` |
| 独立 peer 身份 | 每启动建 owner peer | v2 operator peer |
| 生命周期同生共死 | 父通道断开即退出 | `/bin/sh` 看门狗 |
| 运行时独占 | 独占 `profiles/desktop` + 单实例锁 | `profiles/cowork` + Unix socket 锁 |
| 端口不撞 web | 系统分配 | `--port 0` |

所以对齐 = **做减法收敛**，不加任何新机制。

**删除清单（已执行）**：

- `crates/dsh-host/src/{attach,discover}.rs`、`examples/{attach,discover}.rs` 及其单测；
- `Connection::attach`、`Credential::Signed`、`audience()`、`HostError::{Attach, BadOrigin}`；
- `host.rs` 的 `Target` 枚举、`COWORK_ATTACH` / `COWORK_NO_ATTACH`；`Startup` 回到 `{ready_url, process}`；
- `main.rs` 的 `connect()` 分派折叠回单路径；
- `hmac` / `sha2` / `base64` 从两个 Cargo.toml 退役（仅 attach 用过）。

**保留**：token 交换、401 自愈、mux、看门狗、单实例锁、播种规则
（`--from-default-profile web` 仅 `package.json` 缺失才传）。

**复审修复**（独立 verifier 抓出的凭据入日志路径，均已修 + 回归测试）：

- `host ready` 的 INFO 日志原先带完整 ready URL（含 token）；现经
  `redact_token()` 脱敏（`token=***`），`wait_ready` 的 debug 回声与报错尾巴同处理；
- `reqwest::Error` 的 Display/Debug 默认含请求 URL，`/connect` 的 URL 带 token；
  边界统一 `without_url()`，网络层失败不再把 token 带进「握手失败」文案。

**实测**（一次性 `DSH_HOME`，外部 `dsh web` 在跑）：

| 场景 | 结果 |
| --- | --- |
| 外部 web 在跑 + 设 `COWORK_ATTACH`（应被忽略） | 日志「拉起 host」（seeded=false，播种路径正常），`ps` 两个 node、端口各异；会话就绪在自己 origin |
| `kill -9` app | 自己的 node 与看门狗 3s 内没了；外部 web 存活 |
| `DSH_READY_URL` 指外部 web | 「使用外部 host」，会话就绪在外部 origin，无新 node；app 退出后外部存活 |

**代价（如实）**：

- web 在跑时总 RAM = 客户端 + 两个 host。这是所有权的代价，官方同款；
  优势仍在客户端 ~90MB（Electron 壳几百 MB）与后续 host 瘦身。
- 冷启动秒级，官方同款；加载态已有。

**没做**：

- north star 的真正杠杆是「profile 即能力清单」：cowork 不需要 `frontend-static`
  这类只服务浏览器的插件，seed 后按客户端实际用到的 API 裁剪 cordis patch，
  先测量再删。
- 退出确认（官方 quit-inspection 等价物）：`Phase::Running` 时退出前就地确认，
  不静默杀运行中的工作。
- 官方「CLI 不能启动 desktop profile」是它对自己 CLI 的约束；我们不改 dsh CLI，
  cowork profile 约定只由本 app 启动。

**取代关系**：#40 的 attach 部分与 #19 的 detached 设想被本条取代；#39 的
看门狗/单实例/播种全部仍有效。#40 的协议形状结论（v1/v2 两种身份、签名形状）
仍是 host 侧事实，见 PROTOCOL。

## #42 模型选择对齐官方：文案、空目录、提供方故障

本客户端此前在这块是「自创口径」：没有选择时状态行写 `model —`，目录空时
下拉根本打不开（`!model_groups.is_empty()` 门控），提供方枚举失败被静默丢掉。
官方 `dsh-client-ui-model-selection` 不是这样，逐条对齐：

| 情况 | 官方 | 我们（已对齐） |
| --- | --- | --- |
| 没有选择 / 保存的选择不在目录里 | `trigger.fallback` = 「请选择模型」 | 同，且用正常字重（不是禁用态） |
| 目录空 | `empty.models` = 「暂无可用模型」 | 菜单照常能开，里面一行不可点的该文案 |
| 提供方列举失败 | `warning.groupLoad` = 「{name} 加载失败：{message}」 | 同，逐条作为不可点行附在目录之后 |
| 当前模型没有推理强度 | 隐藏强度文本 | 整格 `effort` 隐藏（原来是 `effort —`） |
| 发送 | 不拦，交给请求执行报错 | 同 |

**前提纠正**：`profiles/*/cordis.patch.yml` 是**补丁层**，内容 `[]` 表示不覆盖，
不是「把模型清空」。空目录的真实成因是没凭据、或提供方列举失败。实测（禁用
llm 服务的一次性 profile）：host 返回 `groups: []`、`failures: []`，
而 `default` 仍然是 `deepseek-flash/deepseek-official` —— 说明 `default` 是
「目录里有它时默认选它」，与「目录是否为空」无关，因此 `ModelCatalog.default`
保持必填，不改成 `Option`。

**多提供方排序**：`deepseek-account` → `deepseek-official` → 其余按目录原序。
官方把「DeepSeek 账号」与「API Key」做成两个提供方分组；稳定的顺序比目录
下发顺序更好按。

**为什么列出失败行**：有凭据但拉不到目录，只显示空目录会让人以为「没配」——
这两件事的修法完全不同（补 key vs 查提供方）。

**验证**：三个单测钉住行构造（空目录一行 muted、故障行 muted 且不叠加空态、
正常目录的 key/选中/hint），外加禁用 llm 的真实 host 上截图确认状态行
「请选择模型 › ctx — cache —」（`effort` 已消失）。

## #43 启动不建会话：主区空状态选工作区（对齐官方 hero 选择器）

**症状**：每个新会话都落进侧栏「未分组」，而且 `cwd` 是 `$HOME`。成因是启动
时无条件 `session/create`，`cwd` 由 `DSH_WORKSPACE` → 非 `/` 的当前目录 → `$HOME`
推出来；`.app` 从 Finder 或 `open` 启动时 `cwd` 是 `/`，于是永远落 `$HOME`。
`$HOME` 不是注册过的工作区，会话自然被归到「未分组」。

**官方**：侧栏 Ungrouped 连「新建会话」入口都不给（它没有后备工作区），新会话
的工作区来自「Session Intent 主视觉区」里的选择器（`hero.workspace`），
选中后新会话就在那个目录下。

**我们的做法（新增空状态，删掉自动建会话）**：

- 启动不再 `session/create`；主区显示空状态（品牌 + 提示 + 工作区卡片 +
  「添加工作区…」），点一行即 `NewSession{cwd}`（#37 已存在的命令）。
- 会话 `cwd` 只来自：空状态里选的工作区、侧栏工作区行的 `+`、`DSH_DEBUG_*`。
- 「未分组」保留原本语义：注册被删但会话还在（#35 只删注册），不是新会话的归宿。
- 握手成功但还没有会话时也要亮绿点：新增 `UiEvent::Ready { origin }`（放在
  `switch_to` 发的 `UiEvent::Connected` 之外），否则空状态会长期「未连接」。
- 无会话时标题栏不摆「未连接 / 新会话」，也不给没有归处的右栏开关。
- 调试口径：`DSH_DEBUG_OPEN` 仍直接打开指定会话（`cwd` 从会话列表补）；
  `DSH_DEBUG_PROMPT` 且没给 id 时按老规则建会话——仅调试，正常启动不触发。

**验证**：release `.app` + 真实 `$DSH_HOME`：空状态截图（工作区列表 +
「添加工作区…」）；合成点击落在第一行（660,482）后日志 `已连接 host … cwd=…/dsh-client-rs`，
标题栏 `dsh-client-rs / 8207d197`，侧栏把新会话归到 `dsh-client-rs` 组，
`未分组` 里只剩历史会话。点击本身走的是和侧栏 `+` 完全相同的命令通道。

**没做**：官方那种「hero 里选完工作区再在 composer 里说第一句话」的连续动效；
我们的空状态与聊天区是两块独立布局，选完即切。空状态也没有最近会话列表——
历史在侧栏里已经能点。

## #44 host 只在「有活」时存在：空闲就收掉

**问题**：north star 是把 desktop 的 RAM 减到极致，而实测（本机 macOS arm64、
Homebrew node、idle 无对话、三次取中位数）：

| 配置 | host RSS |
| --- | --- |
| 现行 `cowork`（= web 系 profile） | **385 MB** |
| 同上 + 关掉 55 个 `ui-*` | 384 MB |
| 同上 + 关遥测/dev/目录选择器/plugin-inventory | **366 MB** |
| 官方 `sdk-minimal`（纯 agent，无 Web/base） | **142 MB** |

即：**浏览器插件全砍也只省 5%**，剩下 ~250MB 是 agent bundle 本身（工具、skills、
office、ptc、subagent、mcp）。继续在插件里抠是低回报的。真正的浪费是——
**没有活要干时，把一个 385MB 的 agent 运行时留在内存里**。

**决策**：host 从「app 生命周期」改成「任务生命周期」。

- `apps/desktop` 新增 supervisor：拉起（含 handshake）→ 服务一代命令 → 空闲收掉
  → 等下一个命令再拉。命令不丢：休眠后收到的第一条当作下一代的第一条。
- 空闲阀值 `COWORK_SLEEP_IDLE_SECS`（缺省 600，`0` = 关）。到点先问一次
  `session/list`：**有轮在跑就不收**（服务端才是权威）；查询失败按「有活」处理。
- 醒来先接回上一个会话（`session/follow` 快照重放，与侧栏切会话同一条路径），
  会话不丢。
- 窗口不再等 host：`main` 不再阻塞在 `host::start()` 上（原来要等 2.4s 才开窗），
  先出窗口、后连 host。
- 界面不依赖 host 存活：侧栏/时间线/芯片都在客户端内存里，左下角标改显
  「host 休眠中」。新增 `UiEvent::HostState { awake }`。

**实测**（release `.app`，`COWORK_SLEEP_IDLE_SECS=20`，真实 `$DSH_HOME`）：

| 时刻 | 事件 | 读数 |
| --- | --- | --- |
| t=0 | 拉起 host | ready 2.6s |
| t=23s | 空闲收掉 | 客户端 99MB，node/看门狗都不在 |
| t=45s | 一条命令 | 重新拉起 → ready 2.3s → 同会话 `session/follow` 重放 |

**踩到的坑（已修）**：收 host 时 `workspace/follow` 的 WS 会报「连接关闭」，
原来它走 `UiEvent::Failed` → UI 收尾时发 `RefreshSessions` → **把刚睡下的 host
立刻叫起来**（日志里能看到 2s 内的打转）。现在用 `AtomicBool` 关停标志区分
「我们自己拆的」与真故障，拆除时静默退出、不重试。

**代价**：冷启/唤醒 2.3–2.6s（首个交互）；app 重启后侧栏要等 host 起来才填满
（本地快照缓存还没做，见下）。

**没做**：

- 交互预热：在 composer 聚焦/悬停时提前拉起，把 2.4s 藏起来。
- 严格休眠门：现在只看 `session/list.running`；后台 job / 子 agent / 待审批
  都靠 running 间接反映，将来应看更直接的信号。

### 侧栏快照（已做）

host 不在时侧栏不能空，所以加了一份客户端快照
`~/Library/Application Support/cowork/state.json`：会话摘要 144 条、工作区注册表、
归档与置顶集合，~26KB。启动时先 `try_send` 给界面，host 的真实数据到了再覆盖。

- **不存时间线**：历史只信 `session/follow`，免得拿旧快照冒充历史。
- **`running` 读回来归零**：否则上次强退的运行点会变成永久绿点。
- 写时机：会改侧栏的命令之后 + 休眠/退出前；打字引发的 `Complete` 不写。
- 原子替换（`.tmp` → rename），解析失败当无快照。

验证：把 `COWORK_NODE` 指到不存在的路径（host 根本拉不起来），界面仍然是
完整侧栏（`dsh-client-rs` 组 3 条、`未分组` 2 条、`已归档 7`）+ 空状态工作区卡片，
只有角标显「未连接」。

### 附：两项顺手做过的研究

**profile 裁剪（能省多少，实测）**：就是上面那张表。结论：纯浏览器插件全砍
≈5%，所以默认不做激进裁剪。有一条坑必须记住：`tool-plugin-manager` 是 agent
preset 的依赖，关掉它 `session/resume` 直接报 `agent-preset/invalid`。所以
任何裁剪必须由「客户端实际用到的 API 清单」驱动，并用探针（probe/model/ctx/
workspaces/archive）验证，不能按名字随手关。

**单文件运行时（去系统 node）**：官方 Python SDK 的做法是
`@yao-pkg/pkg --sea --compress Brotli --targets node24-macos-arm64`，外加两个
sidecar（`-rg` ripgrep、`-spawn-helper` node-pty），Cordis 的动态 import 靠
`pkg.assets` 整树 glob 兜住；macOS arm64 wheel 52–77MB。我们照抄即可去掉
`node-path.txt` 对 Homebrew node 的依赖，与 RAM 无关。

**顺带修掉的真 bug**：`session/list` 的 `SessionProjectionHints.asOfSeq` 在
持久化缓存来源时会回 `-1`，我们原来用 `u64` 接，导致整条响应解码失败、会话
列表直接空（只有多会话工况才撞得到）。改成 `i64` 并加了回归 fixture。

## #45 驻留体积：先分解，再调参，最后才谈携带方式

**第一性原则**：内存占用 = **体积 × 时间**。时间那一维已经用 #44 压掉了（没活就不留
host），这一条只治体积。体积再分两块，不能混着谈：

- **磁盘/文件数**：影响冷启，不影响 RSS。
- **RSS**：`V8 堆（活对象 + 回收区）` + `运行时地板（node/ICU/原生库/栈）`。

### 事实一：379MB 里 63MB 是纯浪费（V8 堆分解）

用 `--report-on-signal` + `SIGUSR2` 拿 node 诊断报告（`cowork` 系 profile、idle、RSS 379MB）：

| 项 | committed | used |
| --- | --- | --- |
| `new_space`（回收区） | **64.0MB** | **1.2MB** |
| `old_space` | 167.2MB | 157.7MB |
| `large_object_space` | 28.2MB | 26.7MB |
| `trusted_space` | 10.0MB | 9.3MB |
| `code_space` | 4.2MB | 3.8MB |
| 堆外（node/ICU/187 个 dylib/栈/malloc） | ≈105MB | — |

读法：**真活对象 ≈194MB**（装进来的 agent bundle），堆外地板 ≈105MB，
而 **63MB 留给了几乎全空的 semi-space**——那是 node 默认按「长时间跑的
服务端」调的参，桌面客户端用不上。

### 事实二：一个参数拿 17–18%（实测，三次取中位数）

| 配置 | idle RSS | 启动 |
| --- | --- | --- |
| 默认 | 371MB | 3.88s |
| **`--max-semi-space-size=4`** | **309MB** | 2.14s |
| `--max-semi-space-size=2` | 307MB | 2.35s |
| `--max-semi-space-size=1` | 310MB | 2.36s |
| `--max-semi-space-size=2 --optimize-for-size` | 292MB | 2.35s |
| `--max-semi-space-size=4 --max-old-space-size=384` | 301MB | 2.13s |
| `--v8-pool-size=1` | 328MB（**更差**） | 2.14s |

产品 profile 复测（`cowork`、真实 `$DSH_HOME`、144 个会话）：**417MB → 343MB**
（−74MB，−18%），启动 2.98s → 2.57s，没有倒贴。

**决策**：默认钉 `--max-semi-space-size=4`。

- 不钉 `--max-old-space-size`：能再省 8MB，但长上下文/大文件真撞上限就是
  OOM 崩宿主，不值。
- `--optimize-for-size` 能再省 ≈17MB，但它改的是一般的 JIT 策略（CPU 代价
  未量），留作实验开关，不进默认。
- 功能回归：同一套探针在参数下 probe/model/ctx/workspaces/archive 全 PASS。

### 事实三：1.4GB 磁盘里有一半是「跟着包走的可选载荷」

| 包 | 大小 | 对 desktop 客户端有用吗 |
| --- | --- | --- |
| `libreoffice-kit-darwin-arm64` | 156MB | 只在跑 Office 技能时 |
| `libreoffice-kit-wasm` | 145MB | 与上一行**重复**（同一能力的 WASM 版） |
| `dsh-experimental-inspector` | 28MB | 否 |
| `dsh-client-ui-sidebar-documentpreview` | 23MB | 否 |
| `dsh-web-frontend` | 20MB | **否（我们不做 Web）** |
| `dsh-experimental-client-ui-forge` | 13MB | 否 |
| `dsh-experimental-liveshare` | 12MB | 否 |
| `dsh-experimental-cua-macos` | 12MB | 否（未接） |

原生件：17 个 `.node` + 4 个 `.dylib`（node-pty 等），外加 `-rg` 类可执行体——
这些是单文件打包的主要难点（SEA 不能从内存 dlopen）。

### 所以优雅的次序是：调参 → 裁闭包 → 按需下载 → 再谈单文件

1. **V8 参数**（本条，已完成）：−74MB，不改代码不裁功能。任何「先重构再优化」
   的方案不如这一行。
2. **闭包裁剪**：去 wasm office（与 darwin-arm64 重复）、去 `web-frontend`、
   去 `experimental-*`，目标磁盘 −400MB；活堆能降多少必须探针验，不靠拍脑袋。
3. **重载荷按需下载**（对齐官方 `download_office()`）：Office kit / CUA 这类
   不常用的大件放到 `~/Library/Application Support/cowork/resources`，
   不进安装包。
4. **单文件运行时**：`pkg --sea --compress Brotli`（官方 0.2.2rc2 的 macOS arm64
   wheel 实测 87.3MB）。这件事只买「冷启/安装体积」，不买 RAM——别把它当内存方案。

## #46 应用图标：官方 mark + 我们自己的深色底

**需求**：`cowork.app` 在 Dock / Finder / 切换器里不能是个白纸图标。

**可选的两条路**：

1. 直接拿官方 `icon-512.png`（深色鲸鱼 + 浅底）转 icns。最忠实，但深色 UI 的 app
   在 Dock 里会发一块白板，跟 `#0F0F10` 界面不像一家人。
2. 用官方矢量重新构图：白色 mark + 深色 squircle。

**选 2**，且图形一律不自己画：

- 鲸鱼逐字来自 `@deepseek-ai/dsh-web-frontend/dist/favicon-dark.svg`（官方为深色背景
  准备的白色 mark），入库为 `crates/dsh-client-ui/assets/deepseek-mark-white.svg`，
  与品牌锁同源。
- 底：`#1C1C21 → #0A0A0C` 竖向渐变，`#FFFFFF` 7% 内描边当反光，跟界面同色系。
- 网格按 macOS Big Sur：1024 画布、824×824 方块、圆角 185、图形高占 0.47。
  上下留白对称，图形包围盒中心对齐方块中心。

**过程中踩到的两个坑**（都写进脚本注释了）：

- 官方 mark 里 `id="path"` 也含子串 `d="`，用 `awk` 取第一个 `d="…"` 会拿到字符串
  `path`，结果圆角方块光秃秃的。改成贪婪取最后一个 `d=` 属性。
- 机器上 `rsvg-convert` 有两份：Mono 自带的（dyld 报 `NSFontManager` 符号缺失，直接
  abort）与 Homebrew 的。脚本显式优先 `/opt/homebrew/bin/rsvg-convert`。

**产物与接线**：

- `scripts/make-icon.sh` 生成 `apps/desktop/assets/AppIcon.icns`（10 个尺寸）与
  `AppIcon-1024.png`（预览）；`.icns` 入库，构建不需要再光栅化。
- `scripts/build-product.sh` 把它拷进 `Contents/Resources/`，`Info.plist` 加
  `CFBundleIconFile = AppIcon`；缺图标则直接报错，不允许静默出一个白纸包。

**验证**：重新打包安装后，用 `NSWorkspace.shared.icon(forFile:)` 渲染系统实际会用的
图标（就是 Dock 里那一帧）——深色圆角块 + 白色鲸鱼，无白底。`iconutil -c iconset`
反向解包也能看到 16→1024 共 10 个尺寸齐全。

## #47 工具调用：两条事件投影成一行，过程组是渲染期派生的

**起点**：时间线原来只认 `user/message` 与 `assistant/message`，`tool/call`
`tool/result` 直接 `_ => {}` 丢掉。code 会话的主体恰好是工具调用，所以这不是
「再加个组件」，是补上时间线的主干。

**第一性推导**：用户要的不是「事件列表」，是「agent 干了什么、结果怎样」。
事件流里这两件事被拆成两条（`tool/call` 携带意图，`tool/result` 携带结果，
靠 `callId` 相连），于是**一行 = 一次调用的投影**，而不是两条事件两行。
这条投影必须只存在一份：历史回放（`session/follow` 快照）与实时流如果各写一套，
同一轮对话在两端的折叠结果会不一样——这是本项目反复踩过的坑（#16/#17）。

于是模型层落在 `crates/dsh-protocol/src/transcript.rs`：

- `Entry = User | Assistant | Tool | Notice`，`Transcript::push_event(&Value)`
  是唯一入口；遇到不认识的 `type` 返回 `None`（开放轴降级，DECISIONS #8）。
- `tool_index: HashMap<callId, index>` 保证同一个 `callId` 重放两次（快照 +
  增量）不会长两行；孤立 `tool/result`（没有 call）直接忽略。

**三个实测出来的坑**（都是先按官方文档写、再拿真实会话纠正的）：

1. `tool/result` 的 `callId` **不在 `data` 顶层**：v4 会话里是
   `data.message.toolCallId`，`message.source.callId` 也有；老数据才是 `data.callId`。
   原来只认顶层，结果历史里每一行都停在「运行中」。现在按三个位置回退。
2. 失败标记是 `data.message.isError`（布尔），不是 `data.error`。两个都认。
3. `bash` 的 `description` 是模型写的英文一句话，`command` 才是真正执行的命令。
   命令行类工具优先取 `command`；其他工具仍走 `description` 优先。

**三档展示**对齐官方 `ui-chat.transcriptView` 的口径，但**存在我们自己这边**
（profile 里没有 `ui-chat` 插件，读不到那个设置）：

| 模式 | 组头 | 组体 |
| --- | --- | --- |
| 简洁 | 只给类别 | 收起 |
| 标准（缺省） | 类别 + 数量 | 收起 |
| 详细 | 类别 + 数量 + 「正在跑 X」 | 运行中展开，历史收起 |

**任何档位都不自动灌输出**——官方也是这个规矩：档位只改过程组的组织方式，
单个工具的正文永远点开才看。所以折叠状态不入库、不跨会话保留（一次误点不该
跟你一辈子），也没必要做。

**过程组不进模型**：一轮内连续的工具行归一组，中间夹一句助手正文就断开，
这是 `tool_runs()` 在看的时候算出来的。好处是切档位/展开/收回都不碰数据，
也不会因为模式切换而重新请求会话。

**两个边界**：

- 单行正文只留前 32K 字符（`DETAIL_LIMIT`，带「输出过长」标记），界面再限
  24 行——`cargo test` 的输出可以几 MB，不能整个塞进 frame 里排版。
- 实时流只把**工具**条目单独推给界面（`UiEvent::Tool`，按 `callId` upsert）；
  用户与助手正文仍走快照/流式帧，否则同一条消息会出现两次。

**验证**：`cargo test --workspace` 72 通过（新增 6 条：配对、失败、重放去重、
孤立结果忽略、老形状兼容、长输出截断、分组断开）；真实会话
`session/9dae3216`（两条 `bash`）在 release `.app` 里回放出两行、摘要为命令本身、
展开后是完整输出；`detailed` 档运行中组体自动展开，`standard` 档历史收起；
组头点击展开、右栏图标点点开均已在屏幕上验过。

**没做（明确留到下一期）**：

- `verbose` 档（官方给调试用，逐事件展开）——我们的定位是 code 主力，详细档
  已经够用，真需要时再加。
- 专用行：`edit` 的 diff 视图、`read` 的行号窗口、子 agent 的嵌套过程组。
  这些需要 `tool/result.data.meta` 的展示载荷（DESIGN 已定好承载位置），
  属于「对齐官方工具卡片」的独立一期。
- 折叠状态入库、按工具类别的折叠偏好，以及**档位写盘**（现在只读
  `COWORK_TRANSCRIPT_VIEW`，重启回到缺省）。

## #48 助手正文的 markdown：用 gpui-base 自带的 TextView，身份用 (turn, step)

**需求**：#47 之后工具行能看了，但助手正文还是纯文本直出——粗体的星号、行内码的
反引号、三反引号代码围栏都原样显示。code 会话里这份内容占比很大，必须渲染。

### 为什么不自己写

候选就三条，排除两条：

| 候选 | 结论 |
| --- | --- |
| Zed `crates/markdown` + `markdown_preview` | ❌ GPL-3.0-or-later；依赖 `project`/`workspace`/`editor`/`db`/`ui`/`theme` 整个编辑器 |
| crates.io `gpui-markdown` / `gpui-md` | ❌ 0.0.1、个位数下载、单人新仓，撑不起正文 |
| **`gpui-base` 的 `TextView::markdown`** | ✅ Apache-2.0，**已是 gpui-kit 的必选依赖**（不新增依赖树），解析器 `markdown 1.0.0`（wooorm/markdown-rs，MIT） |

`gpui-kit/src/lib.rs:130` 就是 `pub use ::gpui_base as base;`，所以入口是
`gpui_kit::base::text::TextView`，不需要开 `component` feature。

### 四条前提推出四个决定

1. **解析是纯函数，排版是状态。** 状态必须挂在「消息身份」上，不能挂数组下标。
2. **身份是 `(turn, step)`。** 证据：`StreamStart.turn/step`
   （`crates/dsh-protocol/src/stream.rs:157`）与持久 `assistant/message.data.turn/step`
   （实测 v4 会话）两边都有；`seq` 在流式期还拿不到，下标会在重放/compaction 时漂。
   于是 `Entry::{User,Assistant}` 带上 `MessageId`，`TextView` 的 `ElementId` 用它
   （`TextView` 内部 `window.use_keyed_state`，`text_view.rs:740`）。
   **效果：流式正在写的那一条与提交后的持久消息是同一个 key，提交那一帧不重建排版、不闪。**
   `user/message.data` 没有 turn/step（实测），退回事件 `seq`；本地刚发出去的提问用
   `MessageId::local()`（高位打标志，与真实 seq 永不撞）。
3. **只有助手正文解析 markdown。** 工具输出是字节流（`*`、diff 的 `-`），解析会改语义；
   用户消息写什么显示什么。这条边界写在 `markdown` 模块头注释里。
4. **开销 ∝ 存活排版状态数**。所以将来的优化方向是虚拟化（DESIGN 里的 P0），
   不是降级渲染器；要临时关掉就给一个同入口的降级分支 `COWORK_RICH_TEXT=0`。

### 落地

- `crates/dsh-protocol/src/transcript.rs`：`MessageId`（`Turn{turn,step}` / `Seq` / `local`），
  `Entry::{User,Assistant}` 带 id；`fold.rs` 记录 `turn_step()`。
- `crates/dsh-client-ui/src/markdown.rs`：唯一正文入口 `markdown::text(id, source)`，
  `font_family(MONO)` + `theme::markdown_style()` + `selectable(true)`。
- `crates/dsh-client-ui/src/theme.rs`：`markdown_style()` 是样式单一真源。
  `TextViewStyle::default()` 是浅色 token，**必须 `with_dark(true)`** 并把
  行内码/表格从白底黑字压回卡片底 + 墨色。
- 探针 `crates/dsh-client-ui/examples/markdown_window.rs`：直接用产品入口渲染一段
  全要素 markdown（标题/粗斜/行内码/围栏码/嵌套列表/任务列表/引用/表格/分隔线），
  改样式时先看它。

### 实测（release `.app`、同会话、三次读数一致）

| 会话 | 纯文本 | markdown | 差 |
| --- | --- | --- | --- |
| 25 条短消息 | 110.2MB | 111.4MB | +1.2MB |
| 2 条（含代码块 + 列表） | 96.2MB | 102.9MB | +6.7MB |

代价跟随 **markdown 结构量**（代码块/表格的排版树），不随消息条数线性涨。
两条会话都在 95–112MB 区间，没有离开 north star 的口径。

### 验证

`cargo test --workspace` 76 通过（新增 4 条：助手身份是 turn/step、user 退回 seq、
本地 id 不撞真实 seq、start 帧携带身份）；clippy 0；真实会话回放：围栏码带语言标签、
行内码胶囊、有序列表粗体都对；新发一轮在 release `.app` 里流式渲染正常，提交后不跳。

### 本期的空白

- **代码块复制按钮**（`code_block_actions`）与语法高亮（会拖 tree-sitter 常驻内存）。
- **图片**（`image_source` 不接）。
- 时间线虚拟化——80 条以上消息的会话还没量；真撞上就按 DESIGN 的 P0 上虚拟化，
  而不是砍渲染。

## #49 模型目录来自 profile：`llm-pi-ai` 承载第三方路由，`selectModel` 的可选字段不能发 `null`

桌面端还没有设置面板，模型只能落在 profile 层。这轮把「换模型」的链路一次摸到底，
顺手修了一个只在非官方模型上才会撞的协议 bug。

### 事实（都是实测，不是文档字面）

1. **`agent-default-model` 只决定新会话。** `provider` / `model` / `reasoningEffort`
   三个字段，换模型时整个 `config` 是**全量替换**而不是合并。
2. **UI 切模型会回写默认值。** 点模型芯片走 `session/selectModel`，host 内部
   `agentDefaultModel.saveSelection()`（`dsh-api-session-controller` 的注释写明
   "save the default in the background"），落盘靠 base 已挂的 `config-editor`。
   实测：手写的 `deepseek-flash` 被 host 改写成了 `deepseek-v4-pro` + `reasoningEffort: high`
   —— 所以「desktop 保存不了模型」是误会，它只是没有设置面板。
3. **第三方模型不需要改代码。** `dsh-base` 已经以「零路由休眠」挂了 `dsh-llm-pi-ai`，
   给它 `config.providers` 就有路由。自定网关（pi-ai 目录里没有的）**必须**自带
   `api` / `baseURL` / `models`，模型条目认 `id` / `name` / `contextWindow` / `maxTokens` /
   `input` / `reasoningEfforts`；`reasoningEfforts` 要么声明等级（值 = 协议里发送的拼写，
   只有 `off` 可为空），要么 `false` 表示非推理模型，**不能是全空对象**。
4. **凭据只写引用，真值有专门的位置。** 路由里只出现 `apiKeyEnv: MAGPIE_GATEWAY_KEY`；
   magpie 会把真值写进 `~/.dsh/.credentials.yaml` 的 `refs:` 段（0600）。解析顺序：
   凭据存储 → 启动环境 → 项目 `.env` → `~/.dsh/.env`。patch、日志、UI 都不出现密钥。
5. **这一整段交给 magpie，不手写。** 它支持 DeepSeek Harness，会自己往
   `~/.dsh/profiles/*/cordis.patch.yml` 注入三个条目：`agent-default-model # magpie`、
   `llm-pi-ai`（两个路由：`magpie`：`openai-completions` + `/v1`；`magpie-anthropic`：
   `anthropic-messages`，共 23 个模型）、`web-search-deepseek # magpie`。
   **手写会被它覆盖**（实测：我手抄的那份 `providers` 被整个重写成它的版本），
   所以换模型用 `magpie dsh <provider/model>`、摘下用 `magpie dsh default`、
   看候选用 `magpie models dsh`。
6. **magpie 认“dsh 命令”，不认 app bundle。** 它通过 `dsh` 命令确认 harness 已安装；
   找不到时照写 patch 但**不写凭据**（`magpie ls` 显示 `CLI not found`）。
   cowork 没装全局 dsh，所以放了个 wrapper：`~/.local/bin/dsh` →
   `/Applications/cowork.app/.../node_modules/@deepseek-ai/dsh/lib/bin.js`。
   装上它之后 `magpie dsh <model>` 才同时写 `refs.MAGPIE_GATEWAY_KEY`。

### 修复：`reasoningEffort: null` 被边界校验拒收

`session/selectModel` 的请求体是 `{sessionId, provider, model, reasoningEffort?}`，
`reasoningEffort` 是 **可选字符串**（`z.string().optional()`），不是可空字段。
我们原来无条件塞 `"reasoningEffort": effort`，`effort` 为 `None` 时发的是 `null`，
host 回 `gateway/input-invalid: wire field "request" failed boundary validation`。
官方两个模型都有目录默认等级所以从没踩到；magpie 的 `gemini-4-hydrogen` 一选就撞。
修法是 `None` 时整条省略（`apps/desktop/src/main.rs` 的 `UiCommand::SetModel`）。

### 实测

| 动作 | 结果 |
| --- | --- |
| patch 写 `deepseek-v4-pro` → 新建会话 | 芯片 `DeepSeek-V4-Pro` |
| patch 写 `deepseek-flash` → 新建会话 | 芯片 `DeepSeek-V4.1-Flash` |
| `DSH_DEBUG_MODEL=remote-magpie/agy-cli/gemini-4-hydrogen` | 芯片 `Gemini 4 Hydrogen`，真实回复正常 |
| `magpie dsh remote-magpie/group/ds-4.1-flash` | patch 被改写（`# magpie` 标记）、`credentials.yaml` 新增 `refs.MAGPIE_GATEWAY_KEY` |
| 删掉 `~/.dsh/.env` 后重启（纯 magpie 链路） | 新会话芯片 `ds 4.1 flash`，回复「我是 remote-magpie/group/ds-4.1-flash 模型」 |

### 纪律

**不手写 `cordis.patch.yml`。** cowork 的这个文件现在只有 magpie 写的 3 个条目，
连注释都不加（magpie 重写条目时会连带删掉紧贴其上的手写注释）：

```sh
magpie ls                       # 它在管哪些 agent、当前模型
magpie models dsh               # cowork 能选哪些模型
magpie dsh <provider/model>     # 换默认模型（新会话生效；旧会话保持自己的）
magpie dsh default              # 把 magpie 从 dsh 里摘出去
```

代价是默认模型跟着 magpie 走（当前 `remote-magpie/group/ds-4.1-flash`）。
想回到官方 `deepseek-official / deepseek-flash` 而不和 magpie 抢这个文件：
`magpie dsh default` + 在 UI 里点一次官方模型（host 会自己写 `agent-default-model`）。

## #50 mode 下拉从 statusline 挪进输入卡片的按钮行

**日期**：2026-11

**决策**：`mode`（权限 preset 下拉）不再站在 statusline 左端，改成输入卡片里的
按钮行左侧（`输入框下面那行：workspace-write ⌄ ……… ↑`），
那一行右端仍是 send / stop。statusline 左端改放**当前工作区名**（只读文本，
`INK_MUTED`、超 280pt 截断），右侧仍是 `model` · `effort` · ctx 环 · cache，
整行 `justify_between` 贴着内容列两端。

**为什么**：`mode` 是「这一轮怎么执行」的开关，与发送动作同属一次提交；把它贴到
发送按钮那一行，视线不用在输入框和下面那行之间来回跳。另外 statusline 因此少一个
可点控件，左侧留白让右侧读数更像「仪表」而不是工具条。

**位置细节**：芯片文字左沿与输入文字同一条线（卡片内边距 12，芯片自身无 padding），
行高由 24pt 的圆形按钮撑开；字号沿用 `META` 11，颜色 `INK_SOFT`、hover/展开时 `INK`。
`pull_menu` 的弹出方向不用改——它在触发器上方 26pt 打开，落到输入区之上，
卡片没有 `overflow_hidden`，不会被裁掉；z 序上 chips 行在卡片之后绘制，但两者不重叠。

**代价**：与官方 desktop 的 composer 摆法不同（官方在 statusline 左端）。
这是有意的偏离，按 #47/#48 的同一口径：位置跟操作距离走，不跟设计稿逐像素走。

## #51 composer 换成多行输入：Enter 发送，Shift+Enter 换行

**日期**：2026-11

**决策**：输入框从 `InputState`（单行）换成 `TextareaState`（多行），
构造时 `submit_on_enter(true)` + `set_auto_grow(1, 6)`：

```rust
let mut state = TextareaState::new(window, cx)
    .placeholder("给 dsh 说点什么…")
    .submit_on_enter(true);
state.set_auto_grow(1, 6, cx);   // 1 行起，长到 6 行，再高就卡片内滚动
```

**为什么**：`Shift+Enter` 换行是聊天输入框的默认约定；单行输入框里它什么也不做。
而且这一步不需要自己拦键——`InputState = InputBaseState<InputMode>` 的
`MULTI_LINE = false`，`TextareaState` 的 `MULTI_LINE = true`，库在
「多行 + `submit_on_enter`」时已经是这个口径：裸 `Enter` 只发
`InputEvent::PressEnter`（不插换行），`Shift+Enter` 插换行（同时仍发一个
`shift: true` 的事件，我们的处理器只认 `shift: false`，所以不会误发）。

**实测**（release `.app`，`DSH_DEBUG_KEYS` 合成按键）：

| 动作 | 结果 |
| --- | --- |
| 草稿 `abc` + 按 `Enter` | 日志 `测试模式：拦下这次投递 chars=3`，卡片保持 1 行——没有插换行，走了发送 |
| 草稿 `第一行文字` + `Shift+Enter` | 卡片从 1 行（78pt）涨到 2 行（101pt），文字落到第二行（`set_value` 把光标放在 0，换行插在它前面） |
| `DSH_DEBUG_COMPLETE=$'第一行\n第二行'` | 两行同时渲染，卡片高度按内容自适应 |

**副作用**：草稿可以带换行，`send` 只 `trim` 两端，内部换行原样进 prompt；
卡片超过 6 行后在输入框内部滚动，不再继续挤压时间线。

**调试**：`DSH_DEBUG_KEYS` 白名单加了 `shift-enter`（原先只有 up/down/enter/tab/escape/shift-tab），
这个开关跑起来时 `send` 仍自动进测试模式，不会真投递。

## #52 runtime package：app 与运行时解耦

**日期**：2026-11

**决策**：dsh runtime 不再以 loose `Resources/runtime/node_modules` 的形式属于 app 体积。
它被打包成一个不可变的 `.coworkrt.tar.gz`：里面有固定版本 Node、dsh 生产依赖树、
`runtime.json` 和唯一入口 `bin/cowork-host`。app 只带 `runtime-manifest.json`，
按清单校验 hash 后安装到 `~/.cowork/runtimes/<id>/`，再启动 launcher。

**为什么**：

- 当前全量依赖树是 1.3GB，占 app 体积的 93%；继续塞进 app 只会把 Node 再加 90MB。
- 官方 Desktop 用 Electron 兼作 Node runtime；我们是原生 GPUI 客户端，没有 Electron UI 可复用，
  为 runtime 单独塞 Electron 不划算。
- app 版本与 harness runtime 版本的变更频率不同，应该能分别升级；#18 的版本钉死通过 manifest 保留。
- 用户状态在 `~/.dsh`，runtime 是可替换缓存；两者不能混在一个目录里。

**入口契约**：

```text
runtime-manifest.json → id/target/archive/sha256/size/launcher/url
runtime package       → runtime.json + bin/cowork-host + node/ + dsh/
app                   → ensure runtime → spawn bin/cowork-host → 接 ready URL
```

`bin/cowork-host` 自己知道 Node 与 dsh 入口，app 不再拼 `node bin.js ...`。
`COWORK_RUNTIME_URL` 可以覆盖在线下载地址；内嵌 archive 优先于下载。

**发行形态**：

| 模式 | app 内容 | 行为 |
| --- | --- | --- |
| `legacy` | 完整 `Resources/runtime` | 兼容旧构建；默认保留直到 runtime package 正式发布 |
| `embedded` | manifest + archive | 离线双击即用，首次启动安装到用户目录 |
| `plain` | 仅 manifest + URL | app 最小，首次启动下载 runtime |

**硬规则**：

- manifest 的 `target` 必须等于当前平台，否则拒绝启动。
- id 与 archive/launcher 路径受限，防止 manifest 越界写文件。
- 安装前校验 archive size 与 SHA-256；安装后校验 `runtime.json` 的 schema/id。
- 已安装 runtime 有唯一目录；损坏目录先改名隔离，不原地覆盖。
- 安装锁带 PID：活进程持锁时拒绝并发安装；崩溃留下的陈旧锁和未完成 `.install` 目录可回收。
- 不自动删除旧版本 runtime；回滚和清理由后续版本管理负责。

**代价**：

- 第一版仍未做依赖裁剪，runtime package 初期可能接近当前 1.3GB；本决策先把边界和校验做对。
- `embedded` 安装包仍包含压缩 runtime，只是 app 壳本身回到十 MB 量级。
- `plain` 首次启动需要网络；离线用户应使用 `embedded`。
- 当前 launcher 仍是 shell `exec`，ready URL 继续从 host 输出中识别；把 ready 行收进 launcher
  是后续独立优化，不影响本决策的 runtime 边界。

**当前打包基线（2026-11-19）**：

```text
dsh          = 0.2.2-rc.5
node         = 24.20.0
runtime id   = dsh-0.2.2-rc.5+node-24.20.0+darwin-arm64
sha256       = a6ef9f43e48d9317f799a95b244530be05f7fd58034f64de6047156ef98da218
archive size = 345,334,176 bytes
```

rc.3 的 manifest 与 archive 保留在 `runtime-build/rollback/rc3/`，用于回滚。升级前先在隔离
`COWORK_HOME` / `DSH_HOME` 完成冷启动与 smoke：`probe`、`model`、`workspaces`、`follow`，
并确认 `tool/call` 与 `tool/result` 均落盘。此轮未做依赖裁剪，体积仍约 329MB 压缩、
1.5GB 展开。

## #53 host 生命周期：supervisor 单一发布状态

**日期**：2026-11

**决策**：host 状态从 `connected` / `sleeping` 两个布尔改成互斥枚举
`HostLifecycle { Starting, Ready, Sleeping, Failed }`，由 supervisor 在状态迁移处发布，
UI 只做投影。两个布尔会允许非法组合，也会把「还没收到事件」误渲染成「未连接」。

**状态迁移**：

```text
启动/唤醒 → Starting
握手成功   → Ready
空闲收起   → Sleeping
启动/握手失败 → Failed
```

`UiEvent::Failed` 继续承载具体错误文案，但不再代表 host 生命周期；普通命令失败不改变状态。
侧栏文案一一映射：`host 启动中` / `dsh host` / `host 休眠中` / `host 启动失败`。

## #54 runtime code profile：保守裁剪，不删功能

**日期**：2026-11-19

**决策**：`package-runtime.py` 增加 `--profile full|code`。`full` 是默认且字节不变；
`code` 只套用官方 Desktop 对已安装生产 npm 树的保守排除规则。裁剪包的 id 后缀 `+code`，
manifest 与 `runtime.json` 记录 `profile` 和逐类排除计数，避免与 full runtime 混用同一目录。

**排除范围**：

- package manager metadata；source map；TypeScript 声明与 build cache。
- 非目标 LibreOfficeKit engine；`fs-ext` 编译器输出与构建配置。
- Domino test / `.yarn`；node-pty Windows ConPTY 源码、非目标 prebuild、`.pdb`。
- Koffi 的 Windows import library。

官方规则保留目标 native 二进制、许可证和所有无法识别的资源。因此这一轮**不删除**
Office / WASM / 语音等大包：它们是 Harness 功能，不属于 build/diagnostic/off-platform 文件。
Node 自带 npm 的声明文件也不在 dsh npm 树的规则范围内。

**实测**：

| profile | archive | 压缩 | 展开 |
| --- | --- | --- | --- |
| `full` | `a6ef9f43…da218` | 345,334,176 B（329M） | 约 1.5G |
| `code` | `ca1329f0…ce9` | 254,025,459 B（257M） | 约 1.1G |

`code` 包排除 25,494 个声明、9,947 个 source map、非目标 LibreOffice/WASM 和
node-pty 文件，压缩体积减少约 91MB。full 包重生成后 hash 与已安装的 `/Applications` 一致，
说明新增 profile 没有改变既有产物。

**验证**：`tests/test_package_runtime.py` 覆盖每类排除、未知资源保留、目标 engine 选择与
id 校验；`scripts/runtime/smoke-runtime.sh` 跑 `probe` / `model` / `workspaces` / `follow`。
code 包还做了隔离冷启动（`Starting → Ready`）、真实工具调用（`tool/call` → `tool/result`）
和一次正常文本 prompt。正式 app 未替换为 code 包；后续如需切换，使用
`COWORK_RUNTIME_PACKAGE_DIR=runtime-build/code ./scripts/build-product.sh`。
