# DESIGN — dsh-client-rs

## 使用定位：code 为主，通用 work 轻度

这决定了表面优先级，也决定了哪些 host 插件值得留。

| 场景 | 权重 | 含义 |
| --- | --- | --- |
| 编程任务（读码、改码、跑命令、查 diff） | **主** | 全部交互按高频路径优化 |
| 通用工作（问答、资料整理、脚本） | 轻度 | 能完成即可，不为它加专用界面 |

### 表面优先级

| 优先级 | 表面 | 依据 |
| --- | --- | --- |
| P0 | 工具行：`bash` `read` `write` `edit` `grep` `glob` | code 会话的主体是工具调用，不是纯文本 |
| P0 | `edit` / `write` 的 **diff 渲染** | 改码任务里最重要的可读信号 |
| P0 | 流式 assistant 消息 + 等宽代码块 | 主反馈通道 |
| P0 | timeline 虚拟化 | code 会话工具行密集，一屏几十行是常态 |
| P0 | ctx 圆环 + compaction 区间替换 | code 会话长，压缩是常态而非例外 |
| P1 | 审批 UX（`bash` 命令展示与批准） | 命令是真实副作用，必须看清再批 |
| P1 | 工作区文件树 + 变更文件 | 「改了哪些文件」是收尾时的高频问题 |
| P2 | 会话列表、搜索、历史 | 有即可，不追求复杂 |
| 不做 | Office / 文档预览 / 内嵌浏览器 | code 为主时用不上 |
| 不做 | 定时任务、MCP 管理页、消息反馈 | 同上 |

规则：**P0 没做好之前不加 P1，P1 没做好之前不加 P2。** 每个新表面都要先回答
「code 场景下什么时候会用到它」。

### 对 host 插件选择的影响

| 行 | 判定 |
| --- | --- |
| `office-kit` `skill-office` | 不需要，进 disable 候选（仍需实测收益） |
| `message-feedback` `session-log-download` | 不需要，进候选 |
| `schedule` | 不需要，进候选 |
| `directory-picker` | **需要**（打开文件夹） |
| `open-in-app` | 轻度需要（用外部编辑器打开），保留 |
| `mcp-manager` | 保留（通用 work 会用到 MCP） |
| `workspace-files` `workspace-changes` | **需要**，升为保留 |

注意：工具集来自 **agent preset**（`agent-preset-registry`，web-app 默认
`standard`），不来自 profile。所以「code 为主」不改变 profile 的 bundles，
只改变我们优先实现哪些工具行的渲染。

## 架构总览

```text
┌─ dsh-client-rs（窗口进程，唯一常驻）────────────────┐
│  GPUI 渲染层（wgpu / Metal / Vulkan，自绘）         │
│  ├─ 项目分组侧栏（会话树）                          │
│  ├─ 流式 timeline（消息 / 工具调用 / UiBlock）      │
│  ├─ 交互层（审批弹窗 / user question / 输入框）     │
│  └─ composer 控制条（model / effort / ctx 读数）     │
│  协议客户端 crate                                   │
│  └─ dsh-protocol ←── 唯一了解 wire 细节的 crate     │
└──────────┬────────────────────────────────────────┘
           │ HTTP / WebSocket（未来可加 JSON-RPC 传输）
┌──────────▼────────────────────────────────────────┐
│  dsh host（Node.js，自己 spawn，按需休眠 #44）     │
│  agent 循环 · 模型路由 · MCP · cordis 插件（HMR）   │
└───────────────────────────────────────────────────┘
```

## runtime package 边界

dsh runtime 是独立 sidecar，不再算 app 壳的体积（#52）。构建期由
`scripts/runtime/package-runtime.py` 产出不可变 archive：固定 Node、dsh 生产依赖树、
`runtime.json` 与唯一入口 `bin/cowork-host`。app 只带 `runtime-manifest.json`，
启动前校验 archive hash，安装到 `~/.cowork/runtimes/<id>/`，再启动 launcher。

```text
cowork.app      → runtime-manifest.json + 可选内嵌 archive
runtime package → runtime.json + bin/cowork-host + node/ + dsh/
用户目录        → ~/.cowork/runtimes/<id>/
用户状态        → ~/.dsh/（与 runtime 分开）
```

`legacy` 的 `Resources/runtime/node_modules` 保留为兼容路径，但不是新发行形态。

runtime 生命周期由 `apps/desktop/src/runtime.rs` 统一负责：选择、安装、last_good、
last_verified 与回收都在 `~/.cowork/state.json` / `~/.cowork/runtimes/` / `~/.cowork/runtime-quarantine/`；
`host.rs` 只消费 `ActiveRuntime` 的 argv 并管理进程（#55）。

## crate 划分

| crate | 职责 | 说明 |
| --- | --- | --- |
| `dsh-protocol` | wire types、序列化、端点常量 | 全项目唯一知道协议细节的地方；上游 API 变更只改这里 |
| `dsh-host` | host 生命周期：spawn / 看门狗 / 重连 / 版本探测 | 永远 own host（#41）；是否常驻由 `apps/desktop` 的 supervisor 决定（有活才在，#44）；立项期的复用模式见 DECISIONS #19/#40 |
| `dsh-client-ui` | GPUI 视图层：窗口、面板、timeline、渲染器 | 不 import 协议细节，只消费 `dsh-protocol` 的类型 |
| `apps/desktop` | 主程序入口 | 薄壳，组装以上 crate |

依赖方向：`apps/desktop → dsh-client-ui → dsh-protocol ← dsh-host`。
UI 与 host 之间不直连，UI 只看协议类型（事件流 / 请求）。

## 进程与内存模型

**窗口进程内单 runtime 是硬约束。** 面板、渲染器、协议客户端全部同进程；除 dsh host
（独立进程，见 DECISIONS #5）外，不为任何功能再开子进程或嵌运行时。

### 内存基线（已实测）

空窗口、零业务代码，`probe` profile（见根 `Cargo.toml`），macOS arm64，
`ps -o rss=` 取启动 8s 与 12s 两点的稳态值，每种配置三次：

| 配置 | 稳态 RSS（裸二进制） | 稳态 RSS（`.app` 启动） | 二进制 |
| --- | --- | --- | --- |
| 仅 `gpui-base`（空窗口） | 61.8MB（61.6–62.1） | **85–91MB** | 6.9MB |
| + `assets`（Lucide 图标，空窗口） | 60MB | — | 7.2MB |
| + `gpui-component` + `assets` | ~73MB（72.9–86.5，中位 73.2） | — | 14MB |
| **完整界面 + 侧栏会话树 + 一次真实对话** | — | **90MB**（90/90/90） | 12MB |
| **完整界面 + host 休眠**（#44；真实 `$DSH_HOME`，空闲 20s 后收 host） | — | **96–99MB** | 10.8MB |
| **完整界面 + host 在跑**（#45：`--max-semi-space-size=4`） | — | 客户端同上 + host **343MB** | 同上 |

**两种启动方式差 23MB，必须先说明是哪一种。** 裸二进制（`cargo run`）在 macOS 上
永远不是前台应用（`window.is_window_active()` 恒为 `false`，见 DECISIONS #23），
没有活跃窗口的合成/背压分配，所以读数偏低；`.app` + `open -a` 是真实用户工况。
历史上那份 61.8MB / 67MB 是裸二进制的数，属于低估，现按工况分开记。

**界面本身的增量是稳定的 +5MB**：空窗口 85MB → 完整界面 90MB。`gpui-base` 的控件
是无样式基元，不拖主题与资源包。复现：`cargo run --example empty_window
-p dsh-client-ui`（`--no-default-features` 关图标包）。

控件库 `component` 才是那 +12MB / +7MB 磁盘的来源；`assets` 单独启用基本零增量
（60MB、+0.3MB 二进制），所以图标直接走 Lucide，见 DECISIONS #20 的订正与 #22。

⚠️ 单次采样不可靠：同一个二进制首次读到 75MB，重复三次稳定在 61.8MB
（二进制刚生成时页面还在换入）。任何 RSS 结论至少三次取中位数。

