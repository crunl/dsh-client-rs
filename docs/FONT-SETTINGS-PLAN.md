# 字号与字体族设置：实施计划

> 状态：待实施（2026-11-23 编写）
> 目标：把官方 desktop 的「字号 + 字体族（含回退）」能力搬进本项目 settings 页。
> 依据：官方 `deepseek-harness` 源码 `packages/client/ui-theme`（`theme-settings.ts`、`FontSettingsGroup.tsx`、`FontSizeRow.tsx`、`FontFamilyRow.tsx`、`client/index.ts`、`README.zh.md`）与本项目 GPUI 实现现状，均逐项核实。

## 0. 一句话

官方靠 CSS 自定义属性级联实现「一处改、全局生效」；本项目是 GPUI 自绘，字号是编译期常量、字体族是单值，**照搬 CSS 方案不成立**。本计划的做法是：真值仍存官方 `ui-theme` namespace（与 desktop/web 共享），在客户端增加一层运行时解析，把 74 处硬编码字号与 3 处字体族改为从这一层读取，UI 形态照官方。

## 1. 目标与范围

### 本期做

- 三个角色的**字号**与**字体族**都可在 settings 页修改，含用户字体列表的**回退链**与「留空恢复内置栈」。
- 真值写入 host 的 `ui-theme` settings namespace，经 `settings/describe` 读、`settings/update` 写，带 `expectedRevision`，与官方 desktop/web 共享同一份配置、跨重启保留。
- UI 形态对齐官方：「通用」区放正文字号步进器 +「更多字体设置」展开区（含正文/代码/终端字体行与代码/终端字号步进器，每次进入默认折叠）。

### 本期不做

- 外观主题切换（`light`/`dark`/`system`）：本项目固定深色，超范围。
- 侧栏终端的字体/字号**渲染**：本项目没有终端组件；`terminal` 字段照常读写以保持 wire 兼容，但 UI 不暴露、渲染不接入。
- 本机字体枚举：与官方一致，只给自由输入框，本机缺字体靠回退兜底。
- `Cmd/Ctrl +` / `-` 快捷键：UI 落地后作为收尾小项顺带，不单列阶段。

## 2. 官方契约（值与行为必须对齐）

来源：`theme-settings.ts:24-30,57-76`、`README.zh.md:30-40`。

| 角色 | 字号字段 | 范围（整数 px） | 默认 | 字体族字段 | 默认 |
| --- | --- | --- | --- | --- | --- |
| `text`（界面 + 会话正文） | `fontSize` | 10–22 | 14 | `textFontFamily` | `''` |
| `code`（代码块、行内码、工具输出） | `codeFontSize` | 10–16 | 11 | `codeFontFamily` | `''` |
| `terminal`（侧栏终端） | `terminalFontSize` | 10–20 | 13 | `terminalFontFamily` | `''` |

行为规则：

- 字号是整数、步长 1，超出范围或非整数拒绝；行内代码比代码块大 1px。
- 字体族输入是逗号分隔字符串；官方 `normalizeFontFamily` 去掉引号、反斜杠、尖括号、控制字符，压缩空白，给 CSS 通用族名以外的值加双引号；`''` 表示用内置栈。
- 用户列表排在内置字体栈**之前**，本机缺字体自动回退到内置栈。
- 写入 `ui-theme` namespace，host 默认持久化到 `$DSH_HOME/cordis.patch.yml`，每个已认证浏览器独立、跨重启保留。

## 3. 本项目现状与关键约束

### 3.1 字号是编译期常量，散在 74 处调用点

`crates/dsh-client-ui/src/theme.rs`：`BODY=14 / UI=13 / ROW=12 / META=11`。引用统计：`.text_size(px(theme::…))` 共 **74 处**（`chat.rs` 73、`markdown.rs` 1）；按 token 分布 `META` 44、`UI` 18、`BODY` 11、`ROW` 3。`markdown.rs` 另有 `theme::BODY - 1.0` 用于 reasoning。

改法：引入运行时解析结果，74 处逐一改为读解析值；不能只改根元素——显式 `.text_size()` 会覆盖继承。

### 3.2 字体族是单值 + 显式回退，不能直接吃 CSS 列表

- 本项目 `.font_family(theme::MONO)` 共 3 处（`markdown.rs:46,123`、`chat.rs:4216`），且根 `div()` 在 `chat.rs:4216` 设置一次后向全窗口继承。
- GPUI 的 `Styled::font_family` 只接受**单个** `SharedString`（`gpui-pre-0.3.12/src/styled.rs:708`），没有 CSS 逗号回退链。
- 回退要用 `Styled::font(Font { family, fallbacks: Some(FontFallbacks::from_fonts(rest)), .. })`（`styled.rs:720`）；`FontFallbacks` 是 `Arc<Vec<String>>` 的有序列表（`text_system/font_fallbacks.rs`）。
- 结论：官方 normalize 的输出是**给 CSS 用的带引号列表**，GPUI/CoreText 不能直接消费。必须做双向转换：host 里存官方格式（保证 desktop/web 兼容），渲染前拆成 `family + fallbacks` 并去引号。

