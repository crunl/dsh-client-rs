//! 主视图：侧栏 + 主区（标题栏 / 时间线 / composer）。
//!
//! 几何全部取自 Ardot 设计稿 `747373011992944`：侧栏 264、内容列 860、
//! 顶/底栏 52、主区水平内边距 28、侧栏内边距 16。
//!
//! 数据流是单向的：外部把 [`UiEvent`] 推进 channel，本视图消费后更新状态，
//! 用户输入通过 [`UiCommand`] 推回去。视图内**不做网络**。

use dsh_protocol::fold::{AssistantStreamFold, FoldOutcome};
use dsh_protocol::workspace::WorkspaceRegistry;
use gpui_kit::base::input::{InputEditorStyle, InputEvent, Textarea, TextareaState};
use gpui_kit::*;
use std::collections::HashMap;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::completion;
use crate::theme;
use crate::{
    CompletionItem, CtxMeter, EffortOption, ModelGroup, PresetInfo, SelectedModel, SessionInfo,
    UiCommand, UiEvent,
};
use dsh_protocol::context::Occupancy;
use dsh_protocol::transcript::{Entry, MessageId, ToolCall, ToolStatus};
use gpui_kit::assets::IconName;

/// 工作步骤展示模式（对齐官方 `ui-chat.transcriptView` 的三档，桌面默认「标准」）。
///
/// 差别只在**过程组**怎么组织，不在单个工具正文：任何档位下单行正文都靠手动展开，
/// 否则长输出会把时间线冲垮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TranscriptView {
    /// 简洁：组头只给类别摘要。
    Compact,
    /// 标准：组头给摘要 + 运行中的实时提示。
    #[default]
    Standard,
    /// 详细：运行中的轮次直接展开，历史轮次仍按手动开合。
    Detailed,
}

impl TranscriptView {
    fn label(self) -> &'static str {
        match self {
            TranscriptView::Compact => "简洁",
            TranscriptView::Standard => "标准",
            TranscriptView::Detailed => "详细",
        }
    }

    /// 点一下换下一档，跑一圈回到起点。
    ///
    /// 没做成设置项是因为我们还没有设置面板：挂在组头旁边既看得见、又不用二次跳转。
    fn next(self) -> Self {
        match self {
            TranscriptView::Compact => TranscriptView::Standard,
            TranscriptView::Standard => TranscriptView::Detailed,
            TranscriptView::Detailed => TranscriptView::Compact,
        }
    }

    /// 启动时的默认档；`COWORK_TRANSCRIPT_VIEW` 可覆盖（也方便验收截图）。
    fn from_env() -> Self {
        match std::env::var("COWORK_TRANSCRIPT_VIEW").as_deref() {
            Ok("compact") => TranscriptView::Compact,
            Ok("detailed") => TranscriptView::Detailed,
            _ => TranscriptView::default(),
        }
    }

    /// 过程组正文默认是否展开。
    fn group_body_open(self, running_turn: bool) -> bool {
        matches!(self, TranscriptView::Detailed) && running_turn
    }
}

/// 主视图。
pub struct Chat {
    input: Entity<TextareaState>,
    entries: Vec<Entry>,
    fold: AssistantStreamFold,
    /// 本地已显示、host 还没给 `seq` 的消息计数器（只用于 `Entry::User` 的身份）。
    local_seq: u64,
    meter: Option<CtxMeter>,
    commands: Sender<UiCommand>,
    /// 工作区名（取 cwd 的末段）。
    workspace: String,
    /// 会话标题；拿到投影标题前用会话 id 尾部代替。
    title: String,
    /// 会话根的 `~` 缩写路径。
    path: String,
    connected: bool,
    /// host 被收起了（空闲休眠）。界面不依赖它活着：侧栏/时间线都是客户端自己的状态。
    sleeping: bool,
    /// 当前会话这一轮处在哪一步（composer 主按钮、时间线尾巴、侧栏点共用）。
    phase: Phase,
    /// 本轮开始的时刻；只在 `Sending` / `Running` 时有值，用来给「正在思考」计时。
    turn_started: Option<std::time::Instant>,
    /// 侧栏会话列表，来自 `session/list`。
    sessions: Vec<SessionInfo>,
    /// 当前打开的会话 id。
    active_session: String,
    /// 工作区展开状态，按 cwd 记；缺省展开。
    expanded: HashMap<String, bool>,
    /// 工作步骤展示模式。
    view: TranscriptView,
    /// 已展开正文的工具行（`callId`）。
    open_tools: std::collections::HashSet<String>,
    /// 仅调试：把每个工具行的正文都当成展开（验收排版用）。
    debug_tools_open: bool,
    /// 过程组的开合；没有记录的组按模式默认。
    open_groups: HashMap<String, bool>,
    /// 左侧栏是否展开（侧栏顶部的关闭按钮收起，标题栏左侧的按钮展开）。
    sidebar_open: bool,
    /// 右侧面板是否展开（标题栏面板图标切换）。
    right_panel_open: bool,
    completion: Option<Completion>,
    completion_seq: u64,
    /// 鼠标悬在哪一行候选上（与键盘选中分开：悬停时不抢选中）。
    hover_completion: Option<usize>,
    /// 工作区注册表 + 归档/置顶集合（侧栏分组的唯一来源）。
    registry: WorkspaceRegistry,
    /// 正在等二次确认的工作区 id（垃圾桶按一下不直接删）。
    pending_delete: Option<String>,
    /// 正在等二次确认的**运行中**会话 id（归档会停掉它，所以先问一句）。
    pending_archive: Option<String>,
    /// 按键拦截器的订阅；丢掉就失效，所以必须攥在手里。
    _completion_keys: Subscription,
    /// 当前会话的权限 preset（composer 左侧 mode）。
    mode: String,
    /// 可选 preset 目录。
    mode_options: Vec<PresetInfo>,
    /// mode 下拉是否展开。
    mode_open: bool,
    /// mode 控件的 hover 态。
    hover_mode: bool,
    /// 模型目录，按提供方分组。
    model_groups: Vec<ModelGroup>,
    /// 提供方级故障文案（官方 `{name} 加载失败：{message}`）；空则不显示。
    model_failures: Vec<String>,
    /// host 的默认选择（会话还没选过时显示它）。
    model_default: Option<SelectedModel>,
    /// 当前会话的模型选择。
    model: Option<SelectedModel>,
    /// 模型下拉是否展开。
    model_open: bool,
    /// 强度下拉是否展开。
    effort_open: bool,
    /// 模型控件的 hover 态。
    hover_model: bool,
    /// 强度控件的 hover 态。
    hover_effort: bool,
    /// 上下文占用；缺失时不画圆环。
    occupancy: Option<Occupancy>,
    /// 缓存命中率百分比。
    cache_hit: Option<u8>,
    /// 仅调试：等目录到了要切过去的模型。
    debug_model: Option<String>,
    /// 左侧栏关闭按钮的 hover 态。
    hover_side_close: bool,
    /// 左侧栏展开按钮的 hover 态。
    hover_side_open: bool,
    hover_composer: bool,
    hover_panel: bool,
    _enter: Subscription,
}

/// 触发器的外观与状态。
struct ChipSpec {
    /// 元素 id（浮动层的定位锚点）。
    id: &'static str,
    /// 显示文本。
    label: String,
    /// 是否展开。
    open: bool,
    /// 是否 hover。
    hovered: bool,
    /// hover 状态回写。
    on_hover: fn(&mut Chat, bool),
    /// 按下时干嘛。
    on_press: fn(&mut Chat),
}

/// 上拉浮层的规格。
struct PullSpec {
    /// 包裹层 id。
    id: &'static str,
    /// 是否展开。
    open: bool,
    /// 面板宽度。
    width: f32,
    /// 行。
    rows: Vec<PullRow>,
}

/// 上拉浮层里的一行。
#[derive(Debug, PartialEq, Eq)]
struct PullRow {
    /// 选中后回传给 `pick` 的键。
    key: String,
    /// 主标签。
    label: String,
    /// 右侧次要信息（如提供方名）。
    hint: Option<String>,
    /// 是否当前选中项。
    selected: bool,
    /// 只用于提示、不可点（空态与故障行）：不改 hover、不接鼠标。
    muted: bool,
}

/// 模型下拉的行：目录 → 可选行，提供方故障与空态 → 不可点的提示行。
///
/// 文案与优先顺序对齐官方 `dsh-client-ui-model-selection`：
/// 有目录就列目录（末尾附故障），目录空且无故障才给「暂无可用模型」。
fn model_menu_rows(
    groups: &[ModelGroup],
    failures: &[String],
    current: Option<&SelectedModel>,
) -> Vec<PullRow> {
    let multi_provider = groups.len() > 1;
    let mut rows: Vec<PullRow> = groups
        .iter()
        .flat_map(|group| {
            group.models.iter().map(move |item| PullRow {
                key: format!("{}/{}", group.provider, item.id),
                label: item.name.clone(),
                hint: multi_provider.then(|| group.provider_name.clone()),
                selected: current.is_some_and(|selected| {
                    selected.provider == group.provider && selected.model == item.id
                }),
                muted: false,
            })
        })
        .collect();

    // 官方 `warning.groupLoad`：提供方列举失败要看得见，否则「有凭据但拉不到目录」
    // 会伪装成「什么都没配」。
    for (index, failure) in failures.iter().enumerate() {
        rows.push(PullRow {
            key: format!("failure/{index}"),
            label: failure.clone(),
            hint: None,
            selected: false,
            muted: true,
        });
    }

    if rows.is_empty() {
        rows.push(PullRow {
            key: "empty".to_owned(),
            label: "暂无可用模型".to_owned(),
            hint: None,
            selected: false,
            muted: true,
        });
    }

    rows
}

/// 候选标签：文件路径保留尾巴（文件名），命令名保留开头。
fn label_cell(item: &CompletionItem, selected: bool, is_file: bool) -> Div {
    let tone = if selected {
        theme::INK
    } else {
        theme::INK_SOFT
    };
    let cell = div()
        .flex_1()
        .min_w(px(0.))
        .whitespace_nowrap()
        .text_color(rgb(tone))
        .child(item.label.clone());
    if is_file {
        cell.text_ellipsis_start()
    } else {
        cell.text_ellipsis()
    }
}

/// 正在进行的一次补全。
#[derive(Clone)]
struct Completion {
    /// 哪一类触发。
    kind: completion::Kind,
    /// 待替换区间（字节偏移，含触发字符）。
    span: std::ops::Range<usize>,
    /// `@` 是否用引号起的头。
    quoted: bool,
    /// 候选，由 host 结果填。
    items: Vec<CompletionItem>,
    /// 键盘高亮行：**默认没有**（`None`）。
    ///
    /// 官方 `input-trigger` 的口径是「没高亮时 Enter 放行」：菜单开着也只在你按过
    /// 上下/Tab 之后才吃 Enter。这样「打 `@` 时随手回车」依旧是把话发出去，
    /// 而不是默默插一个没看过的候选。
    selected: Option<usize>,
}

/// 输入停滞多久才去问 host。`@` 会起 ripgrep，逐字查太浪费。
const COMPLETION_DEBOUNCE_MS: u64 = 130;

/// 浮层最多显示几行候选。
const COMPLETION_ROWS: usize = 8;