| 项 | 预算 | 依据 |
| --- | --- | --- |
| 窗口进程空闲（框架 + 空窗口） | 85MB 实测（`.app` 启动） | 上表；约为 Electron 190–430MB 的 1/5–1/5 |
| 窗口进程带完整界面 | 90MB 实测 | 上表末行；界面本身 +5MB |
| 每个打开的会话 | 增量 ≈ 数据结构 + 时间线可见区虚拟化缓冲 | 只渲染可见行，历史消息懒取（80 条/启动，参考 rdsh-desktop） |
| dsh host（Node + base + web-app） | **256–290MB 稳态**，启动峰值 396MB，footprint 307MB | 下述实测。`node -e ''` 空基线仅 41MB，所以约 **215–250MB 是 harness 自身** |
| 禁止项 | 窗口进程内 V8 / JSC / QuickJS 常驻 | 见 DECISIONS #1 |

**host 实测细节**（`@deepseek-ai/dsh@0.2.2-rc.3`，隔离 `DSH_HOME`，
`--profile dsh-client-rs --from-default-profile web --no-open --port 0`）：

| 指标 | 值 |
| --- | --- |
| `ps` RSS（boot 后 25s 起，刚 boot 完） | 405MB → 290MB → 256MB（逐步 GC 下降） |
| `ps` RSS（单会话、单轮对话后） | **165MB**（165/165/165） |
| `vmmap` physical footprint | 307MB（peak 376MB） |
| 线程 / 子进程 | 13 / 0 |
| 最大常驻区域 | `__TEXT` 169MB + `Memory Tag 255` 229MB（944 个区段） |

⚠️ **host RSS 随「碰过什么」大幅波动**：只跑一轮短对话的实例稳定在 165MB，
而启动后带 Web UI 预热的实例是 256–290MB。引用 host 数字时必须写明是哪种工况，
否则差出 90MB。

**必须按「总量」对外比较，不能只报客户端的 90MB。** 上游 Electron 桌面并不是只有
Electron：它同样 spawn 一个 RunAsNode 的 dsh host。同口径：

| 方案 | 客户端 | host | 合计 |
| --- | --- | --- | --- |
| 上游 Electron 桌面 | 190–430MB | ~165–300MB（同一个 host） | **490–730MB** |
| 本方案 | **90MB** | 165–290MB | **255–380MB** |

结论要写成两句话：**总量约降一半，客户端降 75–85%。** 「极致低内存」只对客户端
成立，host 不归我们控制（DECISIONS #5）。对外若只报 90MB 就是误导。

⚠️ 连带推论：**host 才是内存大头，优化它的收益远大于优化 UI。** 可行手段只有
「减少启用的插件」（最小性第 2 层），目标是把 215–250MB 的 harness 自身体积压下来。

控内存三原则（写代码时对照）：

1. 单进程 UI，不拆多 runtime；
2. 次要面板（设置 / 插件管理）懒初始化，不打开不占内存；
3. 纹理 / 字形图集 / 时间线做虚拟化与回收，长会话不线性涨。

## UiBlock：插件驱动的声明式界面

GPUI 窗口不执行 harness 插件代码（那是 Node 侧 cordis 的事）。
插件要向 desktop 呈现自定义内容，走**数据协议**，不是代码注入：

```text
cordis 插件（服务端，可 HMR 动态装卸）
    │ 产出结构化 UiBlock（表 / 图表 / 对比 / 步骤 / 输入框 …）
    ▼ tool/result.data.meta（dsh 官方工具私有展示载荷）
dsh-client-rs 通用渲染器（Rust）
    └─ 按 block 类型画进 timeline
```

**承载位置不新造通道**：UiBlock 挂在 `tool/result.data.meta`。dsh 将它定义为
工具私有展示载荷：对核心不透明、必须 JSON 可序列化、可从持久日志复现同一张
卡片（`dsh-tool-fs` 的 diff 即走此处）。`meta` 缺失或 schema 不识别时
降级为普通工具行，不得报错或丢弃。

设计要点：

- schema 起步对齐上游 `dsh-intelligent-ui` 的元素目录（render_ui 语义）；
- 客户端必须对**未知**工具调用做通用降级渲染（raw 行），工具目录是动态的（HMR 生效中）；
- 由此「动态更新界面自定义内容」在 GPUI 窗口同样成立：插件热装 → 新 UiBlock 出现在 timeline，客户端不改代码、不重启。

## composer 控制条（statusline）

输入区下方独立一行：左端是**当前工作区名**（`INK_MUTED`、超过 280pt 截断、不可点），
右端是 `model` | `effort` | ctx 圆环（32px 环形进度）| cache。
`mode` 下拉不在这一行——它在输入卡片的按钮行左侧（#50），那行右端是 send / stop。

输入框本身是**多行**的：`Enter` 发送（不插换行）、`Shift+Enter` 插换行，
卡片跟着长高（自动涨到 6 行，再高就在卡片内滚动）。实现与理由见 DECISIONS #51。

数据绑定（wire 结构与语义见 PROTOCOL.md「statusline（composer 控制条）数据源」）：

| 位 | 源 | 说明 |
| --- | --- | --- |
| mode | 投影 `permissions` + `permissionPresets/catalog` | 服务端有状态；**键缺失 = 未组合权限服务，隐藏控件**。控件位置在输入卡片里，菜单从芯片向上开 |
| model | `session/modelCatalog` + 投影 `modelSelection` | 目录驱动下拉，当前值来自会话投影 |
| effort | catalog 条目 `reasoning.efforts` | 不透明 ID，精确匹配，禁止自动降级/别名 |
| ctx 圆环 | 投影 `contextPressure` / `tokenUsage` | `session/projections` 快照 + `session/follow` 增量 |

mode 写路径：slash 命令 `/permission <preset>` 经 `session/prompt` 提交，
无专用 RPC；不要自己缓存为权威。

圆环取数规则：

- 百分比 = **`projectedTokens / contextWindow`**，封顶 100%；
  `pressureTokens` 是上一发请求采样，streaming 期间不动、不感知 compaction，
  不用于圆环；
- 无 `contextWindow` 时降级为紧凑文本（如 `ctx 42k`，取 `pressureTokens`）；
- 字段非原子（last-wins，换模型瞬间可能「新容量 + 旧压力」）：UI 接受瞬态
  错位，不闪烁重排。这是用户参考值，不是断言。

## 侧栏：workspaces 树

侧栏是导航，不是「当前会话」的占位。结构对齐官方 `dsh-client-ui-workspace`：

| 层级 | 行内结构 | 交互 |
| --- | --- | --- |
| 头 | `Workspaces` + 加号 | 弹系统目录选择框，把目录**注册成工作区**（不是新建会话） |
| 工作区 | `[折叠箭头][名称][hover 加号][hover 垃圾桶]` | 点行展开/收起；加号在该工作区**新建会话**；垃圾桶进入二次确认 |
| 会话 | `[状态点][标题][hover 归档][相对时间]` | 点行切换，选中行 `BG_ACTIVE`；归档把会话收进底部的「已归档」 |
| 已归档 | `[折叠箭头]已归档 N` | 默认收起；展开后每条带 hover「取消归档」 |

两个加号分工是刻意的：头部的加号增加的是**目录**（`workspace/create`，路径由
系统目录框给出），行内 hover 的加号才是在某个已有目录下开会话。把头部加号
做成「在当前工作区开会话」，用户就没有任何入口把一个新目录加进侧栏。

**垃圾桶走行内二次确认**：点一下把行尾换成「取消 / 移除」，点「移除」才生效。
现在「移除」是客户端本地隐藏，不调用 `workspace/delete`：整组连同会话从侧栏消失，
底部出现可点开的「已隐藏的工作区」；重新添加同一目录后老会话归位（DECISIONS #58）。

**会话只能归档，没有删除**（DECISIONS #36）：host 的 `workspace/*` 里没有删会话的
接口，只有 `archiveSession` / `unarchiveSession`。归档把会话从树里拿走、记录与目录
都留着，随时点「取消归档」放回来。空闲会话一点就走；**正在跑的会先就地确认**
——归档会顺手停掉它那一轮（`stopActivity: true`），得先问一句。
侧栏正文可滚动，否则会话一多最底下的「已归档」永远在画布外。