### 3.3 设置通道已具备泛化条件

- `settings/describe` 返回全部 namespace，wire 形状 `SettingsNamespaceView { ns, value, user, revision }` 已是通用的（`crates/dsh-protocol/src/settings.rs`）。
- `apps/desktop/src/host_settings.rs` 现在硬编码找 `ui-chat` + `transcriptView`，小改即可按 namespace 名取 `ui-theme`。
- 本项目无终端组件，故 `terminal` role 只读写、不渲染。

## 4. 设计

### 4.1 数据模型

`Settings`（`crates/dsh-client-ui/src/settings.rs`）当前只有 `verbosity` 且是 `Copy`；加字体族字符串后改为 `Clone`：

```rust
pub struct Settings {
    pub verbosity: ChatVerbosity,
    pub font_sizes: FontSizes,       // text / code / terminal
    pub font_families: FontFamilies, // 官方格式的 CSS 列表字符串，'' 为内置栈
    pub font_revision: u64,          // ui-theme namespace revision
}
```

新增 `crates/dsh-client-ui/src/font.rs`，职责收敛在一处：

- `FontRole`（`Text`/`Code`/`Terminal`）与 `FONT_SPECS`（范围、默认值、字段名），数值照第 2 节表格。
- `normalize_font_family(&str) -> String`：对齐官方 `normalizeFontFamily`，用于写回 host。
- `gpu_font_parts(&str) -> (String, Vec<String>)`：把官方格式拆成 GPUI 的 `family` + `fallbacks`（去掉 CSS 引号，过滤空项）。
- `ResolvedFonts`：渲染时一次性派生，含 `body/ui/row/meta/code/code_inline` 与三个角色的 `(family, fallbacks)`。

### 4.2 四档 token 与官方三角色的映射

| 官方 role | 本项目用途 | 映射规则 |
| --- | --- | --- |
| `text`（`fontSize`，10–22，默认 14） | 主基准，驱动 `BODY` 及 `UI`/`ROW`/`META` | `UI = text - 1`、`ROW = text - 2`、`META = text - 3`，默认时恰好是 14/13/12/11，与现状完全一致；`META` 下限 clamp 到 9，避免元信息不可读 |
| `code`（`codeFontSize`，10–16，默认 11） | 新增 `CODE` token，默认 11 | markdown 代码块；行内代码 `code + 1`（对齐官方）；reasoning 仍为 `body - 1` |
| `terminal`（`terminalFontSize`，10–20，默认 13） | 无组件 | 只读写 wire 字段，不参与渲染 |

### 4.3 回退规则

以 `text` 角色为例，优先级从高到低：

1. 用户列表第一项 → `Font.family`；
2. 用户列表其余项 → `FontFallbacks::from_fonts(...)`；
3. 内置栈（`Menlo` → 系统等宽字体）→ GPUI/CoreText 最终兜底；
4. 字段为 `''` 时跳过 1、2，直接用内置栈（等价官方留空行为）。

`code` 角色同理，内置栈仍为 `Menlo`（维持 DECISIONS #7 的 mono 基线）。

### 4.4 数据流

- **启动**：`host_settings::describe_theme` 从 host 读 `ui-theme` → 解析写入 `Settings` → `Chat` 持有。
- **设置页点击**：`UiCommand::SetFontSize` / `SetFontFamily` → `host_settings::update_theme`（带 `expectedRevision`）→ host 回送权威值 → `Chat` 更新并 `cx.notify()`；写失败沿用现有 verbosity 的回滚 + 错误提示模式。
- **渲染**：`Chat::render` 开头构造一次 `ResolvedFonts`，74 处字号引用与 3 处字体族引用改读它；`theme::markdown_style()` 参数化 `code` 字号与字体，通过 `TextViewStyle::with_code_block(StyleRefinement)` 和 `with_inline_code_style(StyleRefinement)` 注入（两个 API 均支持 font family/size/fallbacks）。

## 5. 实施步骤

### Phase 0 — host 设置通道（可独立验证，暂无 UI）