impl Chat {
    /// 建视图并接上事件泵。
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        mut events: Receiver<UiEvent>,
        commands: Sender<UiCommand>,
    ) -> Self {
        let input = cx.new(|cx| {
            // 多行输入：`submit_on_enter` 让裸 Enter 只发 `PressEnter`（不插换行），
            // 换行留给 `Shift+Enter`（库内已是这个口径）；
            // auto grow 从 1 行长到 6 行，再高就自己滚。
            let mut state = TextareaState::new(window, cx)
                .placeholder("给 dsh 说点什么…")
                .submit_on_enter(true);
            state.set_auto_grow(1, 6, cx);
            // 不投影编辑器样式时，caret 取默认调色板的 foreground（近黑），
            // 在深色卡片上等于看不见；正文/占位/选中也一并按主题给。
            state.set_editor_style(InputEditorStyle {
                foreground: rgb(theme::INK).into(),
                muted_foreground: rgb(theme::INK_MUTED).into(),
                background: rgb(theme::BG_CARD).into(),
                border: rgb(theme::BG_CARD_EDGE).into(),
                selection: rgb(theme::INK_SOFT).into(),
                caret: rgb(theme::INK).into(),
                ..InputEditorStyle::default()
            });
            cx.notify();
            state
        });

        // 不聚焦就没有插入符：输入框只在获焦时闪烁光标。
        // 构造时窗口还没上屏，聚焦会被丢弃，所以首帧后再补一次；
        // 之后每次窗口被激活都重新聚焦，符合「点回来就能直接打字」的预期。
        input.update(cx, |state, cx| state.focus(window, cx));
        {
            let input = input.clone();
            window.on_next_frame(move |window, cx| {
                window.activate_window();
                input.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        cx.observe_window_activation(window, |chat, window, cx| {
            if window.is_window_active() {
                let input = chat.input.clone();
                input.update(cx, |state, cx| state.focus(window, cx));
            }
        })
        .detach();

        // 补全开着时接管按键：Tab 向下、Shift+Tab 向上、Enter 选中、Esc 收起。
        // 必须用 keystroke 拦截器——它在动作派发**之前**跑，所以 Tab（焦点遍历）
        // 和 Enter（输入框提交）都能被我们截住。`capture_key_down` 做不到：
        // 它早于事件冒泡，却晚于键绑定解析。
        let keys = cx.entity();
        let completion_keys = cx.intercept_keystrokes(move |event, window, cx| {
            let key = event.keystroke.key.as_str();
            let shift = event.keystroke.modifiers.shift;
            let handled = keys.update(cx, |chat, cx| chat.completion_key(key, shift, window, cx));
            if handled {
                cx.stop_propagation();
            }
        });

        let enter = cx.subscribe_in(
            &input,
            window,
            |chat: &mut Self, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    // 补全开着时回车是「选中候选」，不是发送。这里拦得住是因为
                    // PressEnter 是输入框自己发的事件；capture_key_down 里
                    // stop_propagation 挡不住键绑定，回车必须在事件层分流。
                    // 注意：只有用键盘选过一条（`selected`）才是插入；
                    // 刚弹出、没选过的面板不得吞掉回车（官方也是这个口径）。
                    let picking = chat.completion.as_ref().is_some_and(|completion| {
                        !completion.items.is_empty() && completion.selected.is_some()
                    });
                    if picking {
                        chat.accept_completion(window, cx);
                    } else {
                        chat.send(window, cx);
                    }
                }
                if matches!(event, InputEvent::Change) {
                    chat.refresh_completion(cx);
                }
            },
        );

        cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |chat, cx| chat.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 「正在思考 · Ns」的计时：只在有轮次进行时每秒重画一次，
        // 空闲时这个任务只悬在定时器上，不碰渲染。
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let alive = this
                    .update(cx, |chat, cx| {
                        if chat.phase.is_busy() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            input,
            entries: Vec::new(),
            fold: AssistantStreamFold::new(),
            local_seq: 0,
            meter: None,
            commands,
            workspace: "未连接".to_owned(),
            title: "新会话".to_owned(),
            path: String::new(),
            connected: false,
            sleeping: false,
            phase: Phase::Idle,
            turn_started: None,
            sessions: Vec::new(),
            active_session: String::new(),
            expanded: HashMap::new(),
            view: TranscriptView::from_env(),
            open_tools: std::collections::HashSet::new(),
            debug_tools_open: std::env::var("DSH_DEBUG_TOOL_OPEN").is_ok(),
            open_groups: HashMap::new(),
            sidebar_open: true,
            right_panel_open: false,
            completion: None,
            completion_seq: 0,
            hover_completion: None,
            registry: WorkspaceRegistry::default(),
            pending_delete: None,
            pending_archive: None,
            mode: String::new(),
            mode_options: Vec::new(),
            mode_open: false,
            hover_mode: false,
            model_groups: Vec::new(),
            model_failures: Vec::new(),
            model_default: None,
            model: None,
            model_open: false,
            effort_open: false,
            hover_model: false,
            hover_effort: false,
            occupancy: None,
            cache_hit: None,
            debug_model: None,
            hover_side_close: false,
            hover_side_open: false,
            hover_composer: false,
            hover_panel: false,
            _completion_keys: completion_keys,
            _enter: enter,
        }
    }

    /// 仅调试：启动后切一次模型，走与点选完全相同的命令通道。
    ///
    /// 目录还没到时先记下来，等 `Models` 事件到达再补发。
    #[doc(hidden)]
    pub fn debug_set_model(&mut self, model: String) {
        self.debug_model = Some(model.clone());
        self.apply_debug_model(&model);
    }

    fn apply_debug_model(&mut self, model: &str) {
        let picked = self
            .model_groups
            .iter()
            .find(|group| group.models.iter().any(|item| item.id == model));
        let Some(group) = picked else { return };
        let model = model.to_owned();
        let provider = group.provider.clone();
        let effort = group
            .models
            .iter()
            .find(|item| item.id == model)
            .and_then(|item| item.default_effort.clone());
        let _ = self.commands.try_send(UiCommand::SetModel {
            provider,
            model,
            effort,
        });
    }

    /// 仅调试：启动后切一次权限 preset，走与点选完全相同的命令通道。
    #[doc(hidden)]
    pub fn debug_set_mode(&mut self, mode: String) {
        let _ = self.commands.try_send(UiCommand::SetMode(mode));
    }

    /// 仅调试：把草稿设成 `text` 并立刻触发一次补全（不改输入框的 Change 订阅）。
    #[doc(hidden)]
    pub fn debug_complete(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.refresh_completion(cx);
    }

    /// 仅调试：等价于点下当前高亮的候选。
    #[doc(hidden)]
    pub fn debug_accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.accept_completion(window, cx);
    }

    /// 仅调试：当前草稿。
    #[doc(hidden)]
    pub fn debug_draft(&self, cx: &Context<Self>) -> String {
        self.input.read(cx).value().to_string()
    }

    /// 仅调试：补全面板是否开着。
    #[doc(hidden)]
    pub fn debug_completion_open(&self) -> bool {
        self.completion.is_some()
    }

    /// 仅调试：当前权限 preset。
    #[doc(hidden)]
    pub fn debug_mode(&self) -> &str {
        &self.mode
    }

    /// 仅调试：等价于「点 Workspaces 右侧的 + 并选中这个目录」。
    #[doc(hidden)]
    pub fn debug_add_workspace(&mut self, path: String) {
        let _ = self.commands.try_send(UiCommand::AddWorkspace { path });
    }

    /// 仅调试：等价于「在输入框里打完字按回车」。
    #[doc(hidden)]
    pub fn debug_send(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.send(window, cx);
    }

    fn handle_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        match event {
            UiEvent::Ready { origin } => {
                tracing::info!(%origin, "host 就绪");
                self.connected = true;
                self.sleeping = false;
            }
            UiEvent::HostState { awake } => {
                tracing::info!(awake, "host 状态");
                self.sleeping = !awake;
                if awake {
                    self.connected = true;
                }
            }
            UiEvent::Connected {
                origin,
                session,
                cwd,
            } => {
                tracing::info!(%origin, %session, %cwd, "已连接 host");
                self.connected = true;
                self.workspace = leaf(&cwd);
                self.path = tilde(&cwd);
                self.title = short_id(&session).to_owned();
                self.active_session = session;
                // 上一场会话的读数不能带过来。
                self.occupancy = None;
                self.cache_hit = None;
                self.meter = None;
            }
            UiEvent::Sessions(items) => {
                self.sessions = items;
                // 标题/工作区以列表里的会话元信息为准：切会话时它才跟着变。
                if let Some(active) = self
                    .sessions
                    .iter()
                    .find(|item| item.id == self.active_session)
                {
                    if !active.cwd.is_empty() {
                        self.workspace = leaf(&active.cwd);
                        self.path = tilde(&active.cwd);
                    }
                    self.title = active
                        .title
                        .clone()
                        .unwrap_or_else(|| short_id(&active.id).to_owned());
                    if let Some(mode) = &active.mode {
                        self.mode = mode.clone();
                    }
                    self.model = active.model.clone();
                }
            }
            UiEvent::Workspaces(registry) => {
                tracing::debug!(
                    archived = registry.archived_session_ids.len(),
                    matched = registry
                        .archived_session_ids
                        .iter()
                        .filter(|id| self.sessions.iter().any(|session| &session.id == *id))
                        .count(),
                    workspaces = registry.items.len(),
                    sessions = self.sessions.len(),
                    "侧栏注册表"
                );
                self.registry = registry;
                self.settle_pending();
                cx.notify();
            }
            UiEvent::Presets(options) => {
                self.mode_options = options;
            }
            UiEvent::Context {
                occupancy,
                cache_hit,
            } => {
                self.occupancy = occupancy;
                self.cache_hit = cache_hit;
            }
            UiEvent::Models {
                groups,
                default,
                failures,
            } => {
                self.model_groups = groups;
                self.model_default = default;
                self.model_failures = failures;
                if let Some(model) = self.debug_model.clone() {
                    self.apply_debug_model(&model);
                }
            }
            UiEvent::Transcript(items) => {
                // 快照是「请求回放那一刻」的样子。刚发出上一轮时它会晚到、内容比界面旧：
                // 那条乐观气泡和进行中的一轮都会被抹掉，所以忙着一轮时直接丢掉。
                if self.phase.is_busy() {
                    return;
                }
                self.fold = AssistantStreamFold::new();
                self.entries = items;
                self.open_tools.clear();
                self.open_groups.clear();
            }
            UiEvent::Tool(call) => {
                // 按 `callId` upsert：发起时插一行，结果回来时原地补状态与正文。
                match self
                    .entries
                    .iter_mut()
                    .find(|entry| matches!(entry, Entry::Tool(existing) if existing.id == call.id))
                {
                    Some(Entry::Tool(existing)) => *existing = call,
                    _ => self.entries.push(Entry::Tool(call)),
                }
            }
            UiEvent::Frame(frame) => {
                // 帧到了：进「正在思考」。
                // 两种边缘情况也靠这一行接住：排队的消息被 host 接着跑（本地没调过
                // `send`）、以及一轮里多次 attempt 时上一段刚被 settle 成空闲。
                // 计时器只在空闲时补起点，不重置，否则秒数会从 0 重头跳。
                if self.turn_started.is_none() {
                    self.turn_started = Some(std::time::Instant::now());
                }
                self.phase = Phase::Running;
                self.fold.apply(&frame);
                self.settle_if_done();
            }
            UiEvent::Completions { seq, items } => {
                // 只认最后一次请求的结果，早到的丢掉。
                if let Some(completion) = self.completion.as_mut()
                    && seq == self.completion_seq
                {
                    completion.items = items;
                    // 不去猜用户想要哪一条：没有高亮，回车就等于发送。
                    completion.selected = None;
                }
            }
            UiEvent::Failed(message) => {
                tracing::warn!(%message, "链路失败");
                self.finish_turn();
                self.entries.push(Entry::Notice(message));
            }
        }
        cx.notify();
    }

    /// 补全开着且输入框在焦点上时接管按键。返回 `true` 表示已经消化掉。
    ///
    /// 键位：`Tab` 向下、`Shift+Tab` 向上、`Enter` 选中、`Esc` 收起；
    /// 方向键是同义快捷键。别的控件拿着焦点时一律不拦。
    fn completion_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.completion.is_none() {
            return false;
        }
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        let Some(completion) = self.completion.as_ref() else {
            return false;
        };
        let has_items = !completion.items.is_empty();
        match key {
            "escape" => {
                self.completion = None;
                cx.notify();
                true
            }
            "up" | "down" | "tab" if has_items => {
                let forward = if key == "tab" { !shift } else { key == "down" };
                let len = completion.items.len();
                let selected = completion.selected;
                if let Some(completion) = self.completion.as_mut() {
                    completion.selected = Some(match selected {
                        // 第一次按键才建立高亮：向下选第一条，向上选最后一条。
                        None if forward => 0,
                        None => len - 1,
                        Some(current) if forward => (current + 1) % len,
                        Some(current) => (current + len - 1) % len,
                    });
                }
                cx.notify();
                true
            }
            // 只有用户真用键盘选过一条，回车才是「插入候选」；否则放行给发送。
            "enter" if has_items && completion.selected.is_some() => {
                self.accept_completion(window, cx);
                true
            }
            // 没有候选时回车放行：正常发送草稿。
            _ => false,
        }
    }

    /// 接受当前高亮的候选，把 token 换成实打实的文本。
    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(completion) = self.completion.as_ref() else {
            return;
        };
        let Some(selected) = completion.selected else {
            return;
        };
        let Some(item) = completion.items.get(selected) else {
            return;
        };
        let insert = item.insert.clone();
        let active = completion::Active {
            kind: completion.kind,
            query: String::new(),
            start: completion.span.start,
            end: completion.span.end,
            quoted: completion.quoted,
        };
        let (next, caret) = completion::accept(&self.input.read(cx).value(), &active, &insert);
        self.completion = None;
        self.input.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            // `set_value` 会把光标丢到末尾；这里按插入点摆回去。
            state.set_selected_range(caret..caret, cx);
        });
        cx.notify();
    }

    /// 光标处的触发 token 变了就重新问一次候选；token 没了就收起浮层。
    fn refresh_completion(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let caret = self.input.read(cx).cursor();
        let Some(active) = completion::detect(&text, caret) else {
            if self.completion.take().is_some() {
                self.hover_completion = None;
                cx.notify();
            }
            return;
        };

        let keep_items = self
            .completion
            .as_ref()
            .is_some_and(|current| current.kind == active.kind);
        self.completion = Some(Completion {
            kind: active.kind,
            span: active.start..active.end,
            quoted: active.quoted,
            items: if keep_items {
                self.completion
                    .as_ref()
                    .map(|current| current.items.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            selected: None,
        });
        self.hover_completion = None;
        self.completion_seq += 1;
        let seq = self.completion_seq;
        let kind = active.kind;
        let query = active.query.clone();
        let commands = self.commands.clone();
        // 同类查询串没变就不重复跑 host 检索；改了则等一小会儿再说，
        // 免得每敲一个字都起一次 ripgrep。
        cx.spawn(async move |chat, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(COMPLETION_DEBOUNCE_MS))
                .await;
            let stale = chat
                .update(cx, |chat, _| chat.completion_seq != seq)
                .unwrap_or(true);
            if !stale {
                let _ = commands.try_send(UiCommand::Complete { kind, query, seq });
            }
        })
        .detach();
        cx.notify();
    }

    /// 流式尾巴的身份。
    ///
    /// `(turn, step)` 来自 `assistant-stream` 的 start 帧，与提交后的持久消息
    /// 一致——这就是「流式 → 已提交」不重建排版的原因。拿不到时用
    /// `local(0)` 占位：一屏只有一条活尾巴，稳定就够。
    fn live_id(&self) -> MessageId {
        match self.fold.turn_step() {
            Some((turn, step)) => MessageId::Turn { turn, step },
            None => MessageId::local(0),
        }
    }

    /// 终局到达后把流式文本落成条目，并清空折叠器。
    fn settle_if_done(&mut self) {
        let Some(outcome) = self.fold.outcome().cloned() else {
            return;
        };

        if let Some(usage) = self.fold.usage() {
            tracing::info!(
                input = usage.input_tokens,
                cache_read = usage.cache_read_tokens,
                cache_write = usage.cache_write_tokens,
                output = usage.output_tokens,
                total = usage.total_tokens,
                "usage 全字段"
            );
            // 上下文里真实存在的是「未缓存输入 + 命中缓存」，两者都算喂进去的量。
            self.meter = Some(CtxMeter {
                // 官方 contextTokens：未命中输入 + 缓存读 + 缓存写（三桶不重叠）。
                used: usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
            });
        }

        let text = self.fold.text();
        // 流式尾巴的身份：与提交后的持久消息同一个 `(turn, step)`，
        // 所以「流式 → 已提交」不换 key、不重排版。拿不到时用 local(0) 占位。
        let live_id = self.live_id();
        tracing::info!(
            ?outcome,
            chars = text.chars().count(),
            input_tokens = self.meter.map(|meter| meter.used),
            "回合完成"
        );

        match outcome {
            FoldOutcome::Committed { .. } => {
                if !text.is_empty() {
                    self.entries.push(Entry::Assistant { id: live_id, text });
                }
            }
            FoldOutcome::Interrupted => {
                if !text.is_empty() {
                    self.entries.push(Entry::Assistant { id: live_id, text });
                }
                self.entries.push(Entry::Notice("生成被中断".to_owned()));
            }
        }

        self.fold = AssistantStreamFold::new();
        self.finish_turn();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        tracing::debug!(chars = text.chars().count(), "提交输入");
        if text.is_empty() {
            return;
        }
        // 自动化验证时绝不真的投递：`DSH_DEBUG_NO_SEND` 显式声明，
        // `DSH_DEBUG_KEYS`（合成按键）自动进这个模式——那类跑法最容易误触回车。
        // 手动跑不设这两个变量，行为不变。
        if std::env::var("DSH_DEBUG_NO_SEND").is_ok() || std::env::var("DSH_DEBUG_KEYS").is_ok() {
            tracing::warn!(chars = text.chars().count(), "测试模式：拦下这次投递");
            return;
        }

        self.mode_open = false;
        self.model_open = false;
        self.effort_open = false;
        self.completion = None;
        self.entries.push(Entry::User {
            id: MessageId::local(self.local_seq),
            text: text.clone(),
        });
        self.local_seq += 1;
        self.phase = Phase::Sending;
        self.turn_started = Some(std::time::Instant::now());
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));

        if self.commands.try_send(UiCommand::Prompt(text)).is_err() {
            self.finish_turn();
            self.entries
                .push(Entry::Notice("没有连上 host，消息未发出".to_owned()));
        }

        cx.notify();
    }

    /// 停掉当前这一轮（`session/cancel`）：inbox 里的排队消息不动。
    fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Idle {
            return;
        }
        if self
            .commands
            .try_send(UiCommand::Cancel {
                session_id: self.active_session.clone(),
            })
            .is_err()
        {
            self.entries
                .push(Entry::Notice("没有连上 host，停不下来".to_owned()));
        } else {
            // 收尾还靠 `turn/end`（那一帧到了才真正回空闲），但先给一条回声，
            // 不然点了「停」界面上什么都没变。
            self.entries
                .push(Entry::Notice("已请求停止本轮".to_owned()));
        }
        cx.notify();
    }

    /// 一轮结束（完成/中断/失败）：状态回 Idle，计时停，顺便让侧栏重读一次运行态。
    fn finish_turn(&mut self) {
        self.phase = Phase::Idle;
        self.turn_started = None;
        let _ = self.commands.try_send(UiCommand::RefreshSessions);
    }

    // ── 侧栏 ────────────────────────────────────────────────────────────

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w(px(theme::SIDEBAR_W))
            .h_full()
            .bg(rgb(theme::BG_SIDEBAR))
            .child(self.side_top(cx))
            .child(self.side_body(cx))
            .child(self.side_foot())
    }

    fn side_top(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::BAR_H))
            .px(px(theme::SIDE_PAD))
            .gap_2()
            // 让位给系统红绿灯（窗口标题栏透明，灯由系统按设计稿坐标绘制）。
            .child(div().w(px(68.)).flex_none())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .child(crate::brand::lockup(15.0, theme::INK)),
            )
            .child(div().flex_1())
            .child(icon(IconName::Search, 14.0, theme::INK_MUTED))
            .child(self.side_close(cx))
    }

    /// 弹系统选目录框，选中后把目录交给外层注册成工作区。
    ///
    /// 官方是把选目录这一步交给 host（`directoryPicker/pick`，在 host 那台机器上
    /// 弹框）；本客户端与 host 同机，直接用自己的原生框，少一次往返。
    fn pick_workspace(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择要添加的工作区目录".into()),
        });
        let commands = self.commands.clone();
        cx.spawn(async move |_chat, _cx| {
            // 取消框、系统报错、没选中：三种情况都当作用户放弃。
            let picked = receiver
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            if let Some(path) = picked {
                let _ = commands.try_send(UiCommand::AddWorkspace {
                    path: path.to_string_lossy().into_owned(),
                });
            }
        })
        .detach();
    }

    /// 侧栏顶部的关闭按钮：收起左侧栏（展开入口在标题栏最左）。
    fn side_close(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hover_entity = entity.clone();
        let press_entity = entity;
        let tone = if self.hover_side_close {
            theme::INK
        } else {
            theme::INK_MUTED
        };

        div()
            .id("side-close")
            .cursor_pointer()
            .on_hover(move |hovered, _, cx| {
                hover_entity.update(cx, |chat, cx| {
                    if chat.hover_side_close != *hovered {
                        chat.hover_side_close = *hovered;
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press_entity.update(cx, |chat, cx| {
                    chat.sidebar_open = false;
                    cx.notify();
                });
            })
            .child(icon(IconName::PanelLeftClose, 14.0, tone))
    }

    /// 标题栏最左的展开按钮：只在左栏收起时出现。
    fn side_open(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hover_entity = entity.clone();
        let press_entity = entity;
        let tone = if self.hover_side_open {
            theme::INK
        } else {
            theme::INK_MUTED
        };

        div()
            .id("side-open")
            .cursor_pointer()
            .on_hover(move |hovered, _, cx| {
                hover_entity.update(cx, |chat, cx| {
                    if chat.hover_side_open != *hovered {
                        chat.hover_side_open = *hovered;
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press_entity.update(cx, |chat, cx| {
                    chat.sidebar_open = true;
                    cx.notify();
                });
            })
            .child(icon(IconName::PanelLeftOpen, 14.0, tone))
    }

    /// 侧栏正文：Workspaces 头 + 工作区树（官方 desktop 的行结构）。
    ///
    /// 结构对齐 `dsh-client-ui-workspace`：
    /// 工作区行 `[折叠箭头][名称][hover 新建]`，点行切换展开；
    /// 会话行 `[状态点][标题][相对时间]`，选中行高亮。
    fn side_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .id("side-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            // 会话多了要能滚：不然最下面的「已归档」区永远在画布外。
            .overflow_y_scroll()
            .px(px(theme::SIDE_PAD))
            .pt(px(theme::SIDE_PAD))
            .pb(px(theme::SIDE_PAD))
            .gap_1()
            .child(self.workspaces_header(cx));

        for group in self.workspace_groups() {
            body = body.child(self.workspace_row(
                &group.key,
                &group.label,
                group.workspace_id.as_deref(),
                !group.items.is_empty(),
                cx,
            ));
            if self.expanded.get(&group.key).copied().unwrap_or(true) {
                for session in group.items {
                    body = body.child(self.session_row(&group.key, session, cx));
                }
            }
        }

        // 归档区：默认收起，展开才列出那几条。归档 = 从常驻列表里拿走，记录仍在
        // （host 没有删会话的接口，只有 `archiveSession` / `unarchiveSession`）。
        let archived = self.archived_sessions();
        if !archived.is_empty() {
            let key = ARCHIVED_KEY.to_owned();
            let open = self.expanded.get(&key).copied().unwrap_or(false);
            body = body.child(self.archived_header(key.clone(), archived.len(), open, cx));
            if open {
                for session in archived {
                    body = body.child(self.archived_row(&key, session, cx));
                }
            }
        }

        body
    }

    /// 已归档的会话，最近的在上。
    fn archived_sessions(&self) -> Vec<&SessionInfo> {
        let mut items: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|session| self.registry.is_archived(&session.id))
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        items
    }

    /// 确认态自愈：被确认的那条已经不在列表/已归档（别处动过了）就收起来。
    fn settle_pending(&mut self) {
        if self.pending_delete.as_ref().is_some_and(|id| {
            !self
                .registry
                .items
                .iter()
                .any(|item| &item.workspace_id == id)
        }) {
            self.pending_delete = None;
        }
        if self
            .pending_archive
            .as_ref()
            .is_some_and(|id| self.registry.is_archived(id))
        {
            self.pending_archive = None;
        }
    }

    /// 侧栏分组：**已注册的工作区**（按注册表顺序）在前，剩下的会话归到「未分组」。
    ///
    /// 之前是按会话 `cwd` 现算分组，目录一旦注册就不受注册表影响——那样
    /// `workspace/delete` 删了也不会在界面上消失。现在分组的「有哪些组」由
    /// `workspace/follow` 决定，`cwd` 只用来把会话放进对应的组；
    /// 删掉注册后它的会话落到「未分组」，与 host 语义一致（目录与会话都还在）。
    fn workspace_groups(&self) -> Vec<SideGroup<'_>> {
        let mut visible: Vec<&SessionInfo> = self
            .sessions
            .iter()
            // 空白会话只留当前这一条（官方 `pinCurrentBlank` 的语义）：
            // 每次启动都会建一个新的空白会话，全列出来就是一片「新会话」。
            .filter(|session| !session.blank || session.id == self.active_session)
            // 归档的不进树：它们在下面的「已归档」里。
            .filter(|session| !self.registry.is_archived(&session.id))
            .collect();

        let mut groups: Vec<SideGroup<'_>> = Vec::new();
        for workspace in &self.registry.items {
            let (mine, rest): (Vec<&SessionInfo>, Vec<&SessionInfo>) = visible
                .into_iter()
                .partition(|session| same_path(&session.cwd, &workspace.path));
            visible = rest;
            let mut items = mine;
            items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
            groups.push(SideGroup {
                key: workspace.path.clone(),
                label: workspace.title.clone(),
                workspace_id: Some(workspace.workspace_id.clone()),
                items,
            });
        }

        if !visible.is_empty() {
            visible.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
            groups.push(SideGroup {
                key: "\u{0}未分组".to_owned(),
                label: "未分组".to_owned(),
                workspace_id: None,
                items: visible,
            });
        }
        groups
    }

    /// 「Workspaces」标题行：右侧加号在当前工作区新建会话。
    fn workspaces_header(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let press = _cx.entity();

        div()
            .flex()
            .flex_row()
            .items_center()
            .pb(px(2.))
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child("Workspaces")
            .child(div().flex_1())
            .child(
                div()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        // 这个 + 是「添加目录」，不是「在当前工作区建会话」。
                        press.update(cx, |chat, cx| chat.pick_workspace(cx));
                    })
                    .child(icon(IconName::Plus, 12.0, theme::INK_MUTED)),
            )
    }

    /// 工作区行。
    ///
    /// `workspace_id` 只有已注册的工作区才有；「未分组」传 `None`，
    /// 于是它既不能新建会话（没有注册就没有固定 cwd），也不能被删。
    fn workspace_row(
        &self,
        cwd: &str,
        label: &str,
        workspace_id: Option<&str>,
        has_sessions: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.expanded.get(cwd).copied().unwrap_or(true);
        let key = cwd.to_owned();
        let entity = cx.entity();
        let toggle = entity.clone();
        let create = entity.clone();
        let remove = entity.clone();
        let create_cwd = cwd.to_owned();
        let remove_id = workspace_id.map(|id| id.to_owned());
        let arrow = if expanded || !has_sessions {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

        let mut row = div()
            .group("workspace-row")
            .id(ElementId::Name(format!("workspace:{cwd}").into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                toggle.update(cx, |chat, cx| {
                    chat.pending_delete = None;
                    let next = !chat.expanded.get(&key).copied().unwrap_or(true);
                    chat.expanded.insert(key.clone(), next);
                    cx.notify();
                });
            })
            .child(icon(arrow, 12.0, theme::INK_MUTED))
            .child(
                div()
                    .truncate()
                    .text_size(px(theme::UI))
                    .text_color(rgb(theme::INK))
                    .child(label.to_owned()),
            )
            .child(div().flex_1());

        if let Some(workspace_id) = remove_id {
            if self.pending_delete.as_deref() == Some(workspace_id.as_str()) {
                // 二次确认：就地把两枚图标换成「取消 / 移除」，不进对话框。
                // 删除只动注册（目录、会话都在），所以确认文案不必吓人。
                row = row.child(self.confirm_bar(
                    "ws-delete",
                    &workspace_id,
                    ConfirmKind::WorkspaceDelete,
                    cx,
                ));
            } else {
                // hover 才出现的两枚小图标：新建会话 / 移除注册。
                row = row
                    .child(
                        div()
                            .cursor_pointer()
                            .opacity(0.)
                            .group_hover("workspace-row", |style| style.opacity(1.))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                create.update(cx, |chat, cx| {
                                    let _ = chat.commands.try_send(UiCommand::NewSession {
                                        cwd: create_cwd.clone(),
                                    });
                                    cx.notify();
                                });
                                // 不让点图标同时把行展开/收起。
                                cx.stop_propagation();
                            })
                            .child(icon(IconName::Plus, 12.0, theme::INK_MUTED)),
                    )
                    .child(
                        div()
                            .cursor_pointer()
                            .opacity(0.)
                            .group_hover("workspace-row", |style| style.opacity(1.))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let id = workspace_id.clone();
                                remove.update(cx, |chat, cx| {
                                    // 第一下只进确认态。
                                    chat.pending_delete = Some(id);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            })
                            .child(icon(IconName::Delete, 12.0, theme::INK_MUTED)),
                    );
            }
        }

        row
    }

    /// 行内二次确认的两枚胶囊：「取消 / 动作」。
    ///
    /// 两处共用：移除工作区注册（只删注册）、归档一条**还在跑**的会话（归档会
    /// 顺手把它停掉，所以也要问一句）。不开弹层、行高不变，退路径就是点行。
    fn confirm_bar(
        &self,
        prefix: &str,
        id: &str,
        kind: ConfirmKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let cancel = entity.clone();
        let confirm = entity;
        let cancel_id = id.to_owned();
        let confirm_id = id.to_owned();

        let chip = |name: String, label: &'static str, tone: u32| {
            div()
                .id(ElementId::Name(name.into()))
                .flex()
                .items_center()
                .h(px(20.))
                .px(px(6.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(theme::META))
                .text_color(rgb(tone))
                .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                .child(label)
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                chip(format!("{prefix}-cancel"), "取消", theme::INK_MUTED).on_mouse_down(
                    MouseButton::Left,
                    move |_, _, cx| {
                        let id = cancel_id.clone();
                        cancel.update(cx, |chat, cx| {
                            chat.clear_pending(kind, &id);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    },
                ),
            )
            .child(
                chip(format!("{prefix}-confirm"), kind.verb(), theme::INK).on_mouse_down(
                    MouseButton::Left,
                    move |_, _, cx| {
                        let id = confirm_id.clone();
                        confirm.update(cx, |chat, cx| {
                            chat.confirm_pending(kind, id);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    },
                ),
            )
    }

    /// 收起某一类确认态。
    fn clear_pending(&mut self, kind: ConfirmKind, id: &str) {
        match kind {
            ConfirmKind::WorkspaceDelete => {
                if self.pending_delete.as_deref() == Some(id) {
                    self.pending_delete = None;
                }
            }
            ConfirmKind::SessionArchive => {
                if self.pending_archive.as_deref() == Some(id) {
                    self.pending_archive = None;
                }
            }
        }
    }

    /// 确认时真正动手：先乐观改本地状态（侧栏立刻收起），再把命令推出去。
    fn confirm_pending(&mut self, kind: ConfirmKind, id: String) {
        match kind {
            ConfirmKind::WorkspaceDelete => {
                self.registry.items.retain(|item| item.workspace_id != id);
                self.pending_delete = None;
                let _ = self
                    .commands
                    .try_send(UiCommand::DeleteWorkspace { workspace_id: id });
            }
            ConfirmKind::SessionArchive => {
                self.archive_session(id);
            }
        }
    }

    /// 归档一条会话（乐观）：收进「已归档」，不删记录也不删目录。
    fn archive_session(&mut self, id: String) {
        if !self.registry.is_archived(&id) {
            self.registry.archived_session_ids.push(id.clone());
        }
        self.pending_archive = None;
        let _ = self.commands.try_send(UiCommand::ArchiveSession {
            session_id: id,
            // 走到这里已经确认过了：归档一条在跑的会话就是要停掉它。
            stop_activity: true,
        });
    }

    /// 取消归档（乐观）：先回常驻列表，再发命令。
    fn unarchive_session(&mut self, id: String) {
        self.registry
            .archived_session_ids
            .retain(|item| item != &id);
        let _ = self
            .commands
            .try_send(UiCommand::UnarchiveSession { session_id: id });
    }

    /// 仅调试：等价于点会话行尾的「归档」，走同一条命令通道。
    #[doc(hidden)]
    pub fn debug_archive(&mut self, session_id: String) {
        self.archive_session(session_id);
    }

    /// 仅调试：等价于点 composer 右下角的「停」。
    #[doc(hidden)]
    pub fn debug_cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_turn(cx);
    }

    /// 仅调试：等价于点归档行尾的「取消归档」。
    #[doc(hidden)]
    pub fn debug_unarchive(&mut self, session_id: String) {
        self.unarchive_session(session_id);
    }

    /// 会话行。
    fn session_row(
        &self,
        _cwd: &str,
        session: &SessionInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = session.id == self.active_session;
        let title = session.title.clone().unwrap_or_else(|| {
            if session.blank {
                "新会话"
            } else {
                short_id(&session.id)
            }
            .to_owned()
        });
        let time = relative_time(session.updated_at);
        let id = session.id.clone();
        let entity = cx.entity();
        let archiving = self.pending_archive.as_deref() == Some(session.id.as_str());
        let running = session.running;
        let archive = entity.clone();
        let archive_id = session.id.clone();

        let row = div()
            .id(ElementId::Name(format!("session:{}", session.id).into()))
            .group("session-row")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl(px(24.))
            .pr(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    chat.pending_archive = None;
                    if chat.active_session == id {
                        return;
                    }
                    if chat
                        .commands
                        .try_send(UiCommand::OpenSession(id.clone()))
                        .is_ok()
                    {
                        chat.active_session = id.clone();
                        chat.finish_turn();
                        chat.fold = AssistantStreamFold::new();
                        cx.notify();
                    }
                });
            });

        let row = if selected {
            row.bg(rgb(theme::BG_ACTIVE))
        } else {
            row.hover(|style| style.bg(rgb(theme::BG_HOVER)))
        };

        let busy = session.running || (session.id == self.active_session && self.phase.is_busy());
        row.child(if busy {
            dot(5.0, theme::OK)
        } else {
            div().w(px(5.))
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(px(theme::ROW))
                .text_color(rgb(if selected {
                    theme::INK
                } else {
                    theme::INK_SOFT
                }))
                .child(title),
        )
        .child(if archiving {
            // 归档一条在跑的会话 = 先停下它，所以这里也要确认。
            self.confirm_bar(
                "session-archive",
                &archive_id,
                ConfirmKind::SessionArchive,
                cx,
            )
            .into_any_element()
        } else {
            div()
                .cursor_pointer()
                .opacity(0.)
                .group_hover("session-row", |style| style.opacity(1.))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let id = archive_id.clone();
                    archive.update(cx, |chat, cx| {
                        if running {
                            // 跑着的不直接归档：先问一句（归档会把这一轮停掉）。
                            chat.pending_archive = Some(id);
                        } else {
                            chat.archive_session(id);
                        }
                        cx.notify();
                    });
                    cx.stop_propagation();
                })
                .child(icon(IconName::Inbox, 12.0, theme::INK_MUTED))
                .into_any_element()
        })
        .child(
            div()
                .text_size(px(theme::META))
                .text_color(rgb(theme::INK_MUTED))
                .child(time),
        )
    }

    /// 「已归档 N」标题行，默认收起。
    fn archived_header(
        &self,
        key: String,
        count: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let arrow = if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

        div()
            .id(ElementId::Name("archived:header".into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mt(px(6.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    let next = !chat.expanded.get(&key).copied().unwrap_or(false);
                    chat.expanded.insert(key.clone(), next);
                    cx.notify();
                });
            })
            .child(icon(arrow, 12.0, theme::INK_MUTED))
            .child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(format!("已归档 {count}")),
            )
            .child(div().flex_1())
    }

    /// 归档区里的一行：标题 + hover 才出现「取消归档」。
    fn archived_row(
        &self,
        _key: &str,
        session: &SessionInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = session
            .title
            .clone()
            .unwrap_or_else(|| short_id(&session.id).to_owned());
        let id = session.id.clone();
        let entity = cx.entity();
        let open = entity.clone();
        let open_id = id.clone();

        div()
            .id(ElementId::Name(format!("archived:{}", session.id).into()))
            .group("archived-row")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl(px(24.))
            .pr(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                open.update(cx, |chat, cx| {
                    if chat.active_session == open_id {
                        return;
                    }
                    if chat
                        .commands
                        .try_send(UiCommand::OpenSession(open_id.clone()))
                        .is_ok()
                    {
                        chat.active_session = open_id.clone();
                        chat.finish_turn();
                        chat.fold = AssistantStreamFold::new();
                        cx.notify();
                    }
                });
            })
            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(theme::ROW))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(title),
            )
            .child(
                div()
                    .opacity(0.)
                    .cursor_pointer()
                    .group_hover("archived-row", |style| style.opacity(1.))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let id = id.clone();
                        entity.update(cx, |chat, cx| {
                            chat.unarchive_session(id);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    })
                    .child(icon(IconName::Undo2, 12.0, theme::INK_MUTED)),
            )
    }

    fn side_foot(&self) -> impl IntoElement {
        let (tone, label) = if self.sleeping {
            (theme::INK_MUTED, "host 休眠中")
        } else if self.connected {
            (theme::OK, "dsh host")
        } else {
            (theme::INK_MUTED, "未连接")
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(44.))
            .px(px(theme::SIDE_PAD))
            .gap_2()
            .child(dot(5.0, tone))
            .child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(label),
            )
            .child(div().flex_1())
            .child(icon(IconName::Settings, 14.0, theme::INK_MUTED))
    }

    // ── 主区 ────────────────────────────────────────────────────────────

    fn main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 还没选工作区 / 没有会话：主区是空状态，不是聊天区。
        let body = if self.active_session.is_empty() {
            self.empty_state(cx).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(self.transcript(cx))
                .child(self.composer(cx))
                .into_any_element()
        };

        let column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(self.title_bar(cx))
            .child(body);

        let mut main = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w(px(0.))
            .bg(rgb(theme::BG_MAIN))
            .child(column);

        if self.right_panel_open && !self.active_session.is_empty() {
            main = main.child(self.right_panel());
        }

        main
    }

    /// 没有会话时的主区：像官方 desktop 的空状态，直接列出已注册的工作区。
    ///
    /// 选一个工作区就在那个目录下建会话；没有工作区就只留「添加工作区…」。
    /// 这样新会话不会落进「未分组」，cwd 也不再由启动目录 / 家目录决定。
    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let mut list = div()
            .flex()
            .flex_col()
            .w(px(440.))
            .p(px(4.))
            .gap_1()
            .rounded(px(10.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE));

        for workspace in &self.registry.items {
            let path = workspace.path.clone();
            let press = entity.clone();
            let tone = theme::INK_SOFT;
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!("hero-ws:{}", workspace.workspace_id).into(),
                    ))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        press.update(cx, |chat, cx| {
                            let _ = chat
                                .commands
                                .try_send(UiCommand::NewSession { cwd: path.clone() });
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(theme::BODY))
                            .text_color(rgb(tone))
                            .child(leaf(&workspace.path)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(220.))
                            .truncate()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child(tilde(&workspace.path)),
                    ),
            );
        }

        let add = entity.clone();
        list = list.child(
            div()
                .id("hero-add")
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    add.update(cx, |chat, cx| chat.pick_workspace(cx));
                })
                .child(
                    div()
                        .flex_1()
                        .text_size(px(theme::BODY))
                        .text_color(rgb(theme::INK_SOFT))
                        .child("添加工作区…"),
                ),
        );

        let hint = if self.registry.items.is_empty() {
            "添加一个目录开始"
        } else {
            "选一个工作区开始新会话"
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_6()
            .child(crate::brand::lockup(20.0, theme::INK))
            .child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(hint),
            )
            .child(list)
    }

    /// 右侧面板：会话元信息。没有来源的读数一律显示 `—`（DECISIONS #21）。
    fn right_panel(&self) -> impl IntoElement {
        let ctx = match self.occupancy {
            Some(occupancy) => format!(
                "{} / {} · {}%",
                format_tokens(occupancy.used),
                format_tokens(occupancy.window),
                occupancy.percent
            ),
            None => match &self.meter {
                Some(meter) => format!("{} / —", format_tokens(meter.used)),
                None => "—".to_owned(),
            },
        };
        let cache = match self.cache_hit {
            Some(hit) => format!("{hit}%"),
            None => "—".to_owned(),
        };

        div()
            .flex()
            .flex_col()
            .w(px(theme::PANEL_W))
            .h_full()
            .bg(rgb(theme::BG_SIDEBAR))
            .border_l_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(theme::BAR_H))
                    .px(px(theme::SIDE_PAD))
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child("上下文"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px(px(theme::SIDE_PAD))
                    .child(meta_field("工作区", &self.path))
                    .child(meta_field("会话", &self.title))
                    .child(meta_field(
                        "model",
                        self.effective_model()
                            .and_then(|selected| {
                                self.model_name(&selected.provider, &selected.model)
                            })
                            .unwrap_or("—"),
                    ))
                    .child(meta_field("ctx", &ctx))
                    .child(meta_field("cache", &cache)),
            )
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 左栏收起后主区顶到窗口左边，标题栏就得自己让开系统红绿灯。
        let left = if self.sidebar_open {
            theme::GUTTER
        } else {
            96.0
        };
        // 标题行的宽度与时间线/输入框的 `CONTENT_W` 列一致，居中放：
        // 侧栏打开时三者左沿完全重合，标题不再贴回到 `GUTTER`。
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .max_w(px(theme::CONTENT_W))
            .gap_2();
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::BAR_H))
            .pl(px(left))
            .pr(px(theme::GUTTER));
        // 只在侧栏打开时居中：收起后左边要留给红绿灯和展开图标，
        // 硬居中会把那个图标孤零零地推到中间。
        let bar = if self.sidebar_open {
            bar.justify_center()
        } else {
            bar
        };

        if !self.sidebar_open {
            row = row.child(self.side_open(cx)).child(div().w(px(6.)));
        }

        // 还没有会话（空状态）：标题栏不摆「未连接 / 新会话」，也不给无处的右栏开关。
        if self.active_session.is_empty() {
            return bar.child(row.child(div().flex_1()));
        }

        bar.child(
            row.child(
                div()
                    .text_size(px(theme::UI))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(self.workspace.clone()),
            )
            .child(
                div()
                    .text_size(px(theme::UI))
                    .text_color(rgb(theme::INK_MUTED))
                    .child("/"),
            )
            .child(div().text_size(px(theme::UI)).child(self.title.clone()))
            .child(div().flex_1())
            .child(self.panel_toggle(cx)),
        )
    }

    fn transcript(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(theme::CONTENT_W))
            .gap_3();

        // 过程组是**渲染期派生**的，不存进模型：连续的工具行归一组，
        // 中间只要有一句助手正文就断开。切模式只是换一种画法，数据不重复。
        let mut index = 0;
        for run in tool_runs(&self.entries) {
            while index < run.start {
                column = column.child(render_entry(&self.entries[index]));
                index += 1;
            }
            column = column.child(self.tool_group(&self.entries[run.clone()], cx));
            index = run.end;
        }
        while index < self.entries.len() {
            column = column.child(render_entry(&self.entries[index]));
            index += 1;
        }

        let live = self.fold.text();
        if !live.is_empty() {
            column = column.child(crate::markdown::text(self.live_id(), &live));
        }
        if let Some(status) = self.running_status() {
            column = column.child(status);
        }

        div().id("transcript").flex_1().overflow_y_scroll().child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .px(px(theme::GUTTER))
                .py(px(theme::GUTTER))
                .child(column),
        )
    }

    /// 一段连续的工具调用：组头（可点开合）+ 可选的行列表。
    fn tool_group(&self, calls: &[Entry], cx: &mut Context<Self>) -> Div {
        let mut tools: Vec<&ToolCall> = Vec::new();
        for entry in calls {
            if let Entry::Tool(call) = entry {
                tools.push(call);
            }
        }
        let Some(first) = tools.first() else {
            return div();
        };
        let key = first.id.clone();
        let running_turn = self.phase.is_busy();
        let open = self
            .open_groups
            .get(&key)
            .copied()
            .unwrap_or_else(|| self.view.group_body_open(running_turn))
            || self.debug_tools_open;

        let entity = cx.entity();
        let toggle = entity.clone();
        let header = div()
            .id("group-header")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .py(px(2.))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                toggle.update(cx, |chat, cx| {
                    let next = !chat
                        .open_groups
                        .get(&key)
                        .copied()
                        .unwrap_or_else(|| chat.view.group_body_open(chat.phase.is_busy()));
                    chat.open_groups.insert(key.clone(), next);
                    cx.notify();
                });
            })
            .child(icon(
                if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                },
                12.0,
                theme::INK_MUTED,
            ))
            .child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(group_label(self.view, &tools, running_turn)),
            )
            .child(div().flex_1());

        // 右侧：当前模式，点一下换下一档（还没有设置面板，就近放这里）。
        let switch = entity.clone();
        let mode = TranscriptView::label(self.view);
        let header = header.child(
            div()
                .id("group-view")
                .text_size(px(theme::META))
                .text_color(rgb(theme::INK_MUTED))
                .cursor_pointer()
                .hover(|style| style.text_color(rgb(theme::INK_SOFT)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    switch.update(cx, |chat, cx| {
                        chat.view = chat.view.next();
                        cx.notify();
                    });
                })
                .child(mode),
        );

        let mut group = div().flex().flex_col().gap_1().child(header);
        if open {
            let mut rows = div().flex().flex_col().gap_1().pl(px(16.));
            for call in &tools {
                rows = rows.child(self.tool_row(call, cx));
            }
            group = group.child(rows);
        }
        group
    }

    /// 一行工具调用：图标 + 名字 + 摘要 + 后缀；点开看正文。
    fn tool_row(&self, call: &ToolCall, cx: &mut Context<Self>) -> Div {
        // 调试开关把正文全展开：合成点击在本机只有一半能落上屏，
        // 验收排版时用它代替手点（`DSH_DEBUG_TOOL_OPEN=1`）。
        let open = self.debug_tools_open || self.open_tools.contains(&call.id);
        let entity = cx.entity();
        let id = call.id.clone();
        let stroke = match call.status {
            ToolStatus::Running => theme::INK_SOFT,
            ToolStatus::Ok => theme::INK_MUTED,
            // 失败不靠颜色：fx-ui chrome 零彩色，用字色 + 后缀「失败」表达。
            ToolStatus::Error => theme::INK,
        };

        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    if !chat.open_tools.remove(&id) {
                        chat.open_tools.insert(id.clone());
                    }
                    cx.notify();
                });
            })
            .child(icon(tool_icon(&call.name), 13.0, theme::INK_MUTED))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(call.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(px(theme::META))
                    .text_color(rgb(stroke))
                    .child(call.summary.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(match call.status {
                        ToolStatus::Error => theme::INK,
                        _ => theme::INK_MUTED,
                    }))
                    .child(tool_suffix(call)),
            );

        if !open || call.detail.is_empty() {
            return row;
        }

        // 输出可能上千行：正文之外再限一次行数，免得展开一下就把窗口摸掉。
        const MAX_LINES: usize = 24;
        let lines: Vec<&str> = call.detail.lines().collect();
        let shown = if lines.len() > MAX_LINES {
            format!(
                "{}\n…（共 {} 行，只显示前 {MAX_LINES} 行）",
                lines[..MAX_LINES].join("\n"),
                lines.len()
            )
        } else {
            call.detail.clone()
        };

        div().flex().flex_col().gap_1().child(row).child(
            div()
                .id(ElementId::Name(format!("tool-detail:{}", call.id).into()))
                .pl(px(21.))
                .pr(px(8.))
                .py(px(6.))
                .text_size(px(theme::META))
                .text_color(rgb(theme::INK_MUTED))
                .child(shown),
        )
    }

    /// 时间线尾巴的「正在进行」行：官方是鲸尾 + 流光文字 + 已用时长，
    /// 我们只用字色与一个小点（fx-ui 的 chrome 零彩色），信息密度不变。
    fn running_status(&self) -> Option<Div> {
        let label = match self.phase {
            Phase::Idle => return None,
            Phase::Sending => "已送到 host…".to_owned(),
            Phase::Running => {
                let secs = self
                    .turn_started
                    .map(|started| started.elapsed().as_secs())
                    .unwrap_or(0);
                format!("正在思考 · {secs}s")
            }
        };
        let tone = if self.phase == Phase::Running {
            theme::OK
        } else {
            theme::INK_MUTED
        };

        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(dot(5.0, tone))
                .child(
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_MUTED))
                        .child(label),
                ),
        )
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .px(px(theme::GUTTER))
            .py(px(theme::GUTTER))
            .gap_1()
            .child(self.completion_layer(cx))
            .child(self.composer_card(cx))
            .child(self.chips(cx))
    }

    /// 补全浮层：零高度锚点挂在卡片正上方，弹出时不挤动时间线。
    ///
    /// 键盘走 `intercept_keystrokes`（见 `completion_key`）；鼠标点击候选行
    /// 由行自己接（`on_mouse_down`），面板先于卡片绘制，命中优先级更高。
    fn completion_layer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let anchor = div()
            .relative()
            .w_full()
            .max_w(px(theme::CONTENT_W))
            .h(px(0.));

        let Some(completion) = self.completion.as_ref() else {
            return anchor;
        };

        let capture = anchor;
        let entity = cx.entity();

        let is_file = completion.kind == completion::Kind::File;
        let rows = completion.items.iter().take(COMPLETION_ROWS);
        let mut menu = div()
            .absolute()
            .bottom(px(6.))
            .left(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .p(px(4.))
            .rounded(px(8.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .shadow_lg()
            .text_size(px(theme::UI));
        let mut any = false;
        for (index, item) in rows.enumerate() {
            any = true;
            let selected = completion.selected == Some(index);
            let hovered = self.hover_completion == Some(index);
            let mut row = div()
                .id(("completion-row", index))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .h(px(26.))
                .px(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .on_hover({
                    let entity = entity.clone();
                    move |hovered, _, cx| {
                        entity.update(cx, |chat, cx| {
                            let next = if *hovered { Some(index) } else { None };
                            if chat.hover_completion != next {
                                chat.hover_completion = next;
                                cx.notify();
                            }
                        });
                    }
                })
                .on_mouse_down(MouseButton::Left, {
                    let entity = entity.clone();
                    move |_, window, cx| {
                        entity.update(cx, |chat, cx| {
                            if let Some(completion) = chat.completion.as_mut() {
                                completion.selected = Some(index);
                            }
                            chat.accept_completion(window, cx);
                        });
                    }
                });
            if selected || hovered {
                row = row.bg(rgb(theme::BG_HOVER));
            }
            menu = menu.child(
                row.child(label_cell(item, selected, is_file)).child(
                    div()
                        .flex_none()
                        .text_color(rgb(theme::INK_MUTED))
                        .child(item.hint.clone()),
                ),
            );
        }
        if !any {
            menu = menu.child(
                div()
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .text_color(rgb(theme::INK_MUTED))
                    .child(if completion.kind == completion::Kind::Command {
                        "没有匹配的命令"
                    } else {
                        "没有匹配的文件"
                    }),
            );
        }

        capture.child(menu)
    }

    /// composer 卡片：可点击（聚焦输入框），hover 时描边提亮。
    ///
    fn composer_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hover_entity = entity.clone();
        let press_entity = entity;

        let edge = if self.hover_composer {
            theme::INK_MUTED
        } else {
            theme::BG_CARD_EDGE
        };

        div()
            .w_full()
            .max_w(px(theme::CONTENT_W))
            .rounded(px(8.))
            .bg(rgb(edge))
            .p(px(1.))
            .id("composer-card")
            .cursor_pointer()
            .on_hover(move |hovered, _, cx| {
                hover_entity.update(cx, |chat, cx| {
                    if chat.hover_composer != *hovered {
                        chat.hover_composer = *hovered;
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                press_entity.update(cx, |chat, cx| {
                    let input = chat.input.clone();
                    input.update(cx, |state, cx| state.focus(window, cx));
                });
            })
            .child(
                div()
                    .rounded(px(7.))
                    .bg(rgb(theme::BG_CARD))
                    .p(px(12.))
                    .child(
                        div()
                            .w_full()
                            .text_size(px(theme::BODY))
                            .child(Textarea::new(&self.input)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .pt(px(6.))
                            .text_size(px(theme::META))
                            // 左端：权限 mode 下拉（从 statusline 挪进来的，
                            // 与上方输入文字同一条左沿）。
                            .child(self.mode_chip(cx))
                            .child(div().flex_1())
                            .child(self.primary_button(cx)),
                    ),
            )
    }

    /// composer 右下那个唯一的主按钮：**它就是回车会做的事**。
    ///
    /// 官方 InputBar 的口径：草稿空且会话在跑时它是 Stop，否则是 Send；
    /// 跑着且手里有草稿时两个都在（Send 排队、Stop 单留）。
    fn primary_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let draft_empty = self.input.read(cx).value().trim().is_empty();
        let busy = self.phase.is_busy();
        let entity = cx.entity();
        let send = entity.clone();
        let stop = entity.clone();

        let mut row = div().flex().flex_row().items_center().gap_1();
        if busy {
            row = row.child(round_button(
                "composer-stop",
                IconName::Square,
                theme::INK,
                move |_, _, cx| {
                    stop.update(cx, |chat, cx| chat.cancel_turn(cx));
                    cx.stop_propagation();
                },
            ));
        }
        if !busy || !draft_empty {
            let tone = if draft_empty {
                theme::INK_MUTED
            } else {
                theme::INK
            };
            row = row.child(round_button(
                "composer-send",
                IconName::ArrowUp,
                tone,
                move |_, window, cx| {
                    send.update(cx, |chat, cx| chat.send(window, cx));
                    cx.stop_propagation();
                },
            ));
        }
        row
    }

    /// 标题栏右侧的侧栏开关。
    fn panel_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hover_entity = entity.clone();
        let press_entity = entity;

        let tone = if self.hover_panel {
            theme::INK
        } else {
            theme::INK_MUTED
        };

        div()
            .cursor_pointer()
            .text_size(px(theme::UI))
            .text_color(rgb(tone))
            .id("panel-toggle")
            .on_hover(move |hovered, _, cx| {
                hover_entity.update(cx, |chat, cx| {
                    if chat.hover_panel != *hovered {
                        chat.hover_panel = *hovered;
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press_entity.update(cx, |chat, cx| {
                    chat.right_panel_open = !chat.right_panel_open;
                    cx.notify();
                });
            })
            .child(icon(IconName::PanelRight, 14.0, tone))
    }

    /// 一个下拉触发器：标签 + 箭头，hover 或展开时提亮。
    ///
    /// mode、model、effort 三个控件只有标签和状态不同，其余完全一致。
    fn chip(&self, cx: &mut Context<Self>, spec: ChipSpec) -> impl IntoElement + use<> {
        let ChipSpec {
            id,
            label,
            open,
            hovered,
            on_hover,
            on_press,
        } = spec;
        let tone = if hovered || open {
            theme::INK
        } else {
            theme::INK_SOFT
        };

        let entity = cx.entity();
        let hover_entity = entity.clone();
        let press_entity = entity;

        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .max_w(px(200.))
            .cursor_pointer()
            .text_color(rgb(tone))
            .on_hover(move |hovering, _, cx| {
                let hovering = *hovering;
                hover_entity.update(cx, |chat, cx| {
                    on_hover(chat, hovering);
                    cx.notify();
                });
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                press_entity.update(cx, |chat, cx| {
                    on_press(chat);
                    cx.notify();
                });
            })
            .child(div().min_w(px(0.)).truncate().child(label))
            .child(if open {
                icon(IconName::ChevronUp, 12.0, tone)
            } else {
                icon(IconName::ChevronDown, 12.0, tone)
            })
    }

    /// 上拉浮层：套在触发器外面，向上展开，点别处收起。
    ///
    /// `close` / `pick` 都是普通函数指针——每一行的 key 就是它的语义入口，
    /// 省掉一堆闭包捕获。
    fn pull_menu<Trigger>(
        &self,
        cx: &mut Context<Self>,
        spec: PullSpec,
        trigger: Trigger,
        close: fn(&mut Self),
        pick: fn(&mut Self, &str),
    ) -> impl IntoElement + use<Trigger>
    where
        Trigger: IntoElement,
    {
        let PullSpec {
            id,
            open,
            width,
            rows,
        } = spec;
        let mut cell = div().id(id).relative().child(trigger);
        if !open {
            return cell;
        }

        // 点别处收起：列表在 capture 阶段对窗口内每次按下判定一次，
        // 命中框（触发器）之外的按下都算「点外面」。触发器本身在框内，
        // 所以不会和它的开合逻辑打架。
        let dismiss = cx.entity();
        cell = cell.on_mouse_down_out(move |_, _, cx| {
            dismiss.update(cx, |chat, cx| {
                close(chat);
                cx.notify();
            });
        });

        let mut menu = div()
            .absolute()
            .bottom(px(26.))
            .left(px(0.))
            .w(px(width))
            .flex()
            .flex_col()
            .p(px(4.))
            .gap_1()
            .rounded(px(8.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .shadow_lg();

        for row in rows {
            let entity = cx.entity();
            let key = row.key.clone();
            let muted = row.muted;
            let mut line = div()
                .id(ElementId::Name(format!("{id}:{}", row.key).into()))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px(px(8.))
                .py(px(5.))
                .rounded(px(5.));
            if !muted {
                line = line
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        entity.update(cx, |chat, cx| {
                            pick(chat, &key);
                            cx.notify();
                        });
                    });
            }
            let tone = if muted {
                theme::INK_MUTED
            } else if row.selected {
                theme::INK
            } else {
                theme::INK_SOFT
            };
            let mut line = line.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(theme::ROW))
                    .text_color(rgb(tone))
                    .child(row.label),
            );

            if let Some(hint) = row.hint {
                line = line.child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(theme::INK_MUTED))
                        .child(hint),
                );
            }

            menu = menu.child(line.child(if row.selected && !muted {
                icon(IconName::Check, 12.0, theme::INK).into_any_element()
            } else {
                div().w(px(12.)).into_any_element()
            }));
        }

        cell.child(menu)
    }

    fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        fn hover(chat: &mut Chat, hovering: bool) {
            chat.hover_mode = hovering;
        }
        fn press(chat: &mut Chat) {
            chat.mode_open = !chat.mode_open && !chat.mode_options.is_empty();
        }
        fn close(chat: &mut Chat) {
            chat.mode_open = false;
        }
        fn pick(chat: &mut Chat, key: &str) {
            if chat
                .commands
                .try_send(UiCommand::SetMode(key.to_owned()))
                .is_ok()
            {
                // 先落成选中态，host 的投影回来后会覆盖成权威值。
                chat.mode = key.to_owned();
            }
        }

        let label = if self.mode.is_empty() {
            "—".to_owned()
        } else {
            self.mode.clone()
        };
        let rows = self
            .mode_options
            .iter()
            .map(|option| PullRow {
                key: option.value.clone(),
                label: option.name.clone(),
                hint: None,
                selected: option.value == self.mode,
                muted: false,
            })
            .collect();

        let trigger = self.chip(
            cx,
            ChipSpec {
                id: "mode-chip",
                label,
                open: self.mode_open,
                hovered: self.hover_mode,
                on_hover: hover,
                on_press: press,
            },
        );

        self.pull_menu(
            cx,
            PullSpec {
                id: "mode-cell",
                open: self.mode_open,
                width: 232.,
                rows,
            },
            trigger,
            close,
            pick,
        )
    }

    /// 状态行左侧的模型名。选中项来自投影，未选过时退回 host 的默认选择。
    ///
    /// 文案对齐官方 `dsh-client-ui-model-selection`：当前选择不在目录里（含从未选过、
    /// 或模型被删）时显示「请选择模型」；目录空时菜单里显示「暂无可用模型」，
    /// provider 列举失败逐条显示官方格式的警告。**发送不被目录可用性拦住**——
    /// 官方口径是「请求执行负责报告凭据缺失或模型不可用」，与 #37 一致。
    fn model_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        fn hover(chat: &mut Chat, hovering: bool) {
            chat.hover_model = hovering;
        }
        fn press(chat: &mut Chat) {
            // 目录空也允许展开：官方在菜单里给「暂无可用模型」，不透的按钮等于没反馈。
            chat.model_open = !chat.model_open;
        }
        fn close(chat: &mut Chat) {
            chat.model_open = false;
        }
        fn pick(chat: &mut Chat, key: &str) {
            let Some((provider, model)) = key.split_once('/') else {
                return;
            };
            // 换模型时强度取该模型的默认档（官方「弹窗应用所选模型的默认推理强度」）。
            let effort = chat
                .model_groups
                .iter()
                .find(|group| group.provider == provider)
                .and_then(|group| group.models.iter().find(|item| item.id == model))
                .and_then(|item| item.default_effort.clone());

            if chat
                .commands
                .try_send(UiCommand::SetModel {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    effort: effort.clone(),
                })
                .is_ok()
            {
                chat.model = Some(SelectedModel {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    effort,
                });
            }
        }

        let current = self.effective_model();
        // 官方 `trigger.fallback`：当前选择不在目录中时显示这句，而不是破折号。
        let label = current
            .and_then(|selected| self.model_name(&selected.provider, &selected.model))
            .unwrap_or("请选择模型")
            .to_owned();

        let rows = model_menu_rows(&self.model_groups, &self.model_failures, current);

        let trigger = self.chip(
            cx,
            ChipSpec {
                id: "model-chip",
                label,
                open: self.model_open,
                hovered: self.hover_model,
                on_hover: hover,
                on_press: press,
            },
        );

        self.pull_menu(
            cx,
            PullSpec {
                id: "model-cell",
                open: self.model_open,
                width: 264.,
                rows,
            },
            trigger,
            close,
            pick,
        )
    }

    /// 状态行中间的推理强度。
    fn effort_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        fn hover(chat: &mut Chat, hovering: bool) {
            chat.hover_effort = hovering;
        }
        fn press(chat: &mut Chat) {
            chat.effort_open = !chat.effort_open && !chat.efforts().is_empty();
        }
        fn close(chat: &mut Chat) {
            chat.effort_open = false;
        }
        fn pick(chat: &mut Chat, key: &str) {
            let Some(current) = chat.model.clone().or_else(|| chat.model_default.clone()) else {
                return;
            };
            if chat
                .commands
                .try_send(UiCommand::SetModel {
                    provider: current.provider.clone(),
                    model: current.model.clone(),
                    effort: Some(key.to_owned()),
                })
                .is_ok()
            {
                chat.model = Some(SelectedModel {
                    effort: Some(key.to_owned()),
                    ..current
                });
            }
        }

        let efforts = self.efforts();
        let selected = self.current_effort();
        let label = selected.clone().unwrap_or_else(|| "—".to_owned());
        let rows = efforts
            .iter()
            .map(|effort| PullRow {
                key: effort.id.clone(),
                label: effort.name.clone(),
                hint: None,
                selected: selected.as_deref() == Some(effort.id.as_str()),
                muted: false,
            })
            .collect();

        let trigger = self.chip(
            cx,
            ChipSpec {
                id: "effort-chip",
                label,
                open: self.effort_open,
                hovered: self.hover_effort,
                on_hover: hover,
                on_press: press,
            },
        );

        self.pull_menu(
            cx,
            PullSpec {
                id: "effort-cell",
                open: self.effort_open,
                width: 152.,
                rows,
            },
            trigger,
            close,
            pick,
        )
    }

    /// 会话自己选过的模型；没选过返回 `None`。
    fn effective_model(&self) -> Option<&SelectedModel> {
        self.model.as_ref().or(self.model_default.as_ref())
    }

    /// 目录里某个模型的显示名。
    fn model_name(&self, provider: &str, model: &str) -> Option<&str> {
        self.model_groups
            .iter()
            .find(|group| group.provider == provider)?
            .models
            .iter()
            .find(|item| item.id == model)
            .map(|item| item.name.as_str())
    }

    /// 当前模型支持的强度；模型未知时为空。
    fn efforts(&self) -> &[EffortOption] {
        let Some(current) = self.effective_model() else {
            return &[];
        };
        self.model_groups
            .iter()
            .find(|group| group.provider == current.provider)
            .and_then(|group| group.models.iter().find(|item| item.id == current.model))
            .map(|item| item.efforts.as_slice())
            .unwrap_or(&[])
    }

    /// 当前生效的强度：显式选过的优先，否则取该模型的默认档。
    fn current_effort(&self) -> Option<String> {
        let current = self.effective_model()?;
        if let Some(effort) = &current.effort {
            return Some(effort.clone());
        }
        self.model_groups
            .iter()
            .find(|group| group.provider == current.provider)
            .and_then(|group| group.models.iter().find(|item| item.id == current.model))
            .and_then(|item| item.default_effort.clone())
    }

    /// ctx 圆环：底槽 + 进度弧，从 12 点方向顺时针。
    fn ctx_ring(&self, percent: u8) -> impl IntoElement {
        let ratio = f32::from(percent) / 100.0;
        canvas(
            move |bounds, _, _| {
                let width = px(1.5);
                let radius = bounds.size.width / 2.0 - width / 2.0;
                (
                    arc_path(bounds.center(), radius, -90., 360., width),
                    arc_path(bounds.center(), radius, -90., 360. * ratio, width),
                )
            },
            |_bounds, (track, progress), window, _| {
                window.paint_path(track, rgb(theme::RING_TRACK));
                window.paint_path(progress, rgb(theme::INK_SOFT));
            },
        )
        .flex_shrink_0()
        .w(px(11.))
        .h(px(11.))
    }

    fn chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 有分子也有分母才画环（DECISIONS #21）；缺一个就退回绝对量。
        let ctx_cell = match self.occupancy {
            Some(occupancy) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(self.ctx_ring(occupancy.percent))
                .child(format!("{}%", occupancy.percent))
                .into_any_element(),
            None => {
                let text = match &self.meter {
                    Some(meter) => format!("ctx {}", format_tokens(meter.used)),
                    None => "ctx —".to_owned(),
                };
                div().child(text).into_any_element()
            }
        };

        let cache_cell = match self.cache_hit {
            Some(hit) => format!("cache {hit}%"),
            None => "cache —".to_owned(),
        };

        div()
            .w_full()
            .max_w(px(theme::CONTENT_W))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_4()
            .py(px(4.))
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_SOFT))
            // 左端是当前工作区（不是可点控件）：
            // 它回答「这台会话在哪个目录里干活」，与右侧那排读数分开。
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_w(px(0.))
                    .max_w(px(280.))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(div().min_w(px(0.)).truncate().child(self.workspace.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .flex_none()
                    .child(self.model_chip(cx))
                    // 官方：模型没有推理元数据时隐藏强度文案，不显示「effort —」。
                    .children(
                        (!self.efforts().is_empty())
                            .then(|| self.effort_chip(cx).into_any_element()),
                    )
                    .child(ctx_cell)
                    .child(div().child(cache_cell)),
            )
    }
}