几何：侧栏 264、内边距 16、行圆角 6、会话行左缩进 24；工作区名 13px `INK`，
会话标题 12px（选中 `INK`，未选中 `INK_SOFT`），时间 11px `INK_MUTED`。

**侧栏开关成对**：顶部右侧是 `PanelLeftClose`（收起），收起后标题栏最左出现
`PanelLeftOpen`（展开）。收起时主区顶到窗口左边，标题栏左边距从 28 变 96，
自己让开系统红绿灯。

**主区三段落在同一条左沿**：标题栏、时间线、composer 都靠同一个 `CONTENT_W`（860）
列定位——标题行是「外层 `justify_center` + 行 `w_full + max_w(CONTENT_W)`」，时间线与
输入框是「`items_center` + 同一个 `max_w`」。早先标题栏直接 `pl(GUTTER)`，宽窗口下
内容列居中、标题贴边，顶部看上去就是错的。实测三段的左沿 772/773px（2x）、右沿
2489/2491px。收起侧栏时标题栏仍用 96 让红绿灯、内容列维持 28，**这一段不一致是有意的**
（红绿灯只占标题栏那一条高度，内容没必要跟着缩进）。

数据来自两路：**分组**看 `workspace/follow`（注册表顺序，没有会话的空工作区也显示），
**会话**看 `session/list`。归属真源是账本 `session_ids`（cwd 只在收养老会话时做一次性
启发）；侧栏只显示工作区，**没有「未分组」组**——无归属的历史会话不在侧栏出现，
它们仍在 host 上、可通过搜索访问。「移除」是客户端本地隐藏，隐藏组的会话一起
不显示，底部有可恢复的「已隐藏的工作区」（#58）。会话组内按 `updatedAt` 倒序；
标题取投影 `title`；**空白会话只显示当前那一条**（官方 `pinCurrentBlank` 语义），
否则每次启动都会多出一片「新会话」。

图标统一用 Lucide（`gpui_kit::assets::IconName`），14px（搜索/加号/面板/设置）
与 12px（折叠箭头），颜色跟随容器 `text_color`。理由见 DECISIONS #22。

**侧栏顶部品牌**：不是文字 `dsh`，是官方锁版——鲸鱼 mark + `deepseek HARNESS`
字标，15px 高（宽 113.75，按 182:24 锁比），颜色 `INK`。矢量取自官方前端产物
（19 条路径，`currentColor`），不手绘（#32）。

**加目录的两步**：注册成功后顺手在该目录开一个空白会话（刚注册的空工作区
虽然也会显示，但一条会话都没有的组点开是空的，顺手建一条更顺手）。

## 右栏（会话上下文）

标题栏右端的 `PanelRight` 图标控制**主区内的右栏**（宽 300，`BG_SIDEBAR` 底、
左侧 1px `BG_CARD_EDGE`），不是左侧栏。左侧栏常驻。

右栏只放有来源的字段：工作区路径、会话标题、`model`、`ctx`；缺来源的读数一律
显示 `—`（与 #21 同一原则）。

## composer 的补全（`@` 文件 / `/` 命令）

输入框里打 `@` 或 `/` 就在卡片正上方弹出候选；浮层**与输入框卡片等宽**（同一个
`w_full + max_w(CONTENT_W)`，窄窗口一起缩），最多 8 行、行高 26，`BG_CARD` 底 +
`BG_CARD_EDGE` 描边 + `shadow_lg`，高亮行 `BG_HOVER`。

| 触发 | 候选 | 右侧灰字 | 接受后插入 |
| --- | --- | --- | --- |
| `/` | `commands/list` 的命令 | 命令摘要 | 命令名，尾巴留一个空格 |
| `@` | 按查询分两路（见下） | `目录` 或留空 | `@path`；含空白加引号 |

**`@` 一个字符就能用**，不必再打内容：查询分三档，与官方
`dsh-file-reference-local` 的 `WorkspaceFileSearch::list` 一致：

| 查询 | 走哪个接口 | 结果 |
| --- | --- | --- |
| 空（仅 `@`） | `workspaceFiles/list {path:"."}` | 工作区根目录的直接子项，**目录在前** |
| 含 `/`（`@crates/`、`@crates/dsh-`） | `workspaceFiles/list` 列该目录，`/` 后的片段本地过滤 | 该目录的直接子项，不再全库跑 |
| 不含 `/`（`@crat`） | `workspaceFiles/search` | 全工作区命中，重新排过序 |

为什么不能一个接口打天下：`workspaceFiles/search` 的 `query` **不接受空串**
（`gateway/bad-request: query must not be blank`），而 `workspaceFiles/list` 的
`path` 不接受空串、根目录要写 `.`（DECISIONS #38）。

**排序照官方的打分**（`rankCandidates`）：名字全等 1000 > 名字前缀 900 >
名字包含 700 > 路径包含 500 > 子序列 300+，目录额外 +25；同分时目录在前，
（非空查询）路径短的在前，最后字典序。ripgrep 自己给的是「按深度再按路径」
序，不重排的话 `@cr` 第一条会是 `target/debug/incremental`。

**哪些不给**：隐藏项（`.` 开头的路径段，除非用户自己就在打 `.`）与构建/依赖
目录（`target`、`node_modules`、`dist`、`build`、`out`、`coverage`、`.git`、
`.next`、`.nuxt`、`.turbo`、`.venv`、`__pycache__`、`.pytest_cache`、
`.mypy_cache`、`.gradle`）——这两条都是官方同一张清单。

与官方的唯一差别：全库那一段我们用 host 的 ripgrep（子串，遵守 `.gitignore`），
官方是本地自建索引做子序列——所以 `@cr` 不会给出 `Cargo.toml`（子序列匹配）
这种结果。换取的是不把工作区树读一遍（内存与首字延迟）。

路径太长时**头部截断**（`…/patches/0001-….patch`）——文件路径的尾巴比开头有用。

键位（照官方 `input-trigger` 的 `pass` / `pick-highlighted` 两种手势）：

| 局面 | `Tab` | `Shift+Tab` | `Enter` | `Esc` |
| --- | --- | --- | --- | --- |
| 有候选、**没有键盘高亮**（刚弹出） | 选中第 1 条 | 选中最后 1 条 | **发送**（放行给输入框） | 收起 |
| 有候选、已高亮 | 下移（到底回第 1 条） | 上移 | 接受高亮那行 | 收起 |
| 没候选 | — | — | 发送 | — |

**面板默认不高亮任何一行**，这是刻意的：补全是个提示，不是模式。用户敲 `/new`
顺手回车，期待的是把 `/new` 发出去；如果弹面板就自动高亮第一条，回车会被吞成
「插入 `/new-session` 加一个空格」——上一版就是这么错的（DECISIONS #37）。要补全的人
自然会先 `Tab`/`↓`，那时回车才变成「接受」。方向键与 `Tab` 同义。

鼠标：行可点（左键即接受该行）且 hover 会亮，但**不抢键盘选中**——键盘选中和
hover 高亮可以同时出现在两行上，点下去按你点的那行算。

插入规则照官方：目录补尾斜杠（`@docs/`），带引号的目录**不闭合**（`@"my dir/`），
方便接着往下补；路径含控制字符或引号就拒绝插入。

## 一轮对话的状态（working）

发出去到答完，界面要给同一个事实三处回声。状态只有一个来源（`Chat::phase`）：

| 阶段 | 含义 | 进入时机 |
| --- | --- | --- |
| `Idle` | 没在跑 | 启动；收到 `turn/end`；切会话 |
| `Sending` | 已交给 host，第一帧还没回 | `send()` 成功入命令通道 |
| `Running` | 流已开始 | 收到第一条 `assistant-stream` 帧 |

三处渲染：

1. **时间线尾巴**——用户气泡下面一行：`Sending` 是灰点 +「已送到 host…」，
   `Running` 是绿点（`OK`）+「正在思考 · Ns」。秒数从 `turn_started` 算，
   1 秒一次的 ticker 只在忙时 `cx.notify()`；空闲时不再碰渲染。
