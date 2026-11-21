//! 主视图：侧栏 + 主区（标题栏 / 时间线 / composer）。
//!
//! 几何全部取自 Ardot 设计稿 `747373011992944`：侧栏 264、内容列 860、
//! 顶/底栏 52、主区水平内边距 28、侧栏内边距 16。
//!
//! 数据流是单向的：外部把 [`UiEvent`] 推进 channel，本视图消费后更新状态，
//! 用户输入通过 [`UiCommand`] 推回去。视图内**不做网络**。

use dsh_protocol::workspace::WorkspaceRegistry;
use gpui_kit::base::input::{InputEditorStyle, InputEvent, Textarea, TextareaState};
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::completion;
use crate::markdown::TextRole;
use crate::settings::{ChatVerbosity, RenderPolicy, Settings};
use crate::theme;
use crate::{
    CompletionItem, CtxMeter, EffortOption, HostLifecycle, ModelGroup, PresetInfo, SelectedModel,
    SessionInfo, SidebarPrefs, UiCommand, UiEvent,
};
use dsh_protocol::context::Occupancy;
use dsh_protocol::transcript::ToolStatus;
use dsh_protocol::{ChatTimeline, RenderId, TimelineRow, ToolFamily, ToolResultView, ToolView};
use gpui_kit::assets::IconName;

/// 设置模式的两个应用级视图：偏好与归档会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsTab {
    #[default]
    General,
    Archive,
}