impl Render for Chat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(theme::BG_MAIN))
            .font_family(theme::MONO)
            .text_color(rgb(theme::INK))
            .text_size(px(theme::BODY));

        if self.sidebar_open {
            root = root.child(self.sidebar(cx));
        }
        root.child(self.main(cx))
    }
}

fn render_entry(entry: &Entry) -> AnyElement {
    match entry {
        Entry::User { text, .. } => div()
            .flex()
            .flex_row()
            .gap_2()
            .text_size(px(theme::BODY))
            .child(div().text_color(rgb(theme::INK_MUTED)).child("›"))
            .child(div().child(text.clone()))
            .into_any_element(),
        // 助手正文是唯一解析 markdown 的地方（理由见 `markdown` 模块头注释）。
        Entry::Assistant { id, text } => crate::markdown::text(*id, text).into_any_element(),
        Entry::Notice(text) => div()
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child(text.clone())
            .into_any_element(),
        // 工具行走 `Chat::tool_group`（要拿 `cx` 接点击），不会到这里。
        Entry::Tool(call) => div()
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child(call.name.clone())
            .into_any_element(),
    }
}

/// 组头文案：类别摘要 + 计数，运行中再加一句实时提示。
///
/// 官方口径：组头只给类别，不给逐个名字（避免组头比正文还长）。
fn group_label(view: TranscriptView, tools: &[&ToolCall], running: bool) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for call in tools {
        let kind = tool_kind(&call.name);
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds.truncate(3);
    let summary = kinds.join(" · ");
    let count = tools.len();
    match view {
        // 简洁：只给类别。
        TranscriptView::Compact => format!("{summary} · {count}"),
        // 标准：运行中的组补一句「正在跑哪一个」。
        TranscriptView::Standard | TranscriptView::Detailed => {
            if running
                && let Some(last) = tools.last()
                && last.status == ToolStatus::Running
            {
                return format!("{summary} · {count} · 正在跑 {}", last.name);
            }
            format!("{summary} · {count}")
        }
    }
}