2. **composer 右下主按钮**——忙时是方块（`Square`＝停），空草稿时不出现发送箭头；
   非忙时是箭头（`ArrowUp`＝发送）；**忙且有草稿时两个都在**（先发出去＝排队）。
3. **侧栏会话行状态点**——`session.running` 为真，或该行就是当前会话而
   `phase` 在忙，就点亮。轮次结束后发 `UiCommand::RefreshSessions` 重读一次
   `session/list`，否则那个点会一直亮着（`session/list` 的 `running` 不会自己推到界面上）。

**停**：方块走 `session/cancel`（参数是当前会话），语义是「只取消这一轮、
inbox 里的排队消息不动」；先往时间线补一条灰字「已请求停止本轮」，真正回空闲
还靠 `turn/end` 那一帧。为什么不直接置空闲：取消是请求，host 可能还在收尾。

**快照晚到不覆盖进行中的一轮**：切会话/启动时会拉 `session/follow` 的 snapshot，
它可能比「刚发出的那条」还早到（典型现象：气泡和状态行一起被抹掉）。忙时
直接丢掉 `UiEvent::Transcript`。

与官方的对应（`dsh-client-ui-*` 前端产物）：`RunningStatus.tsx` 的 `deepDiving`
计时 → 时间线尾巴；`rows/Rows.tsx` 的 `sessionStatuses` 优先级 → 侧栏点；
`submission-policy.ts` 的 `queue`/`steer` → 我们固定 `queue`；`stop-shortcut.ts`
的 `session/cancel` → 停止按钮。**没移植**：pending interaction 的 warning 态
（approval / plan-review / question）、subagent 计数、`steer` 策略。

## composer 控制条的 mode 下拉

左端 `mode` 显示当前会话的权限 preset（原值，如 `workspace-write`），点击向上
拉起浮层：

| 要素 | 来源 |
| --- | --- |
| 候选项 | `permissionPresets/catalog`（启动时一次，3 个） |
| 当前值 | 会话投影 `permissions.currentValue` |
| 写入 | `commands/execute` 的 `/permission <preset>`（见 DECISIONS #26） |
| 回读 | 提交后刷新 `session/list`，用投影覆盖芯片 |

浮层几何：宽 232、向上展开、距芯片 26、圆角 8、`BG_CARD` 底 + `BG_CARD_EDGE`
边 + `shadow_lg` 分层；当前项 `INK` + 对勾，其余 `INK_SOFT`，行高 26、hover
`BG_HOVER`。展开时芯片的箭头换成 `ChevronUp`。

点浮层和芯片以外的任何位置收起（`on_mouse_down_out` 挂在「芯片 + 浮层」的包裹层上，
所以点芯片走它自己的开合、不会和外部关闭打架）。

## 启动与主区空状态

启动**不建会话**：主区先显示空状态，由用户挑一个工作区（官方 desktop 的
「Session Intent 主视觉区选择器」等价物）。空状态从上到下只有三块：

1. 品牌锁版（`brand::lockup`，20pt）；
2. 一行提示：有注册工作区时「选一个工作区开始新会话」，一个都没有时
   「添加一个目录开始」；
3. 一张卡片：每个已注册工作区一行（左名字、右 `~` 缩写路径，点 = `session/create{cwd}`），
   最后一行恒为「添加工作区…」（系统选目录框 → `workspace/create`，与侧栏 `+` 同一路径）。

- 会话的 `cwd` 只来自用户选择（或侧栏工作区行的 `+`、或调试开关），不再由
  进程启动目录 / `$HOME` 推出来（#43）。
- 会话一旦打开，主区换回聊天区：标题栏 `工作区 / 会话短 id`、时间线、composer、
  状态行；右栏开关也只在有会话时出现。
- 握手成功但还没有会话时，侧栏脚标仍显示 `dsh host` 绿点（`UiEvent::Ready`），
  否则空状态会长期停在「未连接」。

## 状态行的模型与强度

composer 下方右侧三个格子，前两个是上拉浮层：

| 格子 | 显示 | 来源 |
| --- | --- | --- |
| `model` | 目录里的模型显示名 | 投影 `modelSelection.next` → `lastUsed` → 目录 `default` |
| `effort` | 强度 id（`low` / `high` / `max` / `off`） | `next.reasoningEffort` → 该模型 `defaultEffort` |
| `ctx` | 圆环 + `1%`；缺分子或分母时退回 `ctx 5.7k` | `contextPressure` 投影（`session/projections`） |
| `cache` | `cache 60%` 或 `cache —` | `tokenUsage` 投影：`cacheRead / (input + read + write)` |

模型浮层按提供方分组：只有一个提供方时不出分组标题，多个时每行右侧补提供方名。
点选即 `session/selectModel`，换模型时强度取该模型的 `defaultEffort`。

空目录与故障的口径对齐官方 `dsh-client-ui-model-selection`（#42）：

| 情况 | 显示 |
| --- | --- |
| 当前选择不在目录里 | 触发器写「请选择模型」（正常字重），不写 `—` |
| 目录空 | 菜单照常能开，里面是一行不可点的「暂无可用模型」 |
| 提供方列举失败 | 每行一条不可点的「{name} 加载失败：{message}」 |
| 当前模型没有强度 | 整格 `effort` 隐藏 |

列出失败行很重要：有凭据但拉不到目录时，只显示空目录会让人以为「没配」。
发送**不**受目录影响 —— host 会在请求执行时报缺凭据/模型不可用，客户端不替它拦。

三个下拉（mode / model / effort）共用同一套实现：`chip()` 画触发器、
`pull_menu()` 画浮层并处理「点外面收起」，每行只带 `key`，点选后由各控件自己的
普通函数指针解释这个 key。宽度：mode 232、model 264、effort 152。

## 窗口尺寸

设计稿画布 1440×920 是上限，不是硬指标：开窗时夹到「主屏可用区域 − 96」并居中，
下限 960×640（同时作为 `window_min_size`，再拖小输入框就会被挤掉）。取不到屏幕
信息时才退回设计尺寸。拖动后的尺寸**不持久化**（见 DECISIONS #28）。

## 应用图标

`apps/desktop/assets/AppIcon.icns`，构建时由 `scripts/build-product.sh` 拷进
`Contents/Resources/`，`Info.plist` 的 `CFBundleIconFile` 指过去（见 DECISIONS #46）。

- **图形逐字取自官方**：`@deepseek-ai/dsh-web-frontend/dist/favicon-dark.svg`
  （官方为深色背景准备的白色 mark），入库为
  `crates/dsh-client-ui/assets/deepseek-mark-white.svg`，与品牌锁用同一份矢量。
- **底是我们自己的**：`#1C1C21 → #0A0A0C` 渐变 squircle，跟 `#0F0F10` 界面同色系。
  官方那张浅底 `icon-512.png` 没采用：深色 UI 的 app 在 Dock 里发一块白板并不一致。
- **网格**：1024 画布、824×824 方块、圆角 185、图形高占方块 0.47（macOS Big Sur 网格）。
- **可复现**：`./scripts/make-icon.sh`（需 `rsvg-convert`）重新光栅化并用
  `iconutil` 打包；`AppIcon-1024.png` 只是预览，不进包。

## timeline 与数据模型

时间线渲染的是 **surface**（表层），不是原始事件日志。三条硬约束，
完整类型与算法见 [MODEL.md](MODEL.md)：

1. **表层支持区间替换**，不是 append-only。compaction 用
   `surfaceOp: replace{startSeq, endSeq}` 遮蔽区间；`replace_generation`
   递增用于区分尾部增长与重写。
2. **仅 5 类是 surface 事件**：`system/message` `developer/message`
   `user/message` `assistant/message` `tool/result`。日志专用事件观察后丢弃，
   只留 `tool/call` → 时间的小索引。
3. **三条轴都是开放集合**（事件 63 / 投影键 22 / 内容块 7），按「已知 +
   未知降级」处理，禁止穷举；未知事件无 `ignorable` 标记时必须拒绝 fold。