/// 主视图。
pub struct Chat {
    input: Entity<TextareaState>,
    /// 唯一时间线事实源；entries / stream / tool 更新都从这里进出。
    timeline: ChatTimeline,
    meter: Option<CtxMeter>,
    commands: Sender<UiCommand>,
    /// 工作区名（取 cwd 的末段）。
    workspace: String,
    /// 会话标题；拿到投影标题前用会话 id 尾部代替。
    title: String,
    /// 会话根的 `~` 缩写路径。
    path: String,
    /// host 生命周期。唯一来源是 supervisor；初始就是启动中。
    host_state: HostLifecycle,
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
    /// 全局界面设置的内存镜像。真值在 host profile 的 `ui-chat.transcriptView`。
    settings: Settings,
    /// host 设置读写失败说明；settings 页显示在选项下方。
    settings_error: Option<String>,
    /// 最近一次点击是否已被 host 确认写回（用于成功反馈）。
    settings_saved: bool,
    /// settings 页面是否覆盖主区。
    settings_open: bool,
    /// 设置模式当前选中的叶子（偏好→聊天 / 会话→归档）。
    settings_tab: SettingsTab,
    /// 设置树分组的展开状态：key 为 `prefs` / `sessions`。缺省展开。
    settings_tree: HashMap<String, bool>,
    /// 每个工具行正文的开合覆盖；单个工具正文始终是手动展开。
    tool_open: HashMap<String, bool>,
    /// 每个 reasoning 块的正文开合覆盖；会话内状态，不写设置。
    reasoning_open: HashMap<String, bool>,
    /// 仅调试：把过程与正文都当成展开（验排版用）。
    debug_tools_open: bool,
    /// 过程组的开合；没有记录的组按档位默认。
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
    /// 客户端私有侧栏偏好：隐藏集合 + 历史会话归属覆盖。
    prefs: SidebarPrefs,
    /// 正在等二次确认的工作区 id（垃圾桶按一下不直接删）。
    pending_delete: Option<String>,
    /// 正在等二次确认的**运行中**会话 id（归档会停掉它，所以先问一句）。
    pending_archive: Option<String>,
    /// 归档页里等二次确认的「从本机移除」会话 id。
    pending_session_delete: Option<String>,
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
            timeline: ChatTimeline::new(),
            meter: None,
            commands,
            workspace: "未连接".to_owned(),
            title: "新会话".to_owned(),
            path: String::new(),
            host_state: HostLifecycle::Starting,
            phase: Phase::Idle,
            turn_started: None,
            sessions: Vec::new(),
            active_session: String::new(),
            expanded: HashMap::new(),
            settings: Settings::default(),
            settings_error: None,
            settings_saved: false,
            settings_open: false,
            settings_tab: SettingsTab::default(),
            settings_tree: HashMap::new(),
            tool_open: HashMap::new(),
            reasoning_open: HashMap::new(),
            debug_tools_open: std::env::var("DSH_DEBUG_TOOL_OPEN").is_ok(),
            open_groups: HashMap::new(),
            sidebar_open: true,
            right_panel_open: false,
            completion: None,
            completion_seq: 0,
            hover_completion: None,
            registry: WorkspaceRegistry::default(),
            prefs: SidebarPrefs::default(),
            pending_delete: None,
            pending_archive: None,
            pending_session_delete: None,
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
            UiEvent::HostState(state) => {
                tracing::info!(?state, "host 状态");
                self.host_state = state;
            }
            UiEvent::Connected {
                origin,
                session,
                cwd,
            } => {
                tracing::info!(%origin, %session, %cwd, "已连接 host");
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
                self.adopt_legacy_sessions(cx);
            }
            UiEvent::SidebarPrefs(prefs) => {
                self.prefs = prefs;
                cx.notify();
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
                self.adopt_legacy_sessions(cx);
                cx.notify();
            }
            UiEvent::Presets(options) => {
                self.mode_options = options;
            }
            UiEvent::ChatVerbosity {
                verbosity,
                error,
                saved,
            } => {
                self.apply_verbosity(verbosity);
                self.settings_error = error;
                self.settings_saved = saved;
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
                self.timeline.replace_snapshot(items);
                self.tool_open.clear();
                self.reasoning_open.clear();
                self.open_groups.clear();
            }
            UiEvent::Tool(call) => {
                // 时间线模块按 `callId` upsert：发起插行，结果回来原地收尾。
                self.timeline.apply_tool(call);
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
                let effect = self.timeline.apply_stream(frame);
                self.apply_stream_effect(effect);
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
                self.timeline.push_notice(message);
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

    /// 流末副作用：usage 记账 + 回合收尾。内容落行已经由时间线模块完成。
    fn apply_stream_effect(&mut self, effect: dsh_protocol::timeline::StreamEffect) {
        let Some(outcome) = effect.outcome else {
            return;
        };
        if let Some(usage) = effect.usage {
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
                used: usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
            });
        }
        tracing::info!(
            ?outcome,
            chars = effect.chars,
            input_tokens = self.meter.map(|meter| meter.used),
            "回合完成"
        );
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
        self.timeline.push_user(text.clone());
        self.phase = Phase::Sending;
        self.turn_started = Some(std::time::Instant::now());
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));

        if self.commands.try_send(UiCommand::Prompt(text)).is_err() {
            self.finish_turn();
            self.timeline
                .push_notice("没有连上 host，消息未发出".to_owned());
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
            self.timeline
                .push_notice("没有连上 host，停不下来".to_owned());
        } else {
            // 收尾还靠 `turn/end`（那一帧到了才真正回空闲），但先给一条回声，
            // 不然点了「停」界面上什么都没变。
            self.timeline.push_notice("已请求停止本轮".to_owned());
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

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.settings_open {
            // 设置模式下侧栏是设置导航：工作区列表让位，避免上下文错位。
            div()
                .flex()
                .flex_col()
                .w(px(theme::SIDEBAR_W))
                .h_full()
                .bg(rgb(theme::BG_SIDEBAR))
                .child(self.settings_side_top(cx))
                .child(self.settings_side_body(cx))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .w(px(theme::SIDEBAR_W))
                .h_full()
                .bg(rgb(theme::BG_SIDEBAR))
                .child(self.side_top(cx))
                .child(self.side_body(cx))
                .child(self.side_foot(cx))
                .into_any_element()
        }
    }

    /// 设置模式的侧栏顶栏：品牌 + 收起侧栏（返回入口在侧栏正文顶部）。
    fn settings_side_top(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::BAR_H))
            .px(px(theme::SIDE_PAD))
            .gap_2()
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
            .child(self.side_close(cx))
    }

    /// 设置导航树：两组（偏好 / 会话）可折叠，叶子选中切换主区；偏好按真值归属分组。
    fn settings_side_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let back = cx.entity();
        let prefs_open = self.settings_tree.get("prefs").copied().unwrap_or(true);
        let sessions_open = self.settings_tree.get("sessions").copied().unwrap_or(true);
        let mut body = div()
            .id("settings-side-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(theme::SIDE_PAD))
            .pt(px(theme::SIDE_PAD))
            .pb(px(theme::SIDE_PAD))
            .gap_1()
            .child(
                div()
                    .id("settings-side-back")
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        back.update(cx, |chat, cx| {
                            chat.settings_open = false;
                            cx.notify();
                        });
                    })
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .text_size(px(theme::UI))
                    .text_color(rgb(theme::INK_SOFT))
                    .child("← 返回工作区"),
            )
            .child(self.settings_tree_group("prefs", "偏好", cx));
        if prefs_open {
            body = body.child(div().pl(px(22.)).child(self.settings_tree_leaf(
                "settings-nav:chat",
                SettingsTab::General,
                "聊天 · 详略",
                "全局详略；控制 reasoning 与工具过程的默认显示",
                cx,
            )));
        }
        body = body.child(self.settings_tree_group("sessions", "会话", cx));
        if sessions_open {
            body = body.child(div().pl(px(22.)).child(self.settings_tree_leaf(
                "settings-nav:archive",
                SettingsTab::Archive,
                "归档",
                "可恢复，或从本机移除",
                cx,
            )));
        }
        body
    }

    /// 设置树的一个分组行：点它折叠/展开子项，不切换主区。
    fn settings_tree_group(
        &self,
        key: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.settings_tree.get(key).copied().unwrap_or(true);
        let arrow = if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let toggle = cx.entity();
        div()
            .id(ElementId::Name(format!("settings-tree:{key}").into()))
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
                toggle.update(cx, |chat, cx| {
                    let next = !chat.settings_tree.get(key).copied().unwrap_or(true);
                    chat.settings_tree.insert(key.to_owned(), next);
                    cx.notify();
                });
            })
            .child(icon(arrow, 12.0, theme::INK_MUTED))
            .child(
                div()
                    .text_size(px(theme::UI))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(label),
            )
    }

    /// 设置树的一个叶子行：选中它切换主区页面；副标题只在该叶子选中时显示。
    fn settings_tree_leaf(
        &self,
        id: &'static str,
        tab: SettingsTab,
        label: &'static str,
        note: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let select = cx.entity();
        let selected = self.settings_tab == tab;
        let mut row = div()
            .id(ElementId::Name(id.into()))
            .flex()
            .flex_col()
            .gap_1()
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .bg(rgb(if selected {
                theme::BG_HOVER
            } else {
                theme::BG_SIDEBAR
            }))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                select.update(cx, |chat, cx| {
                    chat.settings_tab = tab;
                    cx.notify();
                });
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(6.))
                            .h(px(6.))
                            .rounded_full()
                            .bg(rgb(if selected {
                                theme::INK
                            } else {
                                theme::INK_MUTED
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(theme::UI))
                            .text_color(rgb(if selected {
                                theme::INK
                            } else {
                                theme::INK_SOFT
                            }))
                            .child(label),
                    ),
            );
        if selected {
            row = row.child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(note),
            );
        }
        row
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
                &group.workspace_id,
                !group.items.is_empty(),
                cx,
            ));
            if self.expanded.get(&group.key).copied().unwrap_or(true) {
                for session in group.items {
                    body = body.child(self.session_row(&group.key, session, cx));
                }
            }
        }

        // 隐藏区：移除工作区只是本地隐藏，节名留着可恢复，避免会话「静默消失」。
        const HIDDEN_KEY: &str = "\u{0}已隐藏";
        let hidden: Vec<(String, String)> = self
            .registry
            .items
            .iter()
            .filter(|workspace| {
                self.prefs
                    .hidden_workspace_ids
                    .contains(&workspace.workspace_id)
            })
            .map(|workspace| (workspace.workspace_id.clone(), workspace.title.clone()))
            .collect();
        if !hidden.is_empty() {
            let open = self.expanded.get(HIDDEN_KEY).copied().unwrap_or(false);
            let arrow = if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            };
            let toggle = cx.entity();
            body = body.child(
                div()
                    .id(ElementId::Name("hidden:header".into()))
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
                        toggle.update(cx, |chat, cx| {
                            let next = !chat.expanded.get(HIDDEN_KEY).copied().unwrap_or(false);
                            chat.expanded.insert(HIDDEN_KEY.to_owned(), next);
                            cx.notify();
                        });
                    })
                    .child(icon(arrow, 12.0, theme::INK_MUTED))
                    .child(
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child(format!("已隐藏的工作区（{}）", hidden.len())),
                    ),
            );
            if open {
                for (workspace_id, title) in hidden {
                    let restore = cx.entity();
                    body = body.child(
                        div()
                            .id(ElementId::Name(format!("hidden:{workspace_id}").into()))
                            .flex()
                            .flex_row()
                            .items_center()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                restore.update(cx, |chat, cx| {
                                    chat.prefs.hidden_workspace_ids.remove(&workspace_id);
                                    let _ = chat
                                        .commands
                                        .try_send(UiCommand::SaveSidebarPrefs(chat.prefs.clone()));
                                    cx.notify();
                                });
                            })
                            .child(
                                div()
                                    .text_size(px(theme::UI))
                                    .text_color(rgb(theme::INK_SOFT))
                                    .child(title),
                            ),
                    );
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
            .filter(|session| {
                self.registry.is_archived(&session.id)
                    && !self.prefs.deleted_session_ids.contains(&session.id)
            })
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

    /// 侧栏分组：按 host 账本 + 客户端历史覆盖归属；只列工作区，没有
    /// 「未分组」组，隐藏工作区与无归属会话都不显示。
    fn workspace_groups(&self) -> Vec<SideGroup<'_>> {
        group_plan(
            &self.sessions,
            &self.registry,
            &self.prefs,
            &self.active_session,
        )
        .into_iter()
        .map(|(key, label, workspace_id, ids)| {
            let mut items: Vec<&SessionInfo> = ids
                .iter()
                .filter_map(|id| self.sessions.iter().find(|session| &session.id == id))
                .collect();
            items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
            SideGroup {
                key,
                label,
                workspace_id,
                items,
            }
        })
        .collect()
    }

    /// 把老客户端用 cwd-only 建出来、host 账本里没有的会话收进本地覆盖。
    ///
    /// 只在「不属于任何账本/覆盖、且 cwd 命中某个工作区」时收养；已经被别组
    /// 记账的会话不会被重复挂到第二组。cwd 只在这里做一次性启发，分组本身
    /// 一律走账本。
    fn adopt_legacy_sessions(&mut self, cx: &mut Context<Self>) {
        let registry = &self.registry;
        let sessions = &self.sessions;
        let prefs = &mut self.prefs;
        let mut owned: HashSet<String> = HashSet::new();
        for workspace in &registry.items {
            owned.extend(workspace.session_ids.iter().cloned());
        }
        for ids in prefs.adopted.values() {
            owned.extend(ids.iter().cloned());
        }

        let mut changed = false;
        for workspace in &registry.items {
            if prefs.hidden_workspace_ids.contains(&workspace.workspace_id) {
                continue;
            }
            let adopted = prefs
                .adopted
                .entry(workspace.workspace_id.clone())
                .or_default();
            for session in sessions {
                if session.blank
                    || registry.is_archived(&session.id)
                    || owned.contains(&session.id)
                    || prefs.deleted_session_ids.contains(&session.id)
                {
                    continue;
                }
                if same_path(&session.cwd, &workspace.path) {
                    adopted.push(session.id.clone());
                    owned.insert(session.id.clone());
                    changed = true;
                }
            }
        }
        if changed {
            let _ = self
                .commands
                .try_send(UiCommand::SaveSidebarPrefs(self.prefs.clone()));
            cx.notify();
        }
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

    /// 工作区行：侧栏只有已注册的工作区，每行都有 id，既能建会话也能移除。
    fn workspace_row(
        &self,
        cwd: &str,
        label: &str,
        workspace_id: &str,
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
        let remove_id = workspace_id.to_owned();
        let create_workspace_id = remove_id.clone();
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

        if self.pending_delete.as_deref() == Some(remove_id.as_str()) {
            // 二次确认：就地把两枚图标换成「取消 / 移除」，不进对话框。
            // 移除只动本地显示（目录、会话都在），所以确认文案不必吓人。
            row = row.child(self.confirm_bar(
                "ws-delete",
                &remove_id,
                ConfirmKind::WorkspaceDelete,
                cx,
            ));
        } else {
            // hover 才出现的两枚小图标：新建会话 / 移除工作区。
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
                                    workspace_id: create_workspace_id.clone(),
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
                            let id = remove_id.clone();
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
            ConfirmKind::SessionDelete => {
                if self.pending_session_delete.as_deref() == Some(id) {
                    self.pending_session_delete = None;
                }
            }
        }
    }

    /// 确认时真正动手：先乐观改本地状态（侧栏立刻收起），再把命令推出去。
    fn confirm_pending(&mut self, kind: ConfirmKind, id: String) {
        match kind {
            ConfirmKind::WorkspaceDelete => {
                // 本地隐藏，不碰 host 注册与账本：重添加同一目录时会话原样回来。
                // 注册表仍留着这条，UI 分组时按 hidden 过滤，隐藏入口也能显示标题。
                self.prefs.hidden_workspace_ids.insert(id);
                self.pending_delete = None;
                let _ = self
                    .commands
                    .try_send(UiCommand::SaveSidebarPrefs(self.prefs.clone()));
            }
            ConfirmKind::SessionArchive => {
                self.archive_session(id);
            }
            ConfirmKind::SessionDelete => {
                self.pending_session_delete = None;
                self.delete_session(id);
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

    /// 从本客户端移除一条归档会话。host 没有删会话命令，只做本地过滤并落快照；
    /// 远端记录仍在，不假装它被删除了。
    fn delete_session(&mut self, id: String) {
        self.prefs.deleted_session_ids.insert(id);
        let _ = self
            .commands
            .try_send(UiCommand::SaveSidebarPrefs(self.prefs.clone()));
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
                        chat.timeline = ChatTimeline::new();
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

    fn side_foot(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (tone, label) = host_status(self.host_state);
        let settings = cx.entity();

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
            .child(
                div()
                    .id("settings-toggle")
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        settings.update(cx, |chat, cx| {
                            chat.settings_open = !chat.settings_open;
                            cx.notify();
                        });
                    })
                    .child(icon(IconName::Settings, 14.0, theme::INK_MUTED)),
            )
    }

    /// 最小 settings 页面：一个设置、三档取值，不摆未来项占位。
    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut segments = div().flex().flex_row().flex_1().gap_1();
        for verbosity in ChatVerbosity::ALL {
            let entity = cx.entity();
            let sender = self.commands.clone();
            let previous = self.settings.verbosity;
            let selected = self.settings.verbosity == verbosity;
            segments = segments.child(
                div()
                    .id(ElementId::Name(
                        format!("verbosity:{}", verbosity.key()).into(),
                    ))
                    .flex()
                    .flex_1()
                    .justify_center()
                    .items_center()
                    .py(px(8.))
                    .rounded(px(6.))
                    .bg(rgb(if selected {
                        theme::BG_HOVER
                    } else {
                        theme::BG_CARD
                    }))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let _ = sender.try_send(UiCommand::SetChatVerbosity {
                            verbosity,
                            previous,
                        });
                        entity.update(cx, |chat, cx| {
                            chat.apply_verbosity(verbosity);
                            chat.settings_error = None;
                            chat.settings_saved = false;
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .text_size(px(theme::UI))
                            .text_color(rgb(if selected {
                                theme::INK
                            } else {
                                theme::INK_SOFT
                            }))
                            .child(verbosity.label()),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(
                div()
                    .id("settings-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .w_full()
                    .max_w(px(theme::CONTENT_W))
                    .mx_auto()
                    .px(px(theme::GUTTER))
                    .py(px(theme::GUTTER))
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(theme::BODY))
                                    .text_color(rgb(theme::INK))
                                    .child("聊天"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::META))
                                    .text_color(rgb(theme::INK_MUTED))
                                    .child("全局详略；控制 reasoning 与工具过程的默认显示"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .p(px(4.))
                            .rounded(px(10.))
                            .bg(rgb(theme::BG_CARD))
                            .border_1()
                            .border_color(rgb(theme::BG_CARD_EDGE))
                            .child(segments),
                    )
                    .child(
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .child(self.settings.verbosity.description()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child("真值：~/.dsh/profiles/cowork · ui-chat.transcriptView（经 host API 读写）"),
                    )
                    .child(if let Some(error) = self.settings_error.as_deref() {
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .child(format!("保存失败：{error}"))
                            .into_any_element()
                    } else if self.settings_saved {
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .child("已保存到 host profile")
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
    }

    /// 归档 Tab：恢复走 host `unarchiveSession`，删除只是本客户端移除（host 无删会话 API）。
    fn archive_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let archived = self.archived_sessions();
        let mut list = div().flex().flex_col().gap_2();
        for session in &archived {
            let id = session.id.clone();
            let title = session
                .title
                .clone()
                .unwrap_or_else(|| short_id(&session.id).to_owned());
            let time = relative_time(session.updated_at);
            let restore = cx.entity();
            let remove = cx.entity();
            let confirming = self.pending_session_delete.as_deref() == Some(id.as_str());
            let mut row = div()
                .id(ElementId::Name(format!("archive:{}", session.id).into()))
                .group("archive-row")
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px(px(12.))
                .py(px(10.))
                .rounded(px(6.))
                .bg(rgb(theme::BG_CARD))
                .border_1()
                .border_color(rgb(theme::BG_CARD_EDGE))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(theme::UI))
                        .text_color(rgb(theme::INK))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_MUTED))
                        .child(time),
                );
            if confirming {
                row = row.child(self.confirm_bar(
                    "archive-delete",
                    &id,
                    ConfirmKind::SessionDelete,
                    cx,
                ));
            } else {
                let restore_id = id.clone();
                let remove_id = id.clone();
                row = row
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let target = restore_id.clone();
                                restore.update(cx, |chat, cx| {
                                    chat.unarchive_session(target);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            })
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .child("恢复"),
                    )
                    .child(
                        div()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let target = remove_id.clone();
                                remove.update(cx, |chat, cx| {
                                    chat.pending_session_delete = Some(target);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            })
                            .child(icon(IconName::Delete, 12.0, theme::INK_MUTED)),
                    );
            }
            list = list.child(row);
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(
            div()
                .id("archive-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .w_full()
                .max_w(px(theme::CONTENT_W))
                .mx_auto()
                .px(px(theme::GUTTER))
                .py(px(theme::GUTTER))
                .gap_6()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(theme::BODY))
                                .text_color(rgb(theme::INK))
                                .child("归档"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::META))
                                .text_color(rgb(theme::INK_MUTED))
                                .child(
                                    "归档会话可恢复到原工作区，或从本机移除；移除不影响 host 记录",
                                ),
                        ),
                )
                .child(if archived.is_empty() {
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_MUTED))
                        .child("没有归档会话")
                        .into_any_element()
                } else {
                    list.into_any_element()
                }),
        )
    }

    /// 应用档位只影响默认值；已展开的会话内覆盖清掉，让新档位立即生效。
    /// 真值由 host 回送，这里只更新内存镜像。
    fn apply_verbosity(&mut self, verbosity: ChatVerbosity) {
        self.settings.verbosity = verbosity;
        self.tool_open.clear();
        self.reasoning_open.clear();
        self.open_groups.clear();
    }

    // ── 主区 ────────────────────────────────────────────────────────────

    fn main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 还没选工作区 / 没有会话：主区是空状态，不是聊天区。
        let body = if self.settings_open {
            match self.settings_tab {
                SettingsTab::General => self.settings_page(cx).into_any_element(),
                SettingsTab::Archive => self.archive_page(cx).into_any_element(),
            }
        } else if self.active_session.is_empty() {
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

        if self.right_panel_open && !self.settings_open && !self.active_session.is_empty() {
            main = main.child(self.right_panel());
        }

        main
    }

    /// 没有会话时的主区：像官方 desktop 的空状态，直接列出已注册的工作区。
    ///
    /// 选一个工作区就在那个目录下建会话；没有工作区就只留「添加工作区…」。
    /// 这样新会话一定建在明确选定的工作区里，cwd 也不再由启动目录 / 家目录决定。
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
            if self
                .prefs
                .hidden_workspace_ids
                .contains(&workspace.workspace_id)
            {
                continue;
            }
            let path = workspace.path.clone();
            let workspace_id = workspace.workspace_id.clone();
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
                            let _ = chat.commands.try_send(UiCommand::NewSession {
                                cwd: path.clone(),
                                workspace_id: workspace_id.clone(),
                            });
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

        let has_visible = self.registry.items.iter().any(|workspace| {
            !self
                .prefs
                .hidden_workspace_ids
                .contains(&workspace.workspace_id)
        });
        let hint = if !has_visible {
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

        // 行来自时间线投影；过程组是渲染期派生的，不回写投影状态。
        // 详略档位只改显示默认值，不改投影：切档不会丢会话内容。
        let rows = self.timeline.rows();
        let mut index = 0;
        for run in process_runs(&rows) {
            while index < run.start {
                column = column.child(render_row(&rows[index]));
                index += 1;
            }
            column = column.child(self.process_group(&rows[run.clone()], cx));
            index = run.end;
        }
        while index < rows.len() {
            column = column.child(render_row(&rows[index]));
            index += 1;
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

    /// 一段连续的过程行（reasoning + 工具）：组头 + 可选的行列表。
    fn process_group(&self, rows: &[TimelineRow], cx: &mut Context<Self>) -> Div {
        let Some(first) = rows.first() else {
            return div();
        };
        let key = row_render_id(first).label();
        let policy = self.settings.verbosity.policy();
        let running_turn = process_group_running(rows, self.phase.is_busy());
        let open = self.debug_tools_open
            || self
                .open_groups
                .get(&key)
                .copied()
                .unwrap_or_else(|| policy.group_body_open(running_turn));

        let mut group = div().flex().flex_col().gap_1();
        {
            let toggle = cx.entity();
            let toggle_key = key.clone();
            let header = div()
                .id(format!("group-header:{key}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .py(px(2.))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    toggle.update(cx, |chat, cx| {
                        let policy = chat.settings.verbosity.policy();
                        let next = !chat
                            .open_groups
                            .get(&toggle_key)
                            .copied()
                            .unwrap_or_else(|| policy.group_body_open(running_turn));
                        chat.open_groups.insert(toggle_key.clone(), next);
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
                        .child(group_label(policy, rows, running_turn)),
                )
                .child(div().flex_1());
            group = group.child(header);
        }

        if open {
            let mut body = div().flex().flex_col().gap_1().pl(px(16.));
            for row in rows {
                body = match row {
                    TimelineRow::Reasoning {
                        id,
                        text,
                        streaming,
                    } => body.child(self.reasoning_row(id, text, *streaming, cx)),
                    TimelineRow::Tool(view) => body.child(self.tool_row(view, cx)),
                    other => body.child(render_row(other)),
                };
            }
            group = group.child(body);
        }
        group
    }

    /// 一行 reasoning：默认收起，按档位给不给预览；正文点开后才是纯文本。
    fn reasoning_row(
        &self,
        id: &RenderId,
        text: &str,
        streaming: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let policy = self.settings.verbosity.policy();
        let key = id.label();
        let open = self.debug_tools_open || self.reasoning_open.get(&key).copied().unwrap_or(false);
        let preview = reasoning_summary(text, streaming, policy);
        let entity = cx.entity();
        let toggle_key = key.clone();

        let row = div()
            .id(format!("reasoning-row:{key}"))
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    let next = !chat
                        .reasoning_open
                        .get(&toggle_key)
                        .copied()
                        .unwrap_or(false);
                    chat.reasoning_open.insert(toggle_key.clone(), next);
                    cx.notify();
                });
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
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
                            .flex_shrink_0()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child("思考"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child(preview),
                    ),
            );

        if !open {
            return row.into_any_element();
        }
        row.child(crate::markdown::text(id.clone(), text, TextRole::Reasoning))
            .into_any_element()
    }

    /// 一行工具调用：家族图标 + 名字 + 标题 + 后缀；点开看审计字段与结果。
    fn tool_row(&self, view: &ToolView, cx: &mut Context<Self>) -> Div {
        // 单个工具正文永远手动展开；档位只决定过程组的默认开合。
        let open = self.debug_tools_open || self.tool_open.get(&view.id).copied().unwrap_or(false);
        let entity = cx.entity();
        let id = view.id.clone();
        let stroke = match view.status {
            ToolStatus::Running => theme::INK_SOFT,
            ToolStatus::Ok => theme::INK_MUTED,
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
                    chat.tool_open.insert(
                        id.clone(),
                        !chat.tool_open.get(&id).copied().unwrap_or(false),
                    );
                    cx.notify();
                });
            })
            .child(icon(tool_icon(view.family), 13.0, theme::INK_MUTED))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(view.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(px(theme::META))
                    .text_color(rgb(stroke))
                    .child(view.title.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(match view.status {
                        ToolStatus::Error => theme::INK,
                        _ => theme::INK_MUTED,
                    }))
                    .child(tool_suffix(view)),
            );

        if !open {
            return row;
        }
        let body = tool_body(view);
        if body.is_empty() {
            return row;
        }

        // 结果可能有上千行：展开区再限一次行数，避免把视口冲垮。
        const MAX_LINES: usize = 24;
        let lines: Vec<&str> = body.lines().collect();
        let shown = if lines.len() > MAX_LINES {
            format!(
                "{}\n…（共 {} 行，只显示前 {MAX_LINES} 行）",
                lines[..MAX_LINES].join("\n"),
                lines.len()
            )
        } else {
            body
        };

        div().flex().flex_col().gap_1().child(row).child(
            div()
                .id(ElementId::Name(format!("tool-detail:{}", view.id).into()))
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

fn render_row(row: &TimelineRow) -> AnyElement {
    match row {
        TimelineRow::User { text, .. } => div()
            .flex()
            .flex_row()
            .gap_2()
            .text_size(px(theme::BODY))
            .child(div().text_color(rgb(theme::INK_MUTED)).child("›"))
            .child(div().child(text.clone()))
            .into_any_element(),
        TimelineRow::Assistant { id, text, .. } => {
            crate::markdown::text(id.clone(), text, TextRole::Assistant).into_any_element()
        }
        TimelineRow::Reasoning { id, text, .. } => {
            crate::markdown::text(id.clone(), text, TextRole::Reasoning).into_any_element()
        }
        TimelineRow::Notice { text, .. } => div()
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child(text.clone())
            .into_any_element(),
        // 工具行走 `Chat::tool_group`（要拿 `cx` 接点击），不会到这里。
        TimelineRow::Tool(view) => div()
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child(view.name.clone())
            .into_any_element(),
    }
}

/// 组头文案：一行类别统计；运行中按档位换成最后一条工具的实时摘要。
fn group_label(policy: RenderPolicy, rows: &[TimelineRow], running: bool) -> String {
    let tools: Vec<&ToolView> = rows
        .iter()
        .filter_map(|row| match row {
            TimelineRow::Tool(view) => Some(view),
            _ => None,
        })
        .collect();
    if tools.is_empty() {
        return if running { "思考中" } else { "已思考" }.to_owned();
    }
    if policy.show_live_summary(running)
        && let Some(last) = tools.last()
        && !last.title.is_empty()
    {
        return last.title.clone();
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for tool in &tools {
        let kind = tool.family.category();
        match counts.iter_mut().find(|(name, _)| *name == kind) {
            Some(entry) => entry.1 += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    counts.truncate(3);
    counts
        .into_iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 行的稳定渲染身份；过程组用它做会话内开合键。
fn row_render_id(row: &TimelineRow) -> RenderId {
    match row {
        TimelineRow::User { id, .. }
        | TimelineRow::Assistant { id, .. }
        | TimelineRow::Reasoning { id, .. }
        | TimelineRow::Notice { id, .. } => id.clone(),
        TimelineRow::Tool(view) => RenderId::Tool(view.id.clone()),
    }
}

/// 连续过程行的区间。assistant 正文 / 用户输入 / 提示都会断开过程组。
fn process_runs(rows: &[TimelineRow]) -> Vec<std::ops::Range<usize>> {
    let is_process =
        |row: &TimelineRow| matches!(row, TimelineRow::Reasoning { .. } | TimelineRow::Tool(_));
    let mut runs = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        if !is_process(&rows[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && is_process(&rows[index]) {
            index += 1;
        }
        runs.push(start..index);
    }
    runs
}

/// 这一段过程是否属于正在跑的 Turn。
///
/// 我们还没有完整的 turn/tail 模型，所以用封闭可观察信号：会话忙 + 本段有
/// 流式 reasoning 或未收尾工具。历史段在忙时也不会被误判成运行中。
fn process_group_running(rows: &[TimelineRow], session_busy: bool) -> bool {
    session_busy
        && rows.iter().any(|row| match row {
            TimelineRow::Reasoning { streaming, .. } => *streaming,
            TimelineRow::Tool(view) => view.status == ToolStatus::Running,
            _ => false,
        })
}

/// reasoning 折叠态的一行摘要。
///
/// 流式取最后一行非空文本（首行往往是开场白，信息量最低），已落定取首行；
/// Low 档两种都不给摘要。正文始终是纯文本，不解析 markdown。
fn reasoning_summary(text: &str, streaming: bool, policy: RenderPolicy) -> String {
    if !policy.show_reasoning_summary() {
        return String::new();
    }
    let line = if streaming {
        text.lines().rev().find(|line| !line.trim().is_empty())
    } else {
        text.lines().find(|line| !line.trim().is_empty())
    };
    line.map(|line| line.trim().to_owned()).unwrap_or_default()
}

/// 展开区正文：审计字段 + 工具结果，永远是纯文本，不解析 markdown。
fn tool_body(view: &ToolView) -> String {
    let mut lines: Vec<String> = view
        .details
        .iter()
        .map(|detail| format!("{}: {}", detail.label, detail.value))
        .collect();
    // 成功的文件读/写输出是噪声（ZCode 同样不渲染），错误与 shell/搜索类仍保留。
    let show_text_output = match view.status {
        ToolStatus::Error => true,
        ToolStatus::Ok => !matches!(view.family, ToolFamily::FileRead | ToolFamily::FileWrite),
        ToolStatus::Running => false,
    };
    match &view.result {
        ToolResultView::None => {}
        ToolResultView::Text { text, truncated } => {
            if show_text_output {
                lines.push(text.clone());
                if *truncated {
                    lines.push("（输出过长，只显示前一段）".to_owned());
                }
            }
        }
        ToolResultView::Diff {
            path,
            added,
            removed,
            preview,
        } => {
            if !path.is_empty() {
                lines.push(path.clone());
            }
            lines.push(format!("+{added} -{removed}"));
            lines.push(preview.clone());
        }
        ToolResultView::Error { text } => lines.push(format!("失败: {text}")),
        ToolResultView::Preview { title, text } => {
            lines.push(title.clone());
            lines.push(text.clone());
        }
    }
    lines.join("\n").trim().to_owned()
}

/// 工具行前缀图标（Lucide）；映射家族而非工具名，未知工具也能稳定归类。
fn tool_icon(family: ToolFamily) -> IconName {
    match family {
        ToolFamily::Shell => IconName::SquareTerminal,
        ToolFamily::FileRead => IconName::FileText,
        ToolFamily::FileWrite => IconName::Pencil,
        ToolFamily::Search => IconName::Search,
        ToolFamily::Web => IconName::Globe,
        _ => IconName::Wrench,
    }
}

/// 行尾后缀：状态 + 输出长度 + 可选耗时。
fn tool_suffix(view: &ToolView) -> String {
    let mut parts: Vec<String> = Vec::new();
    match view.status {
        ToolStatus::Running => parts.push("运行中".to_owned()),
        ToolStatus::Error => parts.push("失败".to_owned()),
        ToolStatus::Ok => {}
    }
    if view.output_chars > 0 {
        parts.push(format!("{} 字", view.output_chars));
    }
    if let Some(ms) = view.elapsed_ms {
        parts.push(if ms >= 1000 {
            format!("{:.1}s", ms as f64 / 1000.0)
        } else {
            format!("{ms}ms")
        });
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
/// 行内确认的种类：决定「取消」清哪个 pending、「确认」发哪条命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmKind {
    /// 移除工作区注册。
    WorkspaceDelete,
    /// 归档一条会话。
    SessionArchive,
    /// 从本客户端移除一条归档会话。
    SessionDelete,
}

impl ConfirmKind {
    /// 确认胶囊上的动词。
    fn verb(self) -> &'static str {
        match self {
            ConfirmKind::WorkspaceDelete => "移除",
            ConfirmKind::SessionArchive => "归档",
            ConfirmKind::SessionDelete => "删除",
        }
    }
}

/// 侧栏的一个分组：一个已注册工作区。
struct SideGroup<'a> {
    /// 展开状态与行 id 的键（工作区用 path）。
    key: String,
    /// 显示名。
    label: String,
    /// 工作区 id。
    workspace_id: String,
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

/// 纯计算侧栏分组，便于单测。返回 `(key, label, workspace_id, [session_id])`。
///
/// 归属真源是账本（host 的 `session_ids` + 本地历史覆盖）。侧栏只显示工作区：
/// 隐藏工作区的会话连同整组消失；账本里没有归属的历史会话也不在这里出现
/// （它们仍在 host 上，可通过搜索访问）。
fn group_plan(
    sessions: &[SessionInfo],
    registry: &WorkspaceRegistry,
    prefs: &SidebarPrefs,
    active_session: &str,
) -> Vec<(String, String, String, Vec<String>)> {
    // session id -> workspace id；隐藏组的成员单独记，直接跳过。
    let mut owner: HashMap<&str, &str> = HashMap::new();
    let mut hidden_ids: HashSet<&str> = HashSet::new();
    for workspace in &registry.items {
        let hidden = prefs.hidden_workspace_ids.contains(&workspace.workspace_id);
        let ledger = workspace.session_ids.iter().map(String::as_str);
        let adopted = prefs
            .adopted
            .get(&workspace.workspace_id)
            .map(|ids| ids.iter().map(String::as_str))
            .into_iter()
            .flatten();
        for id in ledger.chain(adopted) {
            if hidden {
                hidden_ids.insert(id);
            } else {
                owner.insert(id, workspace.workspace_id.as_str());
            }
        }
    }

    let mut buckets: HashMap<&str, Vec<&SessionInfo>> = HashMap::new();
    for session in sessions.iter().filter(|session| {
        (!session.blank || session.id == active_session)
            && !registry.is_archived(&session.id)
            && !prefs.deleted_session_ids.contains(&session.id)
    }) {
        if hidden_ids.contains(session.id.as_str()) {
            continue;
        }
        if let Some(workspace_id) = owner.get(session.id.as_str()) {
            buckets.entry(workspace_id).or_default().push(session);
        }
    }

    let mut plan = Vec::new();
    for workspace in &registry.items {
        if prefs.hidden_workspace_ids.contains(&workspace.workspace_id) {
            continue;
        }
        let mut items = buckets
            .remove(workspace.workspace_id.as_str())
            .unwrap_or_default();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        plan.push((
            workspace.path.clone(),
            workspace.title.clone(),
            workspace.workspace_id.clone(),
            items.into_iter().map(|item| item.id.clone()).collect(),
        ));
    }
    plan
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

/// 侧栏底部的 host 状态点：文案与色调只由生命周期枚举决定。
fn host_status(state: HostLifecycle) -> (u32, &'static str) {
    match state {
        HostLifecycle::Starting => (theme::INK_MUTED, "host 启动中"),
        HostLifecycle::Ready => (theme::OK, "dsh host"),
        HostLifecycle::Sleeping => (theme::INK_MUTED, "host 休眠中"),
        HostLifecycle::Failed => (theme::INK_MUTED, "host 启动失败"),
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
        ChatVerbosity, ModelGroup, PullRow, SelectedModel, format_tokens, group_label, group_plan,
        host_status, leaf, model_menu_rows, process_group_running, process_runs, reasoning_summary,
        short_id, tool_body,
    };
    use crate::{HostLifecycle, ModelOption, SessionInfo, SidebarPrefs};
    use dsh_protocol::workspace::{WorkspaceRegistry, WorkspaceView};
    use dsh_protocol::{TimelineRow, ToolFamily, ToolResultView, ToolView};
    use std::collections::{HashMap, HashSet};

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
    fn host_status_covers_every_lifecycle_state() {
        assert_eq!(host_status(HostLifecycle::Starting).1, "host 启动中");
        assert_eq!(host_status(HostLifecycle::Ready).1, "dsh host");
        assert_eq!(host_status(HostLifecycle::Sleeping).1, "host 休眠中");
        assert_eq!(host_status(HostLifecycle::Failed).1, "host 启动失败");
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

    fn tool(id: &str) -> TimelineRow {
        TimelineRow::Tool(ToolView {
            id: id.to_owned(),
            name: "bash".to_owned(),
            family: ToolFamily::Shell,
            title: "ls".to_owned(),
            summary: "ls".to_owned(),
            details: Vec::new(),
            status: dsh_protocol::transcript::ToolStatus::Ok,
            output_chars: 0,
            elapsed_ms: None,
            result: ToolResultView::None,
        })
    }

    fn text(body: &str) -> TimelineRow {
        TimelineRow::Assistant {
            id: dsh_protocol::RenderId::Assistant {
                id: dsh_protocol::MessageId::Turn { turn: 1, step: 1 },
                index: 0,
            },
            text: body.to_owned(),
            streaming: false,
        }
    }

    fn reasoning(body: &str) -> TimelineRow {
        TimelineRow::Reasoning {
            id: dsh_protocol::RenderId::Reasoning {
                id: dsh_protocol::MessageId::Turn { turn: 1, step: 1 },
                index: 0,
            },
            text: body.to_owned(),
            streaming: false,
        }
    }

    /// 只有**相邻**的过程行才归一组：中间夹助手正文就断开。
    #[test]
    fn process_runs_break_on_assistant_text() {
        let entries = vec![text("a"), tool("t1"), tool("t2"), text("b"), tool("t3")];
        let runs = process_runs(&entries);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], 1..3);
        assert_eq!(runs[1], 4..5);
    }

    #[test]
    fn process_runs_include_reasoning_and_tools() {
        let entries = vec![text("a"), reasoning("think"), tool("t1"), text("b")];
        let runs = process_runs(&entries);
        assert_eq!(runs, vec![1..3]);
    }

    #[test]
    fn process_runs_are_empty_without_process_rows() {
        assert!(process_runs(&[text("a")]).is_empty());
        assert!(process_runs(&[]).is_empty());
    }

    #[test]
    fn only_live_process_group_counts_as_running() {
        let mut live = tool("t1");
        let TimelineRow::Tool(ref mut view) = live else {
            unreachable!()
        };
        view.status = dsh_protocol::transcript::ToolStatus::Running;
        let settled = tool("t2");
        assert!(process_group_running(&[live], true));
        assert!(!process_group_running(&[settled], true));
        assert!(!process_group_running(&[tool("t3")], false));
    }

    #[test]
    fn reasoning_summary_follows_verbosity_and_stream_state() {
        let low = ChatVerbosity::Low.policy();
        let medium = ChatVerbosity::Medium.policy();
        assert_eq!(reasoning_summary("第一行\n第二行", false, low), "");
        assert_eq!(reasoning_summary("第一行\n第二行", false, medium), "第一行");
        // 流式取最后一行非空文本，而不是开场白。
        assert_eq!(reasoning_summary("第一行\n第二行", true, medium), "第二行");
        assert_eq!(reasoning_summary("第一行", true, medium), "第一行");
        assert_eq!(reasoning_summary("第一行\n第二行", true, low), "");
    }

    #[test]
    fn verbosity_controls_group_body_by_default() {
        assert!(!ChatVerbosity::Low.policy().group_body_open(true));
        assert!(ChatVerbosity::Medium.policy().group_body_open(true));
        assert!(!ChatVerbosity::Medium.policy().group_body_open(false));
        assert!(ChatVerbosity::High.policy().group_body_open(true));
        assert!(ChatVerbosity::High.policy().group_body_open(false));
    }

    /// 组头把 N 条工具压成一行分类统计，运行中高亮最后一条的摘要。
    #[test]
    fn group_label_compresses_tools_into_one_line() {
        let mut third = tool("t3");
        if let TimelineRow::Tool(view) = &mut third {
            view.family = ToolFamily::FileWrite;
            view.title = "编辑 src/main.rs".to_owned();
            view.status = dsh_protocol::transcript::ToolStatus::Running;
        }
        let rows = vec![tool("t1"), tool("t2"), third];
        let low = group_label(ChatVerbosity::Low.policy(), &rows, true);
        assert!(low.contains("命令 2"), "低档运行中仍给类别统计：{low}");

        let medium = group_label(ChatVerbosity::Medium.policy(), &rows, true);
        assert_eq!(medium, "编辑 src/main.rs");

        let settled = group_label(ChatVerbosity::Medium.policy(), &rows, false);
        assert!(settled.contains("编辑 1"), "分类统计带数量：{settled}");
    }

    /// 成功的文件读输出不铺进展开区；shell 成功与失败仍保留输出。
    #[test]
    fn tool_body_filters_successful_file_output() {
        let mut read = tool("r1");
        if let TimelineRow::Tool(view) = &mut read {
            view.family = ToolFamily::FileRead;
            view.result = ToolResultView::Text {
                text: "大段文件内容".to_owned(),
                truncated: false,
            };
        }
        let TimelineRow::Tool(read_view) = &read else {
            unreachable!()
        };
        assert!(!tool_body(read_view).contains("大段文件内容"));

        let mut shell = tool("r2");
        if let TimelineRow::Tool(view) = &mut shell {
            view.result = ToolResultView::Text {
                text: "命令输出".to_owned(),
                truncated: false,
            };
        }
        let TimelineRow::Tool(shell_view) = &shell else {
            unreachable!()
        };
        assert!(tool_body(shell_view).contains("命令输出"));
    }

    fn session(id: &str, cwd: &str, updated_at: i64) -> SessionInfo {
        SessionInfo {
            id: id.to_owned(),
            title: None,
            cwd: cwd.to_owned(),
            updated_at,
            running: false,
            blank: false,
            mode: None,
            model: None,
        }
    }

    fn workspace(id: &str, path: &str, title: &str, session_ids: &[&str]) -> WorkspaceView {
        WorkspaceView {
            workspace_id: id.to_owned(),
            path: path.to_owned(),
            title: title.to_owned(),
            session_ids: session_ids.iter().map(|id| (*id).to_owned()).collect(),
            created_at: None,
            updated_at: None,
        }
    }

    /// 归属走账本，不是 cwd：cwd 对不上也不串组。
    #[test]
    fn ledger_beats_cwd() {
        let sessions = vec![session("s1", "/b", 2), session("s2", "/a", 1)];
        let registry = WorkspaceRegistry {
            items: vec![
                workspace("w1", "/a", "A", &["s1"]),
                workspace("w2", "/b", "B", &["s2"]),
            ],
            ..Default::default()
        };
        let plan = group_plan(&sessions, &registry, &SidebarPrefs::default(), "");
        assert_eq!(plan[0].3, vec!["s1"]);
        assert_eq!(plan[1].3, vec!["s2"]);
        assert_eq!(plan.len(), 2);
    }

    /// 侧栏没有「未分组」：隐藏工作区的会话与无归属会话都不显示。
    #[test]
    fn hidden_and_unowned_sessions_are_not_shown() {
        let sessions = vec![session("s1", "/a", 2), session("s2", "/other", 1)];
        let registry = WorkspaceRegistry {
            items: vec![workspace("w1", "/a", "A", &["s1"])],
            ..Default::default()
        };
        let prefs = SidebarPrefs {
            hidden_workspace_ids: HashSet::from(["w1".to_owned()]),
            ..Default::default()
        };
        let plan = group_plan(&sessions, &registry, &prefs, "");
        assert!(plan.is_empty());
    }

    /// 历史会话靠本地覆盖回到原工作区。
    #[test]
    fn adopted_sessions_join_their_workspace() {
        let sessions = vec![session("s1", "/a", 1)];
        let registry = WorkspaceRegistry {
            items: vec![workspace("w1", "/a", "A", &[])],
            ..Default::default()
        };
        let prefs = SidebarPrefs {
            adopted: HashMap::from([("w1".to_owned(), vec!["s1".to_owned()])]),
            ..Default::default()
        };
        let plan = group_plan(&sessions, &registry, &prefs, "");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].2.as_str(), "w1");
        assert_eq!(plan[0].3, vec!["s1"]);
    }

    /// 已注册但零会话的工作区仍显示行。
    #[test]
    fn empty_workspace_row_stays_without_ungrouped() {
        let registry = WorkspaceRegistry {
            items: vec![workspace("w1", "/a", "A", &[])],
            ..Default::default()
        };
        let plan = group_plan(&[], &registry, &SidebarPrefs::default(), "");
        assert_eq!(plan.len(), 1);
        assert!(plan[0].3.is_empty());
    }

    /// 归档与非当前空白会话都不进树。
    #[test]
    fn archived_and_inactive_blank_are_excluded() {
        let mut blank = session("s2", "/a", 3);
        blank.blank = true;
        let sessions = vec![session("s1", "/a", 2), blank, session("s3", "/other", 1)];
        let registry = WorkspaceRegistry {
            items: vec![workspace("w1", "/a", "A", &["s1"])],
            archived_session_ids: vec!["s3".to_owned()],
            ..Default::default()
        };
        let plan = group_plan(&sessions, &registry, &SidebarPrefs::default(), "");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].3, vec!["s1"]);
    }

    /// 从本机移除的会话不再出现在侧栏，即使 host 账本仍记着它。
    #[test]
    fn locally_deleted_sessions_are_excluded() {
        let sessions = vec![session("s1", "/a", 2), session("s2", "/a", 1)];
        let registry = WorkspaceRegistry {
            items: vec![workspace("w1", "/a", "A", &["s1", "s2"])],
            ..Default::default()
        };
        let prefs = SidebarPrefs {
            deleted_session_ids: HashSet::from(["s2".to_owned()]),
            ..Default::default()
        };
        let plan = group_plan(&sessions, &registry, &prefs, "");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].3, vec!["s1"]);
    }
}