/// 连续工具行的区间（渲染期分组的唯一依据）。
///
/// 工具行只与相邻的工具行归一组：中间夹一句助手正文就断开，
/// 所以「思考—工具—回答—工具」会得到两个组，而不是一大块。
fn tool_runs(entries: &[Entry]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        if !matches!(entries[index], Entry::Tool(_)) {
            index += 1;
            continue;
        }
        let start = index;
        while index < entries.len() && matches!(entries[index], Entry::Tool(_)) {
            index += 1;
        }
        runs.push(start..index);
    }
    runs
}

/// 工具类别（组头摘要用）。
fn tool_kind(name: &str) -> &'static str {
    match name {
        "bash" | "pwsh" | "terminal_send" | "bash_persistent" => "命令",
        "read" | "read_image" => "读取",
        "write" | "edit" | "apply_patch" | "str_replace_editor" => "编辑",
        "grep" | "glob" | "search" => "搜索",
        "web_fetch" | "web_search" => "网页",
        "office_render" | "office_convert" | "office_recalculate" => "Office",
        "subagent" | "subagent_fork" => "子智能体",
        _ => "工具",
    }
}

/// 工具行前缀图标（Lucide）。
fn tool_icon(name: &str) -> IconName {
    match name {
        "bash" | "pwsh" | "terminal_send" | "bash_persistent" => IconName::SquareTerminal,
        "read" | "read_image" => IconName::FileText,
        "write" | "edit" | "apply_patch" | "str_replace_editor" => IconName::Pencil,
        "grep" | "glob" | "search" => IconName::Search,
        "web_fetch" | "web_search" => IconName::Globe,
        _ => IconName::Wrench,
    }
}