流式与持久分离：实时增量走 `assistant-stream` 帧（start/chunk/end），
`end{committed, seq}` 到达后用该 seq 的持久 `assistant/message` 替换累积结果。

### 工具行与过程组

时间线的一行只有四类：用户、助手、工具、提示。**工具行是两条事件的投影**——
`tool/call`（在干什么）与 `tool/result`（干成了什么）按 `callId` 配成一条，
不是各占一行。这条投影写在一个地方：`crates/dsh-protocol/src/transcript.rs` 的
`Transcript`，历史回放与实时流都必须走它，否则两端必然长得不一样。

```text
tool/call   {turn, step, callId, name, arguments}      ┐
                                                       ├─→ ToolCall{name, summary,
 tool/result {turn, step, message, error?, meta?}      ┘   status, detail_chars}
```

- `callId` 的取法有三种（实测 v4 会话在 `data.message.toolCallId`，老数据有
  `data.callId` / `data.message.source.callId`），按序回退。
- 失败靠 `message.isError`（老数据 `error` 字段）；**不用颜色**表达失败，
  后缀写「失败」。
- 摘要从参数里挑一个键（`command` 优先于 `description`，其余按
  `file_path`→`path`→`pattern`→`query`→…），挑不到退化成 `k=v` 列表。
  宁可朴素也不猜。
- 正文只留前 32K 字符（`DETAIL_LIMIT`），界面再限 24 行：几 MB 的输出不能
  一次塞进 frame。

**过程组是渲染期派生的**，不进模型：同一轮里连续的工具行归一组，中间只要夹
一句助手正文就断开。切模式只换画法，数据不重复、不重新请求。

| 模式 | 组头 | 组体默认 | 单个工具正文 |
| --- | --- | --- | --- |
| `compact` / 简洁 | 只给类别摘要 | 收起 | 手动展开 |
| `standard` / 标准（缺省） | 类别摘要 + 运行中工具详情 | 收起 | 手动展开 |
| `detailed` / 详细 | 运行中隐藏组头，历史保留摘要 | 运行中展开，历史收起 | 手动展开 |

**任何档位都不自动灌输出**——这是官方 `transcriptView` 的同一口径：档位只决定
过程组的组织方式，工具正文永远点开才看。唯一的切换入口是 settings 页；档位
真值在 dsh host profile 的 `ui-chat.transcriptView`，启动读 `settings/describe`，
点击经 `settings/update` 写回，客户端只保留内存镜像。

### 助手正文的 markdown

**解析与渲染都不自己写**，也不引 Zed（GPL-3.0 + 拖整个编辑器）：用 `gpui-base`
（gpui-kit 家族，Apache-2.0，已在依赖树）自带的 `TextView::markdown`，解析器是
`markdown 1.0.0`（wooorm/markdown-rs，MIT）。唯一入口是 `crates/dsh-client-ui/src/markdown.rs`。

下面两张图的前提是同一套断言：

1. **身份来自数据，不来自渲染顺序**。`ElementId` 用
   `MessageId`（`(turn, step)` 优先，否则事件 `seq`）。流式帧与持久事件两边都有
   `(turn, step)`，所以「流式 → 已提交」不换 key、不重建排版（`fold.turn_step()`）。
2. **只有助手正文解析 markdown**。工具输出是字节流（`*`、diff 的 `-`），
   解析它会改变语义；用户消息也不解析，写什么显示什么。

样式收敛在 `theme::markdown_style()` 一处（正文 `INK`、次要 `INK_SOFT`、弱
`INK_MUTED`，行内码/代码块/表格底 `BG_CARD`，边 `BG_CARD_EDGE`，`with_dark(true)`）。
**不接语法高亮**（tree-sitter 常驻内存与 north star 冲突）、**不接图片**。
链接点击用 `TextView` 默认的 `App::open_url`，不需要 handler。

内存代价（同会话、release `.app`、三次读数一致，用 `COWORK_RICH_TEXT` A/B）：
25 条短消息的会话 **+1.2MB**（111.4 vs 110.2）；含代码块与列表的 2 条消息会话
**+6.7MB**（102.9 vs 96.2）。即代价跟随 markdown 结构量（代码块/表格的排版树），
不随消息条数线性涨。整条退路是 `COWORK_RICH_TEXT=0` —— 同一入口的纯文本分支，
不是第二套渲染。

## host 生命周期

> **现行实现（读这里）**：产品形态是 `cowork.app` + profile `cowork`，启动顺序、
> 认证与收尸逻辑见 `docs/DECISIONS.md` 的 #39、#41 与 **#44（host 只在有活时存在）**。
> 下面几节是立项时的方案与依据，保留作为背景：其中 profile 名 `dsh-client-rs`
> 是开发/探针期用的（#15），产品版已改为 `cowork`（#39）；「窗口退出不带走 host」
> 「ready URL 缓存文件」「loopback 发现 + 自签 cookie attach」先后被 #39 的看门狗
> 与 #41 的「永远 own host」取代。

### 现行启动路径（#39 + #41 + #44）

```text
DSH_READY_URL（带 token） → /connect 换 v2 cookie（开发期逃生口）
否则                    → 自己 spawn dsh --profile cowork → 抓 ready URL
```

对齐官方 desktop：无视本机已在跑的 dsh web，运行时层（进程/profile/peer）独占，
状态层（`$DSH_HOME` 的会话/工作区/凭据）共享。自己拉起的 host 由 `/bin/sh`
看门狗看着，进程退出（含 app 被 `kill -9`）1s 内收掉；`DSH_READY_URL` 情形
`Startup.process = None`，退出绝不杀别人的服务端。

**#44 起 host 不再活到 app 退出**：空闲 `COWORK_SLEEP_IDLE_SECS`（缺省 600 秒，
`0` = 不休眠）就主动收掉 host，下次有命令再拉起（同一条 ready URL 流程）。
界面不依赖它的存活——侧栏、时间线、模型芯片都在客户端内存里；左下角标改显
「host 休眠中」。会话不丢：休眠前记住当前会话 id，醒来用 `session/follow` 快照
重放（与侧栏切会话同一条路径）。

### 本地侧栏快照（#44）

主机不在的时候（刚启动、正在休眠、甚至拉不起来）侧栏不能空。启动时先读
`~/Library/Application Support/cowork/state.json`，把工作区注册表、会话摘要、
归档/置顶集合直接喂给界面，再等 host 的真实数据覆盖：

- 只存侧栏要看的字段；**不存时间线**。假历史比空历史坑人。
- `running` 读回来一律置零：上次强退的运行点不能留成永久绿点。
- 写时机：会改侧栏的命令之后（新建/切会话、工作区增删、归档、一轮结束刷列表）
  以及休眠/退出前；输字引发的 `Complete` 不写。
- 原子替换（先写 `.tmp` 再 rename），解析失败就当没快照，不影响使用。

### profile 选择：专用 `dsh-client-rs`

```text
dsh --profile dsh-client-rs --no-open --port 0
```

专用 profile 目录：`$DSH_HOME/profiles/dsh-client-rs`。

#### 为什么专用 profile 现在可行

存储分层决定了 profile 只管配置：

| 路径 | 层级 | 专用 profile 能否共享 |
| --- | --- | --- |
| `$DSH_HOME/sessions/` | home | ✓ |
| `$DSH_HOME/storages/`（含 workspace） | home | ✓ |
| `$DSH_HOME/.credentials.yaml` | home | ✓ |
| `$DSH_HOME/skills/` `AGENTS.md` `.env` | home | ✓ |
| `$DSH_HOME/cordis.patch.yml`（home patch） | home | ✓ |
| `profiles/<name>/cordis.patch.yml` | **profile** | ✗ |
| `profiles/<name>/package.json`（插件依赖） | **profile** | ✗ |

所以专用 profile **不丢历史**（会话、工作区、凭据、技能全在 home 级），
只丢 **profile 级配置**：它自己的 Cordis patch 与插件依赖。

#### 为什么必须依赖 `@deepseek-ai/dsh-web-app`

我们不用它的浏览器界面，但 **HTTP/WS host API 全在这一个 bundle 的 patch 里**：