1. `apps/desktop/src/host_settings.rs`：把 namespace 查找泛化，新增 `describe_theme(conn) -> ThemeSetting` 与 `update_theme(conn, revision, patch) -> ThemeSetting`；读时优先 `user` 层、回退 `value` 层（复用 `setting_value` 的语义），写时带 `expectedRevision`。
2. `crates/dsh-protocol/src/settings.rs`：wire 形状已通用，预计无需改动；若需加语义，只加字段、不改现有。
3. 单测覆盖：`ui-theme` namespace 的解析、`user` 覆盖 `value`、缺失字段回退默认值、revision 冲突返回错误。
4. 验证：`cargo test -p dsh-desktop`。

### Phase 1 — 数据模型与渲染层（设置仍无 UI，但可用代码注入验证效果）

1. 新增 `crates/dsh-client-ui/src/font.rs`（第 4.1 节）。
2. `crates/dsh-client-ui/src/settings.rs`：`Settings` 扩展字段、`Copy` → `Clone`。
3. `crates/dsh-client-ui/src/theme.rs`：`markdown_style` / `reasoning_markdown_style` 参数化 code 字号与字体。
4. `crates/dsh-client-ui/src/chat.rs` 与 `markdown.rs`：74 + 1 处字号引用、3 处字体族引用改读 `ResolvedFonts`。
5. 单测覆盖：`normalize_font_family` 对齐官方用例（引号/反斜杠/控制字符/通用族名/空串）、`gpu_font_parts` 拆分、`text → UI/ROW/META` 映射与 clamp、行内码 +1。
6. 验证：`cargo test -p dsh-client-ui`。

### Phase 2 — settings 页 UI 与写回

1. `crates/dsh-client-ui/src/lib.rs`：`UiCommand` 新增 `SetFontSize { role, px, previous }`、`SetFontFamily { role, value, previous }`；`Settings` 视图事件增加对应分支。
2. `crates/dsh-client-ui/src/chat.rs`：在现有 settings 页（verbosity 行附近，`chat.rs:2050` 起）加正文字号步进器与「更多字体设置」展开区；步进器到边界禁用，输入框 `maxLength=200`、失焦/Enter 提交、留空恢复默认，全部显示持久值而非点击回显。
3. `apps/desktop/src/main.rs`：事件处理调用 `host_settings::update_theme`，回送权威值并复用现有 `settings_error` / `settings_saved` 反馈。
4. 快捷键：`Cmd/Ctrl +` / `-` 三角色各 ±1px，一次 mutation 写回（官方 `adjustFontSizes` 语义）。
5. 单测覆盖：写失败回滚、展开区默认折叠、边界禁用。
6. 验证：`cargo test --workspace`。

### Phase 3 — 文档与产物

1. 更新 `docs/DESIGN.md`（settings 页能力、字体/字号数据流）、`docs/DECISIONS.md` 新增 #62（决策与取舍）。
2. `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
3. 用 embedded code 包重建安装：`COWORK_RUNTIME_PACKAGE_DIR=runtime-build/code ./scripts/build-product.sh`，核对 manifest/hash/app 大小。
4. headless 环境无法做 GUI 目视验证，需在文档中如实记录。

## 6. 验收标准

- settings 页可改正文字号（10–22）、代码字号（10–16），步进器边界正确；字体输入框接受逗号分隔列表、留空恢复内置栈。
- 改完后界面立即生效；重启 app 后设置保留（真值在 host profile）。
- 用户字体列表首个存在时用首个，否则依次回退，最终回退到 `Menlo`；不含 CSS 引号泄漏到 GPUI。
- 写入 host 的字段名与官方一致（第 2 节表格），可被官方 desktop/web 读取。
- `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` 全绿，无新增外部依赖。

## 7. 风险与取舍

- **存储格式双向转换是最易错点**：host 存官方 CSS 列表（带引号），GPUI 要拆成单名 + 回退列表。Phase 1 单测必须覆盖。
- **74 处机械替换的风险**：`META` 占 44 处，改错会导致层级混乱。按 token 分批替换，每批跑截图可比对的单测/编译检查（headless 下以编译 + 单测为准）。
- **`Settings` 由 `Copy` 变 `Clone`**：影响所有持有处，需全量编译确认未破坏移动语义。
- **字体设置的视觉基线**：正文维持 `Menlo`，避免推翻 DECISIONS #7；若用户设置非等宽字体，这是显式选择而非默认。
- **headless 限制**：字号/字体的真实字形与回退效果无法目视验证，只能验证解析、映射、wire 与持久化逻辑。

## 8. 开放问题（已给推荐，如无否决按此执行）

1. `terminal` role 只读写、不渲染（本项目无终端组件）。
2. `text` 为主基准，`UI`/`ROW`/`META` 按 `-1/-2/-3` 联动并 clamp，而非四个独立控件（控件更少、与官方"正文为主"一致）。
3. host 中存官方格式字符串，GPUI 侧转换，保证与 desktop/web 双向兼容。
4. 外观主题切换不做，保持固定深色。