/// 行尾后缀：状态 + 输出长度。
fn tool_suffix(call: &ToolCall) -> String {
    let mut parts: Vec<String> = Vec::new();
    match call.status {
        ToolStatus::Running => parts.push("运行中".to_owned()),
        ToolStatus::Error => parts.push("失败".to_owned()),
        ToolStatus::Ok => {}
    }
    if call.detail_chars > 0 {
        parts.push(format!("{} 字", call.detail_chars));
    }
    parts.join(" · ")
}

/// 矢量图标：尺寸与颜色跟随容器，命名来自 Lucide（`gpui_kit::assets`）。
fn icon(name: IconName, size: f32, color: u32) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .text_size(px(size))
        .text_color(rgb(color))
        .child(name)
}

/// 右侧面板的一行「标签 + 值」。
fn meta_field(label: &'static str, value: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(52.))
                .flex_shrink_0()
                .text_size(px(theme::META))
                .text_color(rgb(theme::INK_MUTED))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(px(theme::ROW))
                .text_color(rgb(theme::INK_SOFT))
                .child(value.to_owned()),
        )
}

/// 相对时间：刚刚 / N 分钟 / N 小时 / N 天（官方 desktop 时间列的口径）。
fn relative_time(updated_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(updated_at);
    let seconds = (now - updated_at).max(0) / 1000;
    match seconds {
        0..=59 => "刚刚".to_owned(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// 小圆点（状态、会话未读）。
fn dot(size: f32, tone: u32) -> Div {
    div().w(px(size)).h(px(size)).rounded_full().bg(rgb(tone))
}

/// composer 里的小圆按钮：24pt 方框包一枚 13pt 图标。
///
/// 发送/停止共用一个形状，只换图标与字色——chrome 保持零彩色。
fn round_button(
    id: &'static str,
    glyph: IconName,
    tone: u32,
    on_press: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(24.))
        .h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::BG_HOVER)))
        .on_mouse_down(MouseButton::Left, on_press)
        .child(icon(glyph, 13.0, tone))
}