| 行 | 包 | 为什么不可省 |
| --- | --- | --- |
| `web-startup` | `@deepseek-ai/dsh-web-app/startup` | 提供 `webStartup` 服务；`webserver` / `connection` / `web-runtime` 都 `inject: [webStartup]`。没有它不绑端口，且 `--port` `--host` `--no-open` 根本不被识别 |
| `webserver` | `dsh-host-webserver` | 绑端口、路由、gzip |
| `connection` | `dsh-client-connection` | 双面：node 半侧把 gateway 挂到 `/api`，那是我们全部 RPC 的入口 |
| `authentication-token` | `dsh-host-authentication-token` | token 鉴权 |
| `web-runtime` | `@deepseek-ai/dsh-web-app` | **`printUrl: true`：那行带 token 的 URL 是它打印的**（「Providers contribute entry URLs; this row owns printing and browser handoff」） |

关键：`web-startup` 与 `web-runtime` 是**同一个 package 的两个入口**
（bundle 包同时是运行时插件），所以「只要 patch、不要运行时」不可行。

没有其他 bundle 提供 HTTP/WS——逐个核对过：

| bundle | 传输 | 能否用 |
| --- | --- | --- |
| `web-app` | HTTP + WebSocket | ✓ |
| `sdk-app` | **stdio JSON-RPC**（stdout 按换行分隔帧） | ✗ |
| `sdk-minimal` | 同上，独立 patch 树 | ✗ |
| `headless` | 一次性任务，跑完退出 | ✗ |
| `acp-app` | stdio | ✗ |

另一方面，拿掉 `ui-*` 行几乎不省内存：它们是 `dsh.client` +
`platform: "web"` 的**浏览器 bundle**，宿主半侧只「扫描已启用的 Loader
条目并组合启动图」，`/plugins/<id>/client.js` 按需提供，**node 侧从不执行**。
省下的是几十份 package.json 的解析，不是运行时内存。

所以「最小可用 profile」的硬地板就是
`['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app']`——
而这正好是 `--from-default-profile web` 给你的东西，无需额外剪裁。

#### 初始化：dsh 没有 `init` 命令

dsh **没有** `dsh init` 或 `dsh profile init`。只有两条隐式入口，
且写入的 bundle 集不同：

| 入口 | 写入的 bundles | 可用性 |
| --- | --- | --- |
| `dsh plugin --profile X <pnpm 参数>`（"initialized on first use"） | `DEFAULT_PROFILE_BUNDLES` = `['@deepseek-ai/dsh-base']` | **不可用**：无 `dsh-web-app` → 无 host API |
| `dsh --profile X --from-default-profile web` | `PROFILE_TEMPLATES.web.bundles` = `['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']` | **采用** |

源码依据：`apps/cli/src/plugin.ts` 的
`initProfile(dir, PROFILE_TEMPLATES[profile]?.bundles ?? DEFAULT_PROFILE_BUNDLES)`，
自定义名走 `?? DEFAULT_PROFILE_BUNDLES`（base-only）。

#### 打开即用：不做引导

**没有欢迎屏，没有引导流程，没有设置面板。** 启动即进主界面。

```text
1. 检查 $DSH_HOME/profiles/dsh-client-rs/package.json
2. 缺失 → dsh --profile dsh-client-rs --from-default-profile web --no-open --port 0
   （初始化并直接 boot；prepareProfile 被 boot 与 dump 共用，
   apps/cli/src/profile-boot.ts:194）
3. 握手 → 拉 session/modelCatalog → 进主界面
```

要点：

- **`--from-default-profile` 的条件是客户端的责任，不是 dsh 的。** dsh 侧它是
  **无条件 replace**：只要带上这个 flag，旧目录整体 rename 走、新目录就位后删除，
  即使目录已存在也照样执行（`apps/cli/src/profile-boot.ts:90-98, 137-152`），
  用户 patch 一并丢失。所以闸门必须在第 1 步定死：**只有 `package.json` 不存在时
  才允许把这个 flag 拼进命令行**，其余任何情况都不传。
- `--dump-config` 只在「要初始化但不想启动」时用（安装预热、自测）。

模型可用性由 `$DSH_HOME` 与 profile patch 决定，不由客户端决定：

| 来源 | 激活的模型 |
| --- | --- |
| `$DSH_HOME/.credentials.yaml` 有 **`DEEPSEEK_API_KEY`** 凭据 | 官方模型（provider `deepseek-official`） |
| DeepSeek 账号登录 token | 官方模型（provider `deepseek-account`） |
| 环境变量（`credentials` 行读 inherited environment + `.env` 回退） | 官方模型 |
| 当前 profile patch 有 `llm-pi-ai:` 段 | 该段声明的第三方 provider |
| 都没有 | **空列表** |

凭据**必须挂在上面的 ref 名下**：`llm-deepseek-api-key` 走
`credentials.resolve(credentialRef('DEEPSEEK_API_KEY'))`
（`packages/llm/llm-deepseek/src/credential.ts:14-20`），随手写一条别的名字的记录
不会出模型。这是「打开即用」的精确边界，不是任意命名都能用。

全新机器的真实状态是**空列表**——这不是缺陷，是「不做引导」的必然结果。
空态必须说清缺什么、以及凭据文件的确切路径。

空列表时：**不弹引导、不阻断**。composer 保持可输入，发送按钮禁用，
模型选择器显示「未配置模型」。配好后重拉 catalog 即恢复，不需重启。

⚠️ **有凭据 ≠ 有模型**。`buildModelCatalog` 把解析失败的 provider 放进
`failures` 而不是静默丢弃（`packages/api/session-controller/src/catalog.ts:65-71`）。
api-key 路线默认跟随官方 `/models` listing，网络失败时 `discoverModels` 抛错 →
catalog 返回 failure 条目。UI 若不渲染 `failures`，用户看到的就是「有凭据却什么都没有」，
会当成 bug。所以空态分三种：真没配凭据、凭据在但 listing 失败、配了但全被过滤。

#### 最小性：三层，各层的判定方式不同

| 层 | 状态 | 由谁保证 |
| --- | --- | --- |
| 1. bundle 数 | **锁死 2 个**，精确等于 `['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app']` | preflight 断言（见下） |
| 2. host 平面行数 | 默认不动，**实测后按收益决定** | M0 测量协议（见下） |
| 3. 插件依赖 / 磁盘 | `dependencies` 必须为空 | preflight 断言 |

第 1 层不可再减：`base` 提供 `llm` / `credentials` / `session` / 工具与持久化，
`web-app` 提供那 5 行 API。少任何一个都不成形。

第 3 层不能减：剪依赖会直接断 API。

只剩第 2 层有空间，而它**必须靠实测**，不能靠猜。

#### preflight：最小性断言

每次启动前读 `$DSH_HOME/profiles/dsh-client-rs/package.json`，断言三件事：

```text
1. dsh.profile.bundles 精确等于期望值（不是"包含"）
2. dependencies 为空对象
3. 目录里没有意外多出来的 node_modules 顶层包
```

第 1 条用**精确相等**而不是包含，是为了抓 `dsh plugin add` 悄悄塞进来的
bundle——那种情况 profile 会变大但启动仍然成功，静默劣化。

断言失败分两种情况处理：

| 情况 | 处理 |
| --- | --- |
| bundles 缺 `dsh-web-app` | 硬失败，提示用 `--from-default-profile web` 重建（会覆盖配置） |
| bundles 多于期望 / 有额外依赖 | 警告并展示差异，**不阻断启动**（用户可能故意装插件） |

manifest 形状（`packages/boot/app-boot/src/profile.ts:198`）：

```json
{ "name": "dsh-profile-dsh-client-rs", "private": true,
  "dependencies": {}, "dsh": { "profile": { "bundles": ["..."] } } }
```

`initProfile` **只在 `package.json` 不存在时写**，所以对已存在的坏 profile
重跑 `dsh plugin` 不会修复 bundles。

**不只 `initProfile` 会写 bundles。** `packages/util/official-bundles/src/bundles.ts`
还导出两个被 app-boot 读取的常量，boot 时会据此改写 profile：

