# dsh-client-rs

Rust + GPUI 原生桌面客户端，为 [DeepSeek Harness (dsh)](https://github.com/deepseek-ai/deepseek-harness) 提供极致低内存的 desktop 界面。

> **设计初衷：将 desktop 的 RAM 占用减少到极致。** 所有技术决策向这一目标看齐。

## 定位

**使用范围：code 为主，通用 work 轻度。** 编程任务（读码、改码、跑命令、查 diff）
是主场景，全部交互按高频路径优化；通用工作能完成即可，不为它加专用界面。
表面优先级见 [docs/DESIGN.md](docs/DESIGN.md#使用定位code-为主通用-work-轻度)。

dsh 本体（Node.js）自带 Web UI 与完整 agent 运行时，且会自行启动 Web 服务。
本项目**只做 desktop 表面**，不重复实现 Web，不重写 harness：

```text
dsh-client-rs（纯 Rust，GPUI 自绘）      dsh（上游，Node.js）
├─ 窗口 / 渲染 / 交互                    ├─ agent 循环 / 模型 / MCP
├─ 协议客户端（HTTP/WS，可扩展 JSON-RPC） ├─ cordis 插件 / Web UI（不归本项目管）
└─ host 进程管理（spawn / 看门狗 / 重连）   └─ 会话持久化
```

## 为什么不是 Electron / TS

| 方案 | 空闲 RAM | 结论 |
| --- | --- | --- |
| Electron（Qoder desktop 现状） | 190-430MB | ✗ 含完整 Chromium |
| TS → GPU 自绘（GPUIX） | 120-180MB | ✗ 常驻 JS 引擎堆 40-80MB |
| **Rust → GPUI 自绘（本项目）** | **50-100MB** | ✓ 无脚本引擎税 |

参照实测：pi-gpui（Rust + GPUI 完整 agent GUI）16MB 安装包、0.4s 启动、75MB 空闲。
JS 引擎税实测：`node -e ''` 峰值 RSS ≈ 44MB（未跑任何应用代码）。

## 非目标（Non-goals）

- 不做 Web UI（dsh 自带）
- 不重写 agent 运行时 / harness
- 不在窗口进程中引入任何脚本运行时（V8 / JSC / QuickJS 常驻均排除）
- 不做移动端（远期另议）

## 打成 app 用

```bash
./scripts/build-product.sh          # cargo build --release + 组装 + ad-hoc 签名 + 安装到 /Applications
open /Applications/cowork.app       # Finder 双击也行
```

产物是一个自带的 `cowork/cowork.app`（1.4G，其中 1.3G 是内置的 dsh 运行时），
脚本同时把它安装到 `/Applications/cowork.app`（`COWORK_INSTALL_DIR` 可改）；
旧版本进废纸篓、可恢复，不直接 rm：

```text
cowork.app/Contents/
├─ MacOS/cowork                  ← target/release/cowork
└─ Resources/
   ├─ runtime/node_modules/…     ← dsh 运行时（APFS 克隆，秒级）
   └─ node-path.txt              ← 构建时找到的 node 绝对路径
```

**打开即用**：对齐官方 desktop——无视本机已在跑的 dsh web，永远自己拉起
`dsh --profile cowork`（运行时就在包里），从输出里抓 ready URL，不需要先开终端
（见 [docs/DECISIONS.md](docs/DECISIONS.md) #41）。会话/工作区/凭据在 `$DSH_HOME`
级共享，所以终端里的会话这里照样看得到；共享的是状态，不是进程。
profile 固定为 **`cowork`**，存在 `$DSH_HOME/profiles/cowork`
（缺省 `~/.dsh`），首次启动从内置 `web` profile 播种。

几个行为要知道：

| 行为 | 原因 |
| --- | --- |
| dsh web 在跑也各起各的 host | 运行时层独占（进程/profile/peer 身份），状态层共享；对齐官方 desktop（#41） |
| 同一 profile 只开一个实例 | 双击第二次会把已有窗口提到前面（单实例锁：临时目录里的 Unix socket） |
| 自己拉起的 host：app 退出时一起退 | host 由 `/bin/sh` 看门狗看着，app 被强杀也会在 1s 内收掉 node |
| `DSH_READY_URL` 可手动指定 | 带 token 的 ready URL，走 `/connect`（开发期逃生口，不是产品路径） |
| `DSH_HOME` 可覆盖 | 想隔离就用 `open -n cowork/cowork.app --env DSH_HOME=/path` |
| `COWORK_PROFILE` 可覆盖 profile 名 | 同时跑两个隔离实例用 |
| 启动就有侧栏，不等 host | `~/Library/Application Support/cowork/state.json` 存侧栏快照（会话/工作区/归档/隐藏与历史归属），启动先画再等真实数据；不缓存时间线（#44、#58） |
| 空闲就收掉 host，有活再拉 | 空闲 `COWORK_SLEEP_IDLE_SECS`（缺省 600，`0` 关闭）；休眠时只剩客户端，接口/会话不丢，唤醒约 2.4s（#44） |
| node 不打包 | 构建时把 `node` 绝对路径写进 `node-path.txt`；Node 换版本后重跑一次脚本，或设 `COWORK_NODE` |
| 启动先选工作区，不自动建会话 | 主区是空状态：列出已注册工作区 + 「添加工作区…」；会话建在你选的那个目录下，一定属于该工作区（#43） |
| 侧栏没有「未分组」 | Workspace 只显示工作区；无归属的历史会话不在侧栏出现（仍在 host 上，可搜索访问）（#58） |
| 移除工作区是可逆隐藏 | 侧栏「移除」只本地隐藏整组，不删 host 注册/会话；隐藏组的会话一起不显示，底部「已隐藏的工作区」可恢复；重新添加同一目录后老会话归位（#58） |
| 工具调用按 `callId` 配成一行，三档展开 | 历史与实时走同一个折叠器（`dsh-protocol` 的 `Transcript`）；标准档默认收起过程组，点行看输出，任何档位都不自动灌正文（#47） |
| 助手正文按 markdown 渲染 | 用 gpui-base 自带 `TextView::markdown`（Apache-2.0，已在依赖树）；身份用 `(turn, step)`，流式与提交后同一 key；工具输出不解析（#48） |
| 没有凭据时模型格显示「请选择模型」 | 对齐官方文案；目录空时下拉里是「暂无可用模型」，提供方失败逐条列出（#42） |
| 默认模型不手写，交给 magpie | profile 的 `cordis.patch.yml` 里 `agent-default-model # magpie` 由 magpie 维护（`magpie dsh <provider/model>` / `magpie dsh default`）；在 UI 里点模型芯片选一次也会让 host 自己写回这条（`session/selectModel` → `saveSelection`）。前提是 `~/.local/bin/dsh` 指向 app 内置 runtime，否则 magpie 只写 patch、不写凭据 |
| 模型目录可以不止官方两家 | `dsh-base` 以「零路由」挂着 `llm-pi-ai`，magpie 会往里注入 `magpie`（`openai-completions`）与 `magpie-anthropic`（`anthropic-messages`）两个路由共 23 个模型；密钥只以 `apiKeyEnv: MAGPIE_GATEWAY_KEY` 引用，真值在 `~/.dsh/.credentials.yaml` 的 `refs:`（0600） |

内存实测（release、`.app` 启动）：客户端 **82–112MB**（带 markdown 正文的会话到 111MB，
`COWORK_RICH_TEXT=0` 回纯文本可省 1–7MB，见 #48）；host **343MB**（`--max-semi-space-size=4` 后，
无对话、真实 `$DSH_HOME`；不调参是 417MB，见 #45）——且**只在有活时存在**，
空闲一阵就收掉，此时总内存≈客户端（#44）。
host 数字的工况与取样口径见 [docs/DECISIONS.md](docs/DECISIONS.md) #39/#44/#45。

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/DESIGN.md](docs/DESIGN.md) | 架构、crate 划分、内存预算、composer 控制条、UiBlock |
| [docs/MODEL.md](docs/MODEL.md) | 领域数据模型（wire 类型、表层折叠、流式状态机、协议一致性清单） |
| [docs/DECISIONS.md](docs/DECISIONS.md) | 关键决策记录（ADR，含依据与实测数据） |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | dsh 协议面笔记（端点、方法表、statusline 数据源） |

阅读顺序：DESIGN → MODEL → DECISIONS → PROTOCOL。
写 `dsh-protocol` 代码时以 MODEL.md 为唯一参照。

## 现状

纵向切片全部跑通：协议层、fold 状态机、GPUI 界面、真实流量端到端；
可打成 `cowork.app` 双击使用（见上一节），host 与 app 同生共死、无视本机
已在跑的 dsh web（#41）。已知缺口：运行时裁剪（1.3G）、退出确认、
正式签名/公证、设置面板。

## 相关项目（调研来源）

**首要来源：dsh 本体**
[deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) ——
协议与数据结构的权威依据，取源文件与 `docs/subsystems/`、
`docs/persistence-catalog.zh.md`、`.agents/notes/archived/architecture/`。

以下是对照与基准参照（非权威）：

- [jimoto-no-llm/rustdsh](https://github.com/jimoto-no-llm/rustdsh) — Rust + GPUI 壳 + dsh 后端的先驱（`apps/rdsh-desktop` 协议实现可参考）
- [bytesbay/pi-gpui](https://github.com/bytesbay/pi-gpui) — Rust + GPUI agent GUI 的内存基准（75MB）
- [zed-industries/zed](https://github.com/zed-industries/zed) — GPUI 框架本体
- [zaidmukaddam/fx-ui](https://github.com/zaidmukaddam/fx-ui) — 视觉语言基准（技术路线已否决，见 DECISIONS #1/#7）
- [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit) — 组件层（见 DECISIONS #6）