/// 把 token 数压成短标签：`912` / `5.7k` / `128k`。
fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 100_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else if tokens < 1_000_000 {
        format!("{}k", tokens / 1_000)
    } else {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    }
}

/// 一段圆弧的描边路径（`PathStyle::Stroke`，不需要拼环带）。
///
/// 用折线逼近而不是 `arc_to`——尺寸只有 11px，10° 一段已经看不出棱角。
fn arc_path(
    center: Point<Pixels>,
    radius: Pixels,
    start_deg: f32,
    sweep_deg: f32,
    width: Pixels,
) -> Path<Pixels> {
    let mut builder = PathBuilder::stroke(width);
    if sweep_deg.abs() < 0.5 {
        return Path::new(center);
    }

    let steps = ((sweep_deg.abs() / 10.0).ceil() as usize).max(3);
    let at = |step: usize| {
        let angle = (start_deg + sweep_deg * step as f32 / steps as f32).to_radians();
        point(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        )
    };

    builder.move_to(at(0));
    for step in 1..=steps {
        builder.line_to(at(step));
    }
    // 描边三角化理论上可能失败（退化多边形）；失败就画空路径，不 panic。
    builder.build().unwrap_or_else(|_| Path::new(center))
}

/// 取路径末段作为工作区名。
fn leaf(path: &str) -> String {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// 当前会话这一轮处在哪一步。
///
/// 只有一个来源：本端发出去 + 收帧 + 结束，**不**去猜服务端的内部阶段。
/// composer 的主按钮、时间线尾巴的文案、侧栏那个运行点全读它（再加
/// `session/list` 的 `running` 给其他会话）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 没有进行中的轮次。
    Idle,
    /// 已交给 host，还没收到任何一帧。
    Sending,
    /// host 已接住，正在产帧。
    Running,
}