| 常量 | 行为 | 对我们的影响 |
| --- | --- | --- |
| `INSTALLATION_OWNED_PROFILE_TUPLES` | 列表**精确匹配**表中某条时，整体替换成该名的 `PROFILE_TEMPLATES` 条目 | 表里只有 `headless`，我们的 `base+web-app` 不在其中 |
| `RETIRED_BUNDLES` | 从 profile 里**删除**已退役的 bundle（如 `dsh-experimental-schedule-bundle`） | 我们的列表不含退役项 |

所以「精确相等」断言的失败原因要多想一层：不一定是 `dsh plugin add` 塞的，
也可能是 dsh 自己按模板重写或删除。诊断时先比对 `PROFILE_TEMPLATES.web` 再下结论。

#### 第 2 层的测量协议（M0 执行，不猜）

候选禁用行是 host 平面里我们用不上的部分：

| 类别 | 候选行 |
| --- | --- |
| 桌面/运维类 | `open-in-app` `directory-picker` `plugin-inventory` `cordis-host-runner` `harness-dev-context` |
| 我们不做 UI 的功能 | `message-feedback` `session-log-download` `mcp-manager` `schedule` |
| Office / 创作 | `office-kit` `skill-office` `workspace-dependencies` |
| 可能要用，先留着 | `workspace-files` `terminal-controller` `job-controller` `session-stats` `session-turn-outline` `client-hmr` |

测量方法（每轮只改一个变量）：

```text
1. 基线：原样启动，记 host 首次 ready 耗时 + 稳态 RSS
2. 加一组 disabled: true 覆盖到 profile patch
3. 同样记 ready 耗时 + 稳态 RSS
4. 收益 < 阈值（建议 5% 或 4MB，取先到者）→ 回退，不留
```

**默认不发 disable 清单**。理由：那是一份要对着上游 `cordis.patch.yml`
持续复核的工件，上游每加一行都得重看；只有实测证明有收益才值得背这个债。

注意 `web-app` 里已有若干行用
`disabled: !!js "ctx.get('profileContext')?.name !== 'desktop'"`
门控（`desktop-product-telemetry`、`product-analytics`、`workspace-dependencies`、
`ui-sidebar-browser`），我们的 profile 名不是 `desktop`，这些本来就是关的，
**不需要我们处理**。

#### 已知限制：第三方 provider 需手写配置

`llm-pi-ai` 在 base 里是**休眠挂载**，原文注释：

> zero routes (and no extra models in the picker) until a `llm-pi-ai:`
> settings section supplies provider profiles … Supplying those profiles is
> exactly what the web Models page does.

上游唯一写这个段的入口是 Web 的 Models 页（Settings 持久化到当前 profile 的
Cordis patch）。本项目不做设置面板、也不打开 host 自带的 Web UI，
因此第三方 provider 需要用户自己写进
`$DSH_HOME/profiles/dsh-client-rs/cordis.patch.yml`。

这是**明确接受的限制**，不在客户端范围内解决。空态里给一条可复制的路径提示
即可，不展开成引导流程。

#### 命名规则

| 名字 | 可用性 | 说明 |
| --- | --- | --- |
| `desktop` | **禁止** | CLI 硬拦截（`toLowerCase() === 'desktop'`），Electron 独占 |
| `desktop-rs` `desktop-*` | **禁止（自定）** | CLI 能过（精确匹配），但与上游 `desktop` 语义混淆；上游若改前缀匹配即断 |
| `app` | 不推荐 | 太泛；与上游 `apps/` 概念混淆；未来撞名风险最高 |
| **`dsh-client-rs`** | **采用** | 与项目同名，自解释、零碰撞 |

另：`--from-default-profile` 拒绝将 shipped profile 名
（`web` / `headless` / `sdk` / `sdk-minimal` / `acp`，不区分大小写）
用作自定义目标名。

#### 降级开关

环境变量可把 profile 名改回 `web`，直接复用用户的 Web 配置
（包括它的 `llm-pi-ai` 段），供已有 `dsh web` 环境、不想多一份配置的用户使用。

### 进程管理：detached + 复用（立项方案，已被 #39 / #41 取代）

> **现行不是这样**：host 与 app 同生共死（#39 看门狗），不缓存 ready URL、
> 不 attach（#41）。下表保留作立项依据。

参照 rustdsh `apps/rdsh-desktop/src/host.rs` 的做法（它解决的是「Node 启动要几秒」）：

| 机制 | 实现 |
| --- | --- |
| spawn | 自带运行时的 dsh，参数 `--profile dsh-client-rs --no-open --port 0`，`stdin` 置空 |
| **宿主与客户端分离** | 独立进程组（Unix `process_group(0)`）。**窗口进程退出不带走 host** |
| ready 检测 | 每 150ms 扫一次追加写日志找 ready URL，90s 超时 |
| ready URL 缓存 | `~/Library/Application Support/dsh-client-rs/host.json`，目录 0700、文件 0600（URL 含 token） |
| 下次启动 | 直接 attach（见下），**一次 HTTP 往返**，毫秒级 |
| PID 复用防护 | 停止前用 `ps -o command= -p <pid>` 确认命令行仍是记录的调用 |
| 停止 | 校验通过后 `SIGTERM`，再清缓存 |
| host 意外挂掉 | 不自动重启（避免崩溃循环），提示用户手动重试 |

**与上游 Desktop 的差异**：立项时我们的 host 是 detached 的，代价是需要自己管
缓存与 PID 校验，收益是把 Node 启动的秒级成本从每次启动里消掉。
**#41 起对齐上游**：host 生命周期绑定应用（退出即停），冷启动秒级成本接受，
换来所有权与隔离（见 DECISIONS #41）。

### 握手与 attach（立项方案；现行只走路径 B，#41）

两条路径，按可用性选择：

#### 路径 A：attach（#40 实现过，#41 删除；签名形状仍是 host 侧事实）

> **客户端不再使用**：#41 对齐官方 desktop，永远 own host，attach 与 loopback
> 发现子系统已删。下面的签名形状是 host 协议事实，保留作笔记。

浏览器会话 cookie 由 **HMAC 签名**，签名秘钥存在 `$DSH_HOME/.credentials.yaml`。
因此本进程可为已运行的 host **自签 cookie**，不需要它的 process token。

已用 dsh 源码核实（`packages/host/authentication-token/src/browser-auth.ts`）：

| 项 | 值 |
| --- | --- |
| 秘钥记录 | `client-connection/browser-session:` → 字段 `secret:`（base64url，32 字节） |
| cookie 名 | `dsh-auth-<base64url(SHA-256(authority))>` |
| cookie 值 | `v1.<base64url(payload)>.<base64url(HMAC-SHA256(secret, body))>` |
| payload | `{version:1, authority, issuedAt, expiresAt}`，有效期 30 天 |
| `authority` | 非 TLS 取 `url.host`（**含端口**）；TLS 取 `url.origin`（含 scheme） |

流程：读缓存 origin → 读秘钥自签 → 发一个探测请求验证命主接受该 cookie
→ 接受则 attach，拒绝则丢弃缓存。

#### 路径 B：spawn + token 交换（首次启动必需）

首次运行时 `.credentials.yaml` 里还没有那个秘钥（由 host 首次运行写入），
所以必须走原路径：

host 就绪后在 stdout 打印 `<label>: <url>`，URL 携带进程 token：

```text
dsh web: http://127.0.0.1:54321/?token=<token>
```

1. 读 stdout，用**宽容匹配**提取 URL：首个匹配
   `https?://(127.0.0.1|localhost|\[::1\]):\d+/\?token=` 的行；
   不依赖 label 文案（label 可随版本变）。
2. `GET <url>`，**不跟随重定向**，只取 `Set-Cookie` 的第一对 `name=value`。
3. 后续所有请求带该 cookie：
   `POST /api/<namespace>/<method>`，WebSocket 用 `ws://<host>:<port>/…`。
4. 401 → host 拒绝了 token；无 `Set-Cookie` → 握手失败。

**安全约束**：token、cookie、以及 `.credentials.yaml` 里的秘钥都不得写入
日志、崩溃报告、遥测或 UI。缓存文件与日志必须 0600/0700。
打印诊断时只输出不含凭据的 host 与端口。