impl Phase {
    /// 是否有轮次在进行（`Sending` 也算）。
    fn is_busy(self) -> bool {
        self != Phase::Idle
    }
}

/// 侧栏的「已归档」区用一个不会撞路径的键记展开态。
const ARCHIVED_KEY: &str = "\u{1}已归档";

/// 行内确认的种类：决定「取消」清哪个 pending、「确认」发哪条命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmKind {
    /// 移除工作区注册。
    WorkspaceDelete,
    /// 归档一条会话。
    SessionArchive,
}

impl ConfirmKind {
    /// 确认胶囊上的动词。
    fn verb(self) -> &'static str {
        match self {
            ConfirmKind::WorkspaceDelete => "移除",
            ConfirmKind::SessionArchive => "归档",
        }
    }
}

/// 侧栏的一个分组：一个已注册工作区，或「未分组」。
struct SideGroup<'a> {
    /// 展开状态与行 id 的键（工作区用 path）。
    key: String,
    /// 显示名。
    label: String,
    /// 工作区 id；「未分组」为 `None`。
    workspace_id: Option<String>,
    /// 组内会话，按更新时间倒序。
    items: Vec<&'a SessionInfo>,
}

/// host 把路径规范化成 `/private/tmp/...`，而客户端手里可能是 `/tmp/...`；
/// 比较前先把 `/private` 前缀抹平。
fn same_path(left: &str, right: &str) -> bool {
    fn strip(path: &str) -> &str {
        path.strip_prefix("/private").unwrap_or(path)
    }
    let (left, right) = (
        strip(left.trim_end_matches('/')),
        strip(right.trim_end_matches('/')),
    );
    left == right
}