### 与上游 Desktop 的 profile 差异

上游 Electron **不跟随** web，它独占 `$DSH_HOME/profiles/desktop`（初始 bundle
列表抄自共享 Web 模板），且"CLI 与 Desktop 共享 `$DSH_HOME` 下受支持的产品数据，
但绝不共享可执行包、插件激活、锁文件或 `node_modules`"。

我们不照做的原因（完整论证见 DECISIONS #14）：

| 维度 | 上游 Desktop | 本项目 |
| --- | --- | --- |
| profile | 独占 `profiles/desktop` | 专用 `profiles/dsh-client-rs` |
| 初始 bundle | 抄自共享 Web 模板 | 抄自共享 Web 模板（同策略） |
| 运行时来源 | 自带签名运行时，版本冻结 | spawn 用户 PATH 上的 `dsh`，跟随用户版本 |
| 配置 UI | 自带设置面板 + Welcome 引导 | **不做任何配置界面**，打开即用 |
| 分离动因 | 防止用户 CLI 改动内置运行时依赖树 | 隔离配置与插件集，不干扰用户的 web profile |

两边的结论都是专用 profile，但上游用界面解决「新 profile 没模型」，
我们靠「模型来自 `$DSH_HOME` 级凭据」这一事实绕开，代价是第三方 provider
需要手写（见「已知限制」）。

### 为什么不能用 `profiles/desktop`

`desktop` 是上游保留名，CLI **硬拦截**（`apps/cli/src/args.ts:84`）：

```ts
if (profile.toLowerCase() === 'desktop') {
  program.error('error: profile "desktop" is managed exclusively by the Electron application')
}
```

进程以非零码退出；`toLowerCase()` 使得 `Desktop` / `DESKTOP` 同样被拦。
绕过只能不调 CLI 而自己用 Node 启动 profile runner，等于放弃公开接口，
并自行承担 profile 组合、插件解析、home patch 叠加。

上游独占该名是因为 Electron 在 `profiles/desktop` 里放平台插件
（文件拖拽、目录选择器、更新、账号后端）与自己的运行时。

### 客户端对 profile 的写入边界

客户端自身**从不写** profile 配置：

| 我们的操作 | 写入目标 | 写 profile |
| --- | --- | --- |
| 选模型 | `session/selectModel` → 会话配置 / `request/header` | 否 |
| 选 mode | `/permission <preset>` → 会话事件 | 否 |
| 建工作区 | `workspace/create` → `$DSH_HOME/storages` | 否 |
| 读列表 / 投影 / 流 | 只读 | 否 |

唯一的 profile 写入是**首次启动时代为生成 profile**
（带 `--from-default-profile web` 启动时顺带完成）。
之后的 provider 配置由用户手写本 profile 的 Cordis patch ——
客户端不解析、不改写、也不提供界面去做。

### 为什么不用 `dsh-desktop-host` 的 IPC 握手

上游 Electron 走另一条路：spawn `@deepseek-ai/dsh-desktop-host`，
通过 Node 子进程 IPC（fd 4）收发 `starting` / `desktop-auth` / `ready`，
凭据只走内存，cookie 名 `dsh-desktop-session`，协议版本常量
`DESKTOP_HOST_PROTOCOL_VERSION = 5`。

**不用它的理由**：

| 理由 | 说明 |
| --- | --- |
| 非公开面 | 该包是 Electron 发行物的一部分，不一定出现在普通 npm 安装 |
| 内部协议 | `desktop-auth` 握手、IPC 帧格式、协议版本均未文档化，升级风险高 |
| 需自实现 IPC | Rust 侧得实现 Node 子进程 IPC 帧（fd 3/4 + 换行分帧） |
| 收益不足 | 唯一好处是凭据不走 stdout，可用“不打印子进程输出”等效解决 |

**回看条件**：若上游开始把 `dsh --profile <name>` 的 token 交换标记为弃用，
或发布稳定的非 Electron host 握手规范。

**#41 补注**：不用它的机制，但对齐它的性质——凭据每次新生、走私通道
（我们的私有 stdout pipe 等价于它的 IPC）、独立 peer、生命周期同生共死。
见 DECISIONS #41 的性质对照表。

### 已知风险

| 风险 | 对策 |
| --- | --- |
| 与用户已开的 `dsh web` 共享 `$DSH_HOME` | 会话写入有 writer 锁（`session/writer-held`）；收到该错误时提示“已有另一个 dsh 在写会话”，不自行重试 |
| 会话日志并发写 | 同上；各表面各起各的 host，写入仍受 writer 锁保护（#41） |
| 两个表面同时跑 | 运行时层独占：各自的 host / profile / peer 身份；同 profile 双进程由单实例锁防住（#41） |
| launch token 泄露 | 只走私有 stdout pipe，不落盘、不进日志；ready URL 仅内存持有（#41） |
| 自带运行时版本低于用户已有 dsh | `$DSH_HOME/storages/*.json` 的 version 高于支持表就拒绝启动，不做迁移 |
| 自带运行时体积大 | 接受。north star 是运行时 RAM，不是安装包大小 |
| 用户机器无任何凭据 | 空模型列表：能打开、能看历史、能发（由 host 报错）。状态行写「请选择模型」，下拉里是「暂无可用模型」+ 提供方失败行（#42） |
| 端口已打印但连接失败 | 重试握手有限次后退回错误态，不无限循环 |

### 聊天详略的入口

全局聊天详略只在 settings 页面修改，由 `ChatVerbosity` 派生 `RenderPolicy` 后
交给渲染层。真值在 dsh host profile 的 `ui-chat.transcriptView`：启动经
`settings/describe` 读入，点击经 `settings/update` 写回；客户端只有内存镜像。
过程组头不再放档位切换器：它看起来像每组独立设置，实际却改全局。
时间线投影不因档位变化；档位只决定过程组开合、组头摘要、reasoning 是否显示
settled 首行预览。单个工具 / reasoning 正文永远手动展开，逐块的开关是会话内状态。

档位对应官方 `Work details` 的前三档：`low = compact`、`medium = standard`、
`high = detailed`。官方第四档 `verbose` 没有 UI 选项；若 host 里读到该值，native
客户端按最接近的 `high` 渲染，不宣称实现历史全展开。整轮 folding 暂不实现。

### 聊天时间线的边界

会话历史先投影成 `ChatTimeline::rows()` 的 `TimelineRow`，GPUI 才能绘制。
`Chat` 只保存投影器与展开/滚动等会话态；follow 快照、流式块、工具日志和
工具结果的身份/顺序/脱敏都归 `dsh-protocol::timeline`。渲染层不解析事件 JSON，
不把工具输出当 markdown，也不自行生成本地消息身份。

### 调试开关

提供 headless 模式（不建窗口）dump 协议数据：启动 host → 握手 → 拉一次
`session/list` + `workspace/follow` baseline + 首个会话投影 → 写 JSON 退出。
用于 CI 回归与 agent 自测。**输出必须剔除 token 与 cookie。**

界面与生命周期类的开关（均为环境变量）：

| 开关 | 作用 |
| --- | --- |
| `COWORK_SLEEP_IDLE_SECS` | 空闲多久收掉 host（缺省 600，`0` = 不休眠） |
| `DSH_DEBUG_WAKE_AFTER=<秒>` | 到点发一条 `RefreshSessions`，验证「休眠后能被命令叫醒」 |
| `DSH_DEBUG_OPEN=<id>` | 启动就打开该会话（也作为休眠醒来后的恢复目标） |
| `DSH_DEBUG_TOOL_OPEN=1` | 把所有工具行正文当成展开（验排版用；正常用法是点开） |
| `COWORK_RICH_TEXT=0` | 助手正文退回纯文本（同一入口的降级分支，量内存用） |

## 非目标的边界重申

- Web UI 归 dsh 自己（dsh 启动时即带 Web）。本项目不产出任何浏览器端代码。
- 不做 cordis 插件宿主。插件的加载、HMR、生命周期全在 dsh 侧，客户端只观察其效果（工具、事件、UiBlock）。