/// 把 home 前缀缩成 `~`，与设计稿的路径行一致。
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

/// 会话 id 太长，标题栏只留尾部。
fn short_id(session: &str) -> &str {
    let keep = 8;
    if session.len() <= keep {
        return session;
    }
    &session[session.len() - keep..]
}

#[cfg(test)]
mod tests {
    use super::{
        ModelGroup, PullRow, SelectedModel, TranscriptView, format_tokens, leaf, model_menu_rows,
        short_id, tool_runs,
    };
    use crate::ModelOption;

    fn group(provider: &str, models: &[(&str, &str)]) -> ModelGroup {
        ModelGroup {
            provider: provider.to_owned(),
            provider_name: "DeepSeek".to_owned(),
            models: models
                .iter()
                .map(|(id, name)| ModelOption {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                    default_effort: None,
                    efforts: Vec::new(),
                })
                .collect(),
        }
    }

    /// 目录空时菜单只有一行不可点的「暂无可用模型」（官方 `empty.models`）。
    #[test]
    fn empty_catalog_yields_one_muted_hint() {
        let rows = model_menu_rows(&[], &[], None);
        assert_eq!(
            rows,
            vec![PullRow {
                key: "empty".to_owned(),
                label: "暂无可用模型".to_owned(),
                hint: None,
                selected: false,
                muted: true,
            }]
        );
    }

    /// 提供方列举失败逐条显示（官方 `warning.groupLoad` 已由主程序格式化），不可点。
    #[test]
    fn failures_are_appended_as_muted_rows() {
        let rows = model_menu_rows(&[], &["DeepSeek 加载失败：missing key".to_owned()], None);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].muted);
        assert_eq!(rows[0].label, "DeepSeek 加载失败：missing key");
        // 有故障行时不再补空态文案（两者不叠在一起）。
        assert!(!rows.iter().any(|row| row.label == "暂无可用模型"));
    }

    /// 正常目录：键是 `provider/model`，选中项打勾，多个 provider 才带 hint。
    #[test]
    fn normal_catalog_rows_are_pickable() {
        let groups = vec![
            group("deepseek-official", &[("deepseek-flash", "Flash")]),
            group("third", &[("x", "X")]),
        ];
        let current = SelectedModel {
            provider: "deepseek-official".to_owned(),
            model: "deepseek-flash".to_owned(),
            effort: None,
        };
        let rows = model_menu_rows(&groups, &[], Some(&current));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.muted));
        assert_eq!(rows[0].key, "deepseek-official/deepseek-flash");
        assert!(rows[0].selected);
        assert_eq!(rows[0].hint.as_deref(), Some("DeepSeek"));
        assert!(!rows[1].selected);
    }

    #[test]
    fn formats_short_labels() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(912), "912");
        assert_eq!(format_tokens(5_678), "5.7k");
        assert_eq!(format_tokens(99_950), "100.0k");
        assert_eq!(format_tokens(128_000), "128k");
    }

    #[test]
    fn shortens_session_ids() {
        assert_eq!(short_id("short"), "short");
        assert_eq!(
            short_id("session-9771885c-6753-4117-80e7-88047f9ec141"),
            "7f9ec141"
        );
    }

    #[test]
    fn workspace_name_is_the_last_segment() {
        assert_eq!(leaf("/Users/x/projects/dsh-client-rs"), "dsh-client-rs");
        assert_eq!(leaf("/Users/x/projects/dsh-client-rs/"), "dsh-client-rs");
        assert_eq!(leaf("/"), "/");
    }

    fn tool(id: &str) -> dsh_protocol::transcript::Entry {
        dsh_protocol::transcript::Entry::Tool(dsh_protocol::transcript::ToolCall {
            id: id.to_owned(),
            name: "bash".to_owned(),
            summary: "ls".to_owned(),
            status: dsh_protocol::transcript::ToolStatus::Ok,
            detail: String::new(),
            detail_chars: 0,
        })
    }

    fn text(body: &str) -> dsh_protocol::transcript::Entry {
        dsh_protocol::transcript::Entry::Assistant {
            id: dsh_protocol::transcript::MessageId::Turn { turn: 1, step: 1 },
            text: body.to_owned(),
        }
    }

    /// 只有**相邻**的工具行才归一组：中间夹助手正文就断开。
    #[test]
    fn tool_runs_break_on_assistant_text() {
        let entries = vec![text("a"), tool("t1"), tool("t2"), text("b"), tool("t3")];
        let runs = tool_runs(&entries);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], 1..3);
        assert_eq!(runs[1], 4..5);
    }

    #[test]
    fn tool_runs_are_empty_without_tools() {
        assert!(tool_runs(&[text("a")]).is_empty());
        assert!(tool_runs(&[]).is_empty());
    }

    /// 模式只影响**组**：详细模式只在运行中把组展开，历史轮次仍手动。
    #[test]
    fn view_controls_group_body_by_default() {
        assert!(!TranscriptView::Compact.group_body_open(true));
        assert!(!TranscriptView::Standard.group_body_open(true));
        assert!(TranscriptView::Detailed.group_body_open(true));
        assert!(!TranscriptView::Detailed.group_body_open(false));
    }

    #[test]
    fn view_cycles_through_three_modes() {
        assert_eq!(TranscriptView::Compact.next(), TranscriptView::Standard);
        assert_eq!(TranscriptView::Standard.next(), TranscriptView::Detailed);
        assert_eq!(TranscriptView::Detailed.next(), TranscriptView::Compact);
    }
}
