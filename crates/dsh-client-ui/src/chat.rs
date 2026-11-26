//! 主视图：侧栏 + 主区（标题栏 / 时间线 / composer）。
//!
//! 几何全部取自 Ardot 设计稿 `747373011992944`：侧栏 264、内容列 860、
//! 顶/底栏 52、主区水平内边距 28、侧栏内边距 16。
//!
//! 数据流是单向的：外部把 [`UiEvent`] 推进 channel，本视图消费后更新状态，
//! 用户输入通过 [`UiCommand`] 推回去。视图内**不做网络**。

use dsh_protocol::workspace::WorkspaceRegistry;
use gpui_kit::base::input::{InputEditorStyle, InputEvent, Textarea, TextareaState};
use gpui_kit::base::slider::{
    Slider, SliderEvent, SliderIndicator, SliderState, SliderThumb, SliderTrack,
};
use gpui_kit::base::{Tab, Tabs};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::completion;
use crate::markdown::TextRole;
use crate::settings::{ChatVerbosity, Settings};
use crate::theme;
use crate::{
    CompletionItem, CtxMeter, DiscoveredChoice, EffortOption, HostLifecycle, ModelGroup,
    PresetInfo, ProviderSetting, SelectedModel, SessionInfo, SidebarPrefs, UiCommand, UiEvent,
};
use dsh_protocol::context::Occupancy;
use dsh_protocol::presentation::{Detail, FoldSpec, Overrides, RenderOp, plan};
use dsh_protocol::transcript::ToolStatus;
use dsh_protocol::{ChatTimeline, RenderId, TimelineRow, ToolFamily, ToolResultView, ToolView};
use gpui_kit::assets::IconName;

/// 设置模式的三个应用级视图：通用（对话详略）、模型、归档会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsTab {
    #[default]
    General,
    Models,
    Archive,
}

impl SettingsTab {
    /// 侧栏导航的分组表：组名不可点，组内条目才是 tab。空分组不显示组名。
    const SECTIONS: [(&'static str, &'static [Self]); 2] = [
        ("通用", &[Self::General, Self::Models]),
        ("数据", &[Self::Archive]),
    ];

    /// 稳定键，只用于控件 id。
    const fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Models => "models",
            Self::Archive => "archive",
        }
    }

    /// 导航条目的标签。
    const fn label(self) -> &'static str {
        match self {
            Self::General => "常规",
            Self::Models => "模型",
            Self::Archive => "归档",
        }
    }

    /// 导航条目的图标；只用默认资源包里内嵌的 Lucide 名字。
    const fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Settings2,
            Self::Models => IconName::Cpu,
            Self::Archive => IconName::Inbox,
        }
    }
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
    /// 设置模式当前选中的叶子（通用→常规 / 模型 / 数据→归档）。
    settings_tab: SettingsTab,
    /// 「常规」页的详略滑块。四档是四个整数步，拖动只改它的值；
    /// 写 host 由 `SliderEvent::Release` 触发，与旧的分段按钮同一路径。
    verbosity_slider: Entity<SliderState>,
    /// 权威值（启动读取 / 写回回送 / 失败回滚）与滑块当前值不一致时记在这里，
    /// 下一次渲染时推给滑块：`set_value` 需要 `Window`，而事件处理没有。
    verbosity_slider_sync: Option<ChatVerbosity>,
    /// 模型设置页：可配置 provider 的快照。
    model_settings: Vec<ProviderSetting>,
    /// 模型设置页读取失败说明；成功为 `None`。
    model_settings_error: Option<String>,
    /// 模型设置页 profile 是否可写。
    model_settings_writable: bool,
    /// 添加 provider 表单是否展开。
    model_add_open: bool,
    /// 表单字段：路由 id / 显示名 / 网关地址 / API key / 初始模型。
    model_form_route: Entity<TextareaState>,
    model_form_name: Entity<TextareaState>,
    model_form_url: Entity<TextareaState>,
    model_form_key: Entity<TextareaState>,
    model_form_models: Entity<TextareaState>,
    /// 正在给哪个 provider 追加模型（Some = 展开模型输入行）。
    model_add_model_for: Option<String>,
    /// 追加模型的输入（共享一个）。
    model_add_model_input: Entity<TextareaState>,
    /// 线协议家族（下拉用简单按钮轮转）。
    model_form_api: String,
    /// 探活请求序号与最近一次结果。
    model_discover_seq: u64,
    model_discover_choices: Vec<DiscoveredChoice>,
    model_discover_error: Option<String>,
    /// 所有折叠容器的会话内交互状态：语义键 -> 手动开合。取代原先三个
    /// `HashMap<String, bool>`；键与渲染位置无关，分页 / 流式追加不漂移。
    overrides: Overrides,
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
    /// 详略滑块的订阅；同上，丢掉就收不到 `Release`。
    _slider: Subscription,
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

/// 设置页表单用的单行输入：共享深色样式，Enter 不提交（提交走明确的按钮）。
fn form_input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<Chat>,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        let mut state = TextareaState::new(window, cx)
            .placeholder(placeholder)
            .submit_on_enter(false);
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
    })
}

impl Chat {
    /// 清空添加 provider 表单的明文输入（API key 不留驻 UI 状态）。
    fn clear_model_form(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_form_route
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model_form_name
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model_form_url
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model_form_key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model_form_models
            .update(cx, |state, cx| state.set_value("", window, cx));
    }

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

        // 模型设置页的 add-provider 表单输入。构造时不聚焦，避免抢 composer 的光标。
        let model_form_route = form_input("路由 id，如 acme-gateway", window, cx);
        let model_form_name = form_input("显示名（可留空）", window, cx);
        let model_form_url = form_input("https://gateway.example/v1", window, cx);
        let model_form_key = form_input("API key（可留空，走环境变量）", window, cx);
        let model_form_models = form_input("初始模型 id，多个用逗号分隔", window, cx);
        let model_add_model_input = form_input("模型 id，多个用逗号分隔", window, cx);

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

        // 详略滑块：四档就是 0..=3 的整数步，默认值与 `Settings::default()` 一致。
        // 拖动只改滑块值；松手（Release）才写 host，避免拖动途中连发 mutation。
        let verbosity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max((ChatVerbosity::ALL.len() - 1) as f32)
                .step(1.)
                .default_value(ChatVerbosity::default().index() as f32)
        });
        let slider = cx.subscribe(
            &verbosity_slider,
            |chat: &mut Self, _slider, event: &SliderEvent, cx| match event {
                // 拖动途中只重画拇指与填充条：值已经写进滑块状态，但本视图不知道。
                SliderEvent::Change(_) => cx.notify(),
                // 松手才是一次完整的用户意图，这一步写 host。
                SliderEvent::Release(value) => {
                    chat.commit_verbosity(ChatVerbosity::from_index(value.end() as usize), cx);
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
            verbosity_slider,
            verbosity_slider_sync: None,
            model_settings: Vec::new(),
            model_settings_error: None,
            model_settings_writable: false,
            model_add_open: false,
            model_form_route,
            model_form_name,
            model_form_url,
            model_form_key,
            model_form_models,
            model_add_model_for: None,
            model_add_model_input,
            model_form_api: "openai-completions".to_owned(),
            model_discover_seq: 0,
            model_discover_choices: Vec::new(),
            model_discover_error: None,
            overrides: Overrides {
                force_open: std::env::var("DSH_DEBUG_TOOL_OPEN").is_ok(),
                ..Overrides::default()
            },
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
            _slider: slider,
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
                // 会话身份变化（新建 / 重连）：折叠覆盖的键只在单个会话内唯一，
                // 必须清掉，否则上一会话的展开/折叠会命中新会话的同构轮次。
                self.overrides.folds.clear();
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
            UiEvent::ModelSettings {
                providers,
                writable,
                error,
            } => {
                self.model_settings = providers;
                self.model_settings_writable = writable;
                self.model_settings_error = error;
            }
            UiEvent::ModelDiscovery {
                seq,
                choices,
                error,
            } => {
                // 丢掉过期结果：只有最新一次探活才配得上当前表单。
                if seq == self.model_discover_seq {
                    self.model_discover_choices = choices;
                    self.model_discover_error = error;
                }
            }
            UiEvent::Transcript(items) => {
                // 快照是「请求回放那一刻」的样子。刚发出上一轮时它会晚到、内容比界面旧：
                // 那条乐观气泡和进行中的一轮都会被抹掉，所以忙着一轮时直接丢掉。
                if self.phase.is_busy() {
                    return;
                }
                self.timeline.replace_snapshot(items);
                // 语义键在 id 已权威时稳定；乐观 id（msg:local:N）刚被权威 id 替换的
                // 那一轮，其覆盖会自然失效——无害，且不会误折叠别一轮。
                // 跨会话泄漏由切会话处的 overrides.folds.clear() 兜底。
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

    /// 设置导航：一层平铺，分组标题不可点，组内条才是 tab。
    ///
    /// 结构对齐官方 settings 侧栏（`Agent 能力` / `数据与统计` 那种小节标题 +
    /// 组内条目），但不做折叠：条目数量少，折叠只是多一次点击。
    fn settings_side_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let back = cx.entity();
        let mut tabs = Tabs::new("settings-nav").axis(Axis::Vertical);
        for (index, (section, entries)) in SettingsTab::SECTIONS.iter().enumerate() {
            if entries.is_empty() {
                continue;
            }
            tabs = tabs.child(self.settings_nav_section(section, index == 0));
            for tab in entries.iter().copied() {
                tabs = tabs.child(self.settings_nav_tab(tab, cx));
            }
        }

        div()
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
            .child(div().mt(px(8.)).child(tabs.flex().flex_col().w_full()))
    }

    /// 分组标题：小字、弱墨、不可点。`first` 只用来决定要不要留上间距。
    fn settings_nav_section(&self, label: &'static str, first: bool) -> impl IntoElement {
        div()
            .when(!first, |this| this.mt(px(14.)))
            .px(px(8.))
            .py(px(6.))
            .text_size(px(theme::META))
            .text_color(rgb(theme::INK_MUTED))
            .child(label)
    }

    /// 一个导航条目：图标 + 标签，选中给底色与一级墨色。
    fn settings_nav_tab(&self, tab: SettingsTab, cx: &mut Context<Self>) -> impl IntoElement {
        let select = cx.entity();
        let selected = self.settings_tab == tab;
        Tab::new(ElementId::Name(
            format!("settings-nav:{}", tab.key()).into(),
        ))
        .selected(selected)
        .accessibility_label(tab.label())
        .on_click(move |_, _, cx| {
            select.update(cx, |chat, cx| {
                if chat.settings_tab != tab {
                    chat.settings_tab = tab;
                    cx.notify();
                }
            });
        })
        .flex()
        .flex_row()
        .justify_start()
        .gap_2()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .text_size(px(theme::UI))
        .bg(rgb(if selected {
            theme::BG_ACTIVE
        } else {
            theme::BG_SIDEBAR
        }))
        .text_color(rgb(if selected {
            theme::INK
        } else {
            theme::INK_SOFT
        }))
        .hover(|style| style.bg(rgb(theme::BG_HOVER)))
        .cursor_pointer()
        .child(icon(
            tab.icon(),
            14.0,
            if selected {
                theme::INK
            } else {
                theme::INK_MUTED
            },
        ))
        .child(div().child(tab.label()))
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
                        // 折叠覆盖的键（turn/step/callId）只在单个会话内唯一：切会话
                        // 必须清掉，否则 A 会话展开/折叠的轮会泄漏到 B 会话的同构轮次。
                        chat.overrides.folds.clear();
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

    /// 「通用 → 常规」页：对话详略一个四档滑块。
    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                                    .child("对话"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::META))
                                    .text_color(rgb(theme::INK_MUTED))
                                    .child("全局详略：每升一档多展开一层容器（整轮 → 过程组 → 正文）"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p(px(14.))
                            .rounded(px(10.))
                            .bg(rgb(theme::BG_CARD))
                            .border_1()
                            .border_color(rgb(theme::BG_CARD_EDGE))
                            .child(self.verbosity_slider(cx))
                            .child(self.verbosity_scale())
                            .child(
                                div()
                                    .text_size(px(theme::META))
                                    .text_color(rgb(theme::INK_SOFT))
                                    .child(self.settings.verbosity.description()),
                            ),
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

    /// 详略滑块本体。
    ///
    /// `gpui-base` 的滑块是无样式的：`Slider` 只管交互，轨道 / 填充 / 拇指由
    /// 调用方画。三条约定必须同时成立，否则拖不动或松手不写：
    ///
    /// 1. 拇指是 `Slider` 的子元素——松手（`Release`）由 `Slider` 上的 mouse-up
    ///    监听发出，拇指放在 `Slider` 之外就收不到，`dragging` 会一直挂着。
    /// 2. `SliderIndicator` 必须是横跨整条轨道的那个元素：它把自己的 bounds 记进
    ///    状态，值↔位置的换算全靠这份宽度；填充条只能是它的子元素。
    /// 3. 左右各留 7px（拇指半径），两端拇指才不越出可视区。
    fn verbosity_slider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = &self.verbosity_slider;
        let fraction = state.read(cx).percentage().end;
        div().relative().w_full().h(px(22.)).px(px(7.)).child(
            Slider::new(state)
                .size_full()
                .child(
                    SliderTrack::new(state).size_full().child(
                        SliderIndicator::new(state)
                            .relative()
                            .mt(px(9.))
                            .w_full()
                            .h(px(4.))
                            .rounded_full()
                            .bg(rgb(theme::BG_CARD_EDGE))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(rgb(theme::INK_SOFT))
                                    .w(relative(fraction)),
                            ),
                    ),
                )
                .child(
                    SliderThumb::new(state)
                        .absolute()
                        .top(px(4.))
                        .left(relative(fraction))
                        .ml(px(-7.))
                        .w(px(14.))
                        .h(px(14.))
                        .rounded_full()
                        .bg(rgb(theme::INK))
                        .border_2()
                        .border_color(rgb(theme::BG_CARD)),
                ),
        )
    }

    /// 四档刻度标签：当前档用一级墨色，其余压到弱墨。只读，不做点击。
    fn verbosity_scale(&self) -> impl IntoElement {
        let mut scale = div().flex().flex_row().w_full().gap_1();
        for verbosity in ChatVerbosity::ALL {
            let selected = self.settings.verbosity == verbosity;
            scale = scale.child(
                div()
                    .flex_1()
                    .text_size(px(theme::META))
                    .text_color(rgb(if selected {
                        theme::INK
                    } else {
                        theme::INK_MUTED
                    }))
                    .child(verbosity.label()),
            );
        }
        scale
    }

    /// 模型设置页：可配置 provider 列表 + 添加自定义网关表单。
    fn model_settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1();
        if self.model_settings.is_empty() {
            list = list.child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(if self.model_settings_error.is_some() {
                        "模型设置读取失败，见下方说明"
                    } else {
                        "没有可配置的 provider"
                    }),
            );
        }
        for provider in &self.model_settings {
            list = list.child(self.model_provider_row(provider, cx));
        }

        let add = if self.model_add_open {
            self.model_add_form(cx).into_any_element()
        } else {
            let toggle = cx.entity();
            div()
                .id("model-add-toggle")
                .flex()
                .flex_row()
                .items_center()
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(rgb(theme::BG_HOVER))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::BG_CARD_EDGE)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    toggle.update(cx, |chat: &mut Self, cx| {
                        chat.model_add_open = true;
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .text_size(px(theme::UI))
                        .text_color(rgb(theme::INK))
                        .child("＋ 添加 provider"),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(
            div()
                .id("model-settings-scroll")
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
                .gap_5()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(theme::BODY))
                                .text_color(rgb(theme::INK))
                                .child("模型"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::META))
                                .text_color(rgb(theme::INK_MUTED))
                                .child(
                                    "provider 与模型；密钥只写 .credentials.yaml，profile 只留引用",
                                ),
                        ),
                )
                .child(list)
                .child(self.model_add_model_row(cx))
                .child(add)
                .child(if let Some(error) = self.model_settings_error.as_deref() {
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_SOFT))
                        .child(format!("读取失败：{error}"))
                        .into_any_element()
                } else if !self.model_settings_writable {
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_SOFT))
                        .child("当前 profile 不接受设置写入")
                        .into_any_element()
                } else {
                    div().into_any_element()
                }),
        )
    }

    /// 模型设置页里的一行 provider。
    fn model_provider_row(
        &self,
        provider: &ProviderSetting,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let remove = cx.entity();
        let add_model = cx.entity();
        let route = provider.id.clone();
        let route_add = provider.id.clone();
        let mut detail = format!(
            "{} · ",
            if provider.declared {
                "已配置"
            } else {
                "可添加"
            }
        );
        detail.push_str(if provider.key_configured {
            provider.key_source.as_deref().unwrap_or("凭据已配置")
        } else {
            "无凭据"
        });
        if !provider.models.is_empty() {
            detail.push_str(&format!(" · {} 个模型", provider.models.len()));
        }
        div()
            .id(ElementId::Name(
                format!("model-provider:{}", provider.id).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px(px(12.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    .child(
                        div()
                            .text_size(px(theme::UI))
                            .text_color(rgb(theme::INK))
                            .child(provider.display_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_MUTED))
                            .child(detail),
                    ),
            )
            .child(if provider.declared {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("model-provider-add-model:{}", provider.id).into(),
                            ))
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(theme::INK)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                add_model.update(cx, |chat: &mut Self, cx| {
                                    chat.model_add_model_for = Some(route_add.clone());
                                    cx.notify();
                                });
                            })
                            .child("＋模型"),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("model-provider-remove:{}", provider.id).into(),
                            ))
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(theme::INK)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let _ =
                                    remove
                                        .read(cx)
                                        .commands
                                        .try_send(UiCommand::RemoveProvider {
                                            route: route.clone(),
                                        });
                            })
                            .child("移除"),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            })
    }

    /// 给已声明 provider 追加模型的内联输入行（无目标时不渲染）。
    fn model_add_model_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(route) = self.model_add_model_for.clone() else {
            return div().into_any_element();
        };
        let entity_submit = cx.entity();
        let entity_cancel = cx.entity();
        let sender = self.commands.clone();
        let input = self.model_add_model_input.clone();
        let route_submit = route.clone();
        div()
            .id("model-add-model-row")
            .flex()
            .flex_col()
            .gap_2()
            .p(px(12.))
            .rounded(px(6.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(format!("给 {route} 添加模型")),
            )
            .child(div().child(Textarea::new(&input)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("model-add-model-submit")
                            .text_size(px(theme::UI))
                            .text_color(rgb(theme::INK))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                let ids = input.read(cx).value().to_string();
                                if !ids.trim().is_empty() {
                                    let _ = sender.try_send(UiCommand::AddModel {
                                        route: route_submit.clone(),
                                        model_ids: ids,
                                    });
                                }
                                entity_submit.update(cx, |chat: &mut Self, cx| {
                                    chat.model_add_model_input
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    chat.model_add_model_for = None;
                                    cx.notify();
                                });
                            })
                            .child("添加"),
                    )
                    .child(
                        div()
                            .id("model-add-model-cancel")
                            .text_size(px(theme::UI))
                            .text_color(rgb(theme::INK_SOFT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(theme::INK)))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                entity_cancel.update(cx, |chat: &mut Self, cx| {
                                    chat.model_add_model_input
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    chat.model_add_model_for = None;
                                    cx.notify();
                                });
                            })
                            .child("取消"),
                    ),
            )
            .into_any_element()
    }

    /// 添加 provider 表单。字段从 4 个 TextareaState 读，提交走 UiCommand。
    fn model_add_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity_api = cx.entity();
        let entity_discover = cx.entity();
        let sender_probe = self.commands.clone();
        let route = self.model_form_route.clone();
        let name = self.model_form_name.clone();
        let url = self.model_form_url.clone();
        let url_probe = url.clone();
        let key = self.model_form_key.clone();
        let key_probe = key.clone();
        let models_input = self.model_form_models.clone();
        let api = self.model_form_api.clone();
        let api_probe = api.clone();
        let seq = self.model_discover_seq + 1;

        let field = |label: &'static str, input: Entity<TextareaState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK_SOFT))
                        .child(label),
                )
                .child(div().child(Textarea::new(&input)))
        };

        let mut form = div()
            .flex()
            .flex_col()
            .gap_3()
            .p(px(12.))
            .rounded(px(10.))
            .bg(rgb(theme::BG_CARD))
            .border_1()
            .border_color(rgb(theme::BG_CARD_EDGE))
            .child(field("路由 id", route.clone()))
            .child(field("显示名", name.clone()))
            .child(
                div().flex().flex_row().items_center().gap_2().child(
                    div()
                        .id("model-form-api")
                        .text_size(px(theme::META))
                        .text_color(rgb(theme::INK))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            entity_api.update(cx, |chat: &mut Self, cx| {
                                chat.model_form_api = chat.next_api();
                                cx.notify();
                            });
                        })
                        .child(format!("协议：{api} ⇄")),
                ),
            )
            .child(field("网关地址", url.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(field("API key", key.clone()).into_any_element())
                    .child(
                        div()
                            .id("model-form-discover")
                            .flex_none()
                            .text_size(px(theme::META))
                            .text_color(rgb(theme::INK_SOFT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(theme::INK)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                let base = url_probe.read(cx).value().to_string();
                                if base.is_empty() {
                                    return;
                                }
                                let _ = sender_probe.try_send(UiCommand::DiscoverModels {
                                    base_url: base,
                                    api: api_probe.clone(),
                                    api_key: key_probe.read(cx).value().to_string(),
                                    seq,
                                });
                                entity_discover.update(cx, |chat, cx| {
                                    chat.model_discover_seq = seq;
                                    chat.model_discover_choices.clear();
                                    chat.model_discover_error = None;
                                    cx.notify();
                                });
                            })
                            .child("探测模型"),
                    ),
            )
            .child(field(
                "初始模型 id（逗号分隔，可留空）",
                models_input.clone(),
            ));

        if let Some(error) = self.model_discover_error.as_deref() {
            form = form.child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_SOFT))
                    .child(format!("探测失败：{error}")),
            );
        } else if !self.model_discover_choices.is_empty() {
            let choices = self
                .model_discover_choices
                .iter()
                .take(40)
                .map(|choice| {
                    choice
                        .name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&choice.id)
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join(" · ");
            form = form.child(
                div()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(format!("可用模型：{choices}")),
            );
        }

        let submit = cx.entity();
        let cancel = cx.entity();
        let sender2 = self.commands.clone();
        let route2 = route.clone();
        let name2 = name.clone();
        let url2 = url.clone();
        let key2 = key.clone();
        let api2 = api.clone();
        let models2 = models_input.clone();
        form.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("model-form-submit")
                        .text_size(px(theme::UI))
                        .text_color(rgb(theme::INK))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .bg(rgb(theme::BG_HOVER))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(theme::BG_CARD_EDGE)))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            let values = (
                                route2.read(cx).value().to_string(),
                                name2.read(cx).value().to_string(),
                                api2.clone(),
                                url2.read(cx).value().to_string(),
                                key2.read(cx).value().to_string(),
                                models2.read(cx).value().to_string(),
                            );
                            let _ = sender2.try_send(UiCommand::AddProvider {
                                route: values.0,
                                display_name: values.1,
                                api: values.2,
                                base_url: values.3,
                                api_key: values.4,
                                model_ids: values.5,
                            });
                            submit.update(cx, |chat: &mut Self, cx| {
                                chat.clear_model_form(window, cx);
                                chat.model_add_open = false;
                                cx.notify();
                            });
                        })
                        .child("保存"),
                )
                .child(
                    div()
                        .id("model-form-cancel")
                        .text_size(px(theme::UI))
                        .text_color(rgb(theme::INK_SOFT))
                        .px(px(12.))
                        .py(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(theme::INK)))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cancel.update(cx, |chat: &mut Self, cx| {
                                chat.clear_model_form(window, cx);
                                chat.model_add_open = false;
                                cx.notify();
                            });
                        })
                        .child("取消"),
                ),
        )
    }

    /// 协议家族轮转（简单按钮切换到下一个）。
    fn next_api(&self) -> String {
        const APIS: [&str; 4] = [
            "openai-completions",
            "openai-responses",
            "anthropic-messages",
            "google-generative-ai",
        ];
        let index = APIS
            .iter()
            .position(|item| *item == self.model_form_api)
            .unwrap_or(0);
        APIS[(index + 1) % APIS.len()].to_owned()
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

    /// 应用档位只影响默认值；语义键上的手动覆盖保留，让新档位与用户选择共存。
    /// 真值由 host 回送，这里只更新内存镜像。
    ///
    /// 权威值变化时顺手记一次滑块同步：`set_value` 要 `Window`，而这里没有，
    /// 所以只挂标记，由 `render` 在下一帧推给滑块。
    fn apply_verbosity(&mut self, verbosity: ChatVerbosity) {
        self.settings.verbosity = verbosity;
        self.verbosity_slider_sync = Some(verbosity);
    }

    /// 滑块松手：先乐观画上，再把写命令交给 host；失败由回送的旧值回滚。
    fn commit_verbosity(&mut self, verbosity: ChatVerbosity, cx: &mut Context<Self>) {
        if verbosity == self.settings.verbosity {
            return;
        }
        let previous = self.settings.verbosity;
        let _ = self.commands.try_send(UiCommand::SetChatVerbosity {
            verbosity,
            previous,
        });
        self.settings.verbosity = verbosity;
        self.settings_error = None;
        self.settings_saved = false;
        cx.notify();
    }

    // ── 主区 ────────────────────────────────────────────────────────────

    fn main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 还没选工作区 / 没有会话：主区是空状态，不是聊天区。
        let body = if self.settings_open {
            match self.settings_tab {
                SettingsTab::General => self.settings_page(cx).into_any_element(),
                SettingsTab::Models => self.model_settings_page(cx).into_any_element(),
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

        // 设置模式 / 空状态：标题栏不摆会话面包屑，也不给无处的右栏开关。
        // 侧栏收起时 row 里仍有展开按钮（settings tree 在侧栏里）。
        if self.settings_open || self.active_session.is_empty() {
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

        // 行来自时间线投影；绘制结构由纯函数 `plan` 决定：档位只是一个展开
        // 深度标量，手动覆盖按语义键优先。切档 / 分页都不重建会话内容。
        let rows = self.timeline.rows();
        let detail = self.settings.verbosity.detail();
        let ops = plan(&rows, detail, &self.overrides, self.phase.is_busy());
        for op in &ops {
            column = column.child(self.render_op(op, cx));
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

    /// 把一条规划指令画成元素。整轮 / 过程组的开合交给 [`Self::fold_header`]。
    fn render_op(&self, op: &RenderOp, cx: &mut Context<Self>) -> AnyElement {
        match op {
            RenderOp::Row(row) => self.render_interactive_row(row, cx),
            RenderOp::Group { spec, rows } => self.render_group(spec, rows, cx).into_any_element(),
            RenderOp::Turn { spec, inner } => self.render_turn(spec, inner, cx).into_any_element(),
        }
    }

    /// 一行 reasoning / tool 走可点开版本（接点击）；其余走静态绘制。
    fn render_interactive_row(&self, row: &TimelineRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            TimelineRow::Reasoning {
                id,
                text,
                streaming,
            } => self.reasoning_row(id, text, *streaming, cx),
            TimelineRow::Tool(view) => self.tool_row(view, cx).into_any_element(),
            other => render_row(other),
        }
    }

    /// 整轮控件：控件头插在第一个过程组处；折叠时跳过所有过程组，只留豁免行。
    fn render_turn(&self, spec: &FoldSpec, inner: &[RenderOp], cx: &mut Context<Self>) -> Div {
        let mut col = div().flex().flex_col().gap_1();
        let mut header_drawn = false;
        for op in inner {
            if matches!(op, RenderOp::Group { .. }) {
                if !header_drawn {
                    col = col.child(self.fold_header(spec, cx));
                    header_drawn = true;
                }
                if spec.open {
                    col = col.child(self.render_op(op, cx));
                }
            } else {
                // 豁免行（用户输入 / 助手回复）：整轮折叠时仍可见。
                col = col.child(self.render_op(op, cx));
            }
        }
        if !header_drawn {
            col = col.child(self.fold_header(spec, cx));
        }
        col
    }

    /// 过程组：组头 + 可选成员行。
    fn render_group(&self, spec: &FoldSpec, rows: &[TimelineRow], cx: &mut Context<Self>) -> Div {
        let mut group = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.fold_header(spec, cx));
        if spec.open {
            let mut body = div().flex().flex_col().gap_1().pl(px(16.));
            for row in rows {
                body = body.child(self.render_interactive_row(row, cx));
            }
            group = group.child(body);
        }
        group
    }

    /// 一个折叠容器的头：chevron + 摘要；运行中 / 失败的容器不可收起。
    fn fold_header(&self, spec: &FoldSpec, cx: &mut Context<Self>) -> Div {
        let toggle = cx.entity();
        let key = spec.key.clone();
        let open = spec.open;
        let summary = spec.summary.clone();
        let mut header = div().flex().flex_row().items_center().gap_2().py(px(2.));
        if spec.collapsible {
            header = header
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::BG_HOVER)))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    toggle.update(cx, |chat, cx| {
                        // spec.open 已含「档位默认 + 现有覆盖」的最终结果，翻转即可。
                        chat.overrides.set(key.clone(), !open);
                        cx.notify();
                    });
                });
        }
        header
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
                    .child(summary),
            )
            .child(div().flex_1())
    }

    /// 一行 reasoning：默认收起，按档位给不给预览；正文点开后才是纯文本。
    fn reasoning_row(
        &self,
        id: &RenderId,
        text: &str,
        streaming: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = self.settings.verbosity.detail();
        let key = id.label();
        let text = reasoning_text(text);
        // 单节点正文默认展开只在 Full（深度 3）；其余档位默认收起，点选优先。
        let default_open = detail == Detail::Full;
        let open = self.overrides.node_open(&key, default_open);
        let preview = reasoning_summary(text, streaming, detail);
        let entity = cx.entity();
        let toggle_key = key.clone();

        // 点击只挂在抬头行：正文可选中复制，不会被「点一下就收起」打断。
        let mut header = div()
            .id(format!("reasoning-head:{key}"))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    let detail = chat.settings.verbosity.detail();
                    let default_open = detail == Detail::Full;
                    chat.overrides.toggle_node(&toggle_key, default_open);
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
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child("思考"),
            );
        // 展开后摘要与正文首行重复，只留正文。
        if !open {
            header = header.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_size(px(theme::META))
                    .text_color(rgb(theme::INK_MUTED))
                    .child(preview),
            );
        }

        let mut col = div()
            .id(format!("reasoning-row:{key}"))
            .flex()
            .flex_col()
            .gap_1()
            .child(header);
        if open {
            col = col.child(crate::markdown::text(id.clone(), text, TextRole::Reasoning));
        }
        col.into_any_element()
    }

    /// 一行工具调用：家族图标 + 名字 + 标题 + 后缀；点开看审计字段与结果。
    fn tool_row(&self, view: &ToolView, cx: &mut Context<Self>) -> Div {
        // 单工具正文默认收起（Full 档默认展开）；档位通过 Overrides 统一表达。
        let default_open = self.settings.verbosity.detail() == Detail::Full;
        let open = self.overrides.node_open(&view.id, default_open);
        let entity = cx.entity();
        let id = view.id.clone();
        let stroke = match view.status {
            ToolStatus::Running => theme::INK_SOFT,
            ToolStatus::Ok => theme::INK_MUTED,
            ToolStatus::Error | ToolStatus::Stopped => theme::INK,
        };
        // 失败 / 中断时把结果首行顶替参数派生的标题，折叠行就能看到原因；
        // 但命令类工具必须保留「哪条命令失败」——官方对有退出码的行也保留描述。
        let title = if view.family == ToolFamily::Shell {
            &view.title
        } else {
            view.error_summary.as_deref().unwrap_or(&view.title)
        };

        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |chat, cx| {
                    let default_open = chat.settings.verbosity.detail() == Detail::Full;
                    chat.overrides.toggle_node(&id, default_open);
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
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::META))
                    .text_color(rgb(match view.status {
                        ToolStatus::Error | ToolStatus::Stopped => theme::INK,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 权威档位变了（启动读取 / 写回回送 / 写失败回滚）：把滑块拉到同一个值。
        // `set_value` 要 `Window`，事件处理里拿不到，所以只在渲染时补这一步。
        if let Some(verbosity) = self.verbosity_slider_sync.take() {
            let value = verbosity.index() as f32;
            self.verbosity_slider
                .update(cx, |state, cx| state.set_value(value, window, cx));
        }

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

/// reasoning 折叠态的一行摘要。
///
/// 流式时摘要强制显示（与档位无关，官方 `running || settledReasoningPreview`），
/// 落定时才按档位给不给预览：简洁档不显示已结算预览。
/// 摘要算法照官方 `ReasoningRow`：落定取首行，流式取最近完成段落的首行，去 `**`。
/// 官方 reasoning-text.ts 的等价实现：模型 reasoning 首块可能是一整段
/// `<signature>…</signature>` 签名（不含可读内容），必须过滤，否则展开态会
/// 露出不可读的签名串。流式未写完签名标签时也压成空，避免闪出半个标签。
/// 只在首个标签处处理一次——签名只出现在 reasoning 开头。
fn reasoning_text(text: &str) -> &str {
    const OPENING: &str = "<signature>";
    const CLOSING: &str = "</signature>";
    if text.as_bytes().first() != Some(&b'<') {
        return text;
    }
    // text 还是 `<signature>` 的前缀（流式未写完）：压空，不闪标签。
    if OPENING.starts_with(text) {
        return "";
    }
    if !text.starts_with(OPENING) {
        return text;
    }
    let Some(end) = text[OPENING.len()..].find(CLOSING) else {
        return "";
    };
    let mut start = OPENING.len() + end + CLOSING.len();
    let bytes = text.as_bytes();
    while start < bytes.len() {
        if bytes[start] == b'\n' {
            start += 1;
        } else if bytes[start] == b'\r' && bytes.get(start + 1) == Some(&b'\n') {
            start += 2;
        } else {
            break;
        }
    }
    // start 只会落在 ASCII 标签/换行之后，必是 UTF-8 字符边界。
    &text[start..]
}

fn reasoning_summary(text: &str, streaming: bool, detail: Detail) -> String {
    if detail == Detail::Quiet && !streaming {
        return String::new();
    }
    let line = if streaming {
        latest_completed_paragraph_first_line(text)
    } else {
        first_line(text)
    };
    line.replace("**", "")
}

fn first_line(text: &str) -> String {
    let end = text.find('\n').unwrap_or(text.len());
    text[..end].trim().to_owned()
}

/// 官方 `latestCompletedParagraphFirstLine` 的等价实现：段落以空行分隔，
/// 每跨过一段就锁定它的首行，最终停在最近一个「已写完」的段落。
/// 这样每个 chunk 不会让摘要在半行 `1.` / `` ``` `` 上跳动。
fn latest_completed_paragraph_first_line(text: &str) -> String {
    let mut summary = String::new();
    let mut paragraph_start = 0usize;
    loop {
        let separator = paragraph_separator(text, paragraph_start);
        let paragraph_end = separator.as_ref().map_or(text.len(), |(end, _)| *end);
        if let Some(rel) = text[paragraph_start..].find('\n') {
            let newline = paragraph_start + rel;
            if newline <= paragraph_end {
                let candidate = text[paragraph_start..newline].trim();
                if !candidate.is_empty() {
                    summary = candidate.to_owned();
                }
            }
        }
        match separator {
            Some((_, next_start)) => paragraph_start = next_start,
            None => return summary,
        }
    }
}

/// 空行段落分隔符：`\r?\n` 后跟至少一行只剩空白的行。返回（首换行位置, 分隔符末尾）。
fn paragraph_separator(text: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let first_newline = i;
            let mut cursor = i + 1;
            let mut extra_lines = 0usize;
            loop {
                let mut k = cursor;
                while k < bytes.len() && matches!(bytes[k], b'\t' | b' ') {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'\r' {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'\n' {
                    extra_lines += 1;
                    cursor = k + 1;
                } else {
                    break;
                }
            }
            if extra_lines > 0 {
                return Some((first_newline, cursor));
            }
        }
        i += 1;
    }
    None
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
        ToolStatus::Error | ToolStatus::Stopped => true,
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
        ToolResultView::Error { text } => {
            let label = if view.status == ToolStatus::Stopped {
                "已停止"
            } else {
                "失败"
            };
            lines.push(format!("{label}: {text}"));
        }
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
/// 失败 / 中断行丢弃长度与耗时——那是噪声，官方同样只留状态提示。
fn tool_suffix(view: &ToolView) -> String {
    let status = match view.status {
        ToolStatus::Running => "运行中",
        ToolStatus::Error => "失败",
        ToolStatus::Stopped => "已停止",
        ToolStatus::Ok => "",
    };
    if !status.is_empty() {
        return status.to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
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
        ModelGroup, PullRow, SelectedModel, format_tokens, group_plan, host_status, leaf,
        model_menu_rows, reasoning_summary, reasoning_text, short_id, tool_body, tool_suffix,
    };
    use crate::{HostLifecycle, ModelOption, SessionInfo, SidebarPrefs};
    use dsh_protocol::presentation::{Detail, group_summary, plan, process_runs};
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
            error_summary: None,
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

    fn user_row() -> TimelineRow {
        TimelineRow::User {
            id: dsh_protocol::RenderId::User(dsh_protocol::MessageId::Turn { turn: 1, step: 0 }),
            text: "问题".to_owned(),
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
    fn plan_marks_running_containers_non_collapsible() {
        let mut live = tool("t1");
        let TimelineRow::Tool(ref mut view) = live else {
            unreachable!()
        };
        view.status = dsh_protocol::transcript::ToolStatus::Running;
        let rows = vec![user_row(), live];
        let ops = plan(
            &rows,
            Detail::Quiet,
            &dsh_protocol::presentation::Overrides::default(),
            true, // 会话忙：运行中的一轮恒展开且不可收起
        );
        let dsh_protocol::presentation::RenderOp::Turn { spec, .. } = &ops[0] else {
            panic!("应是整轮");
        };
        // 运行中整轮恒展开且不可收起；这就是原 `process_group_running` 的替代。
        assert!(spec.open);
        assert!(!spec.collapsible);
    }

    #[test]
    fn reasoning_text_strips_leading_signature_block() {
        // 官方 reasoning-text.ts：签名块不是可读内容，展开时不能露出。
        assert_eq!(
            reasoning_text("<signature>abc</signature>\n真正的内容"),
            "真正的内容"
        );
        assert_eq!(
            reasoning_text("<signature>abc</signature>\r\n真正的内容"),
            "真正的内容"
        );
        // 签名为空、无换行。
        assert_eq!(reasoning_text("<signature>x</signature>"), "");
        // 签名后多个换行全跳过。
        assert_eq!(reasoning_text("<signature>x</signature>\n\n\n正文"), "正文");
    }

    #[test]
    fn reasoning_text_keeps_normal_and_streaming_prefixes() {
        assert_eq!(reasoning_text("普通推理"), "普通推理");
        // 流式中签名标签还没写完：压空，不闪半个标签。
        assert_eq!(reasoning_text("<sig"), "");
        assert_eq!(reasoning_text("<signature"), "");
        assert_eq!(reasoning_text("<"), "");
        // 非 `<` 开头原样。
        assert_eq!(reasoning_text(" leading"), " leading");
        // 有开标签但没闭标签（流式）：压空。
        assert_eq!(reasoning_text("<signature>还在写"), "");
        // 不是以签名开头，即使后面出现签名也不动（官方只处理首块）。
        assert_eq!(
            reasoning_text("前面<signa</signature>"),
            "前面<signa</signature>"
        );
    }

    #[test]
    fn reasoning_summary_follows_detail_and_stream_state() {
        assert_eq!(
            reasoning_summary("第一行\n第二行", false, Detail::Quiet),
            ""
        );
        assert_eq!(
            reasoning_summary("第一行\n第二行", false, Detail::Brief),
            "第一行"
        );
        // 流式无空行时锁在首行，不随每个 chunk 跳动。
        assert_eq!(
            reasoning_summary("第一行\n第二行", true, Detail::Standard),
            "第一行"
        );
        // 最近完成段落的首行；未完成的那段不算。
        assert_eq!(
            reasoning_summary("开场\n\n已完成段落首行\n未完成", true, Detail::Standard),
            "已完成段落首行"
        );
        // 双星号不进摘要（官方 replaceAll('**','')）。
        assert_eq!(
            reasoning_summary("**重点**\n其他", false, Detail::Standard),
            "重点"
        );
        // 简洁档只关「已结算预览」；流式时摘要强制显示，与档位无关。
        assert_eq!(
            reasoning_summary("第一行\n第二行", true, Detail::Quiet),
            "第一行"
        );
    }

    #[test]
    fn verbosity_folds_completed_turns_at_all_non_flat_layers() {
        // 官方 foldCompletedTurns：三档都折叠已完成轮（只有 Full 平铺不折）。
        // 档位差异落在展开后的组层，由 presentation 层的测试单独验证。
        let rows = vec![user_row(), tool("t1"), text("答")];
        let open = |detail| {
            let ops = plan(
                &rows,
                detail,
                &dsh_protocol::presentation::Overrides::default(),
                false,
            );
            let dsh_protocol::presentation::RenderOp::Turn { spec, .. } = &ops[0] else {
                panic!("应是整轮");
            };
            spec.open
        };
        // 三档都折叠已完成轮（官方 foldCompletedTurns）；展开深度只决定组层。
        assert!(!open(Detail::Quiet));
        assert!(!open(Detail::Brief));
        assert!(!open(Detail::Standard));
    }

    /// 组头把 N 条工具压成一行动作叙述，运行中按档位给实时详情或动作短语。
    #[test]
    fn group_label_uses_official_action_narration() {
        let mut third = tool("t3");
        if let TimelineRow::Tool(view) = &mut third {
            view.family = ToolFamily::FileWrite;
            view.title = "编辑 src/main.rs".to_owned();
            view.status = dsh_protocol::transcript::ToolStatus::Running;
        }
        let rows = vec![tool("t1"), tool("t2"), third];
        let quiet = group_summary(&rows, true, false);
        assert_eq!(
            quiet, "正在写入文件",
            "运行中取当前运行工具的类别，不是最频繁的类别：{quiet}"
        );

        let detailed = group_summary(&rows, true, true);
        assert_eq!(
            detailed, "正在写入文件 · 编辑 src/main.rs",
            "运行中带实时详情时官方是 label + ' · ' + detail"
        );

        let settled = group_summary(&rows, false, true);
        assert_eq!(settled, "执行了命令并已写入文件", "关闭态动作叙述无计数");
    }

    #[test]
    fn group_summary_dedupes_kinds_and_caps_at_three() {
        // 官方 processTitle：top3 动作短语、无计数、多于 3 类追加「等」。
        use dsh_protocol::timeline::ToolFamily as F;
        let families = [
            F::Shell,
            F::Shell,
            F::FileRead,
            F::FileWrite,
            F::Search,
            F::Web,
        ];
        let rows: Vec<_> = families
            .iter()
            .enumerate()
            .map(|(i, family)| {
                let mut row = tool(&format!("t{i}"));
                if let TimelineRow::Tool(view) = &mut row {
                    view.family = *family;
                }
                row
            })
            .collect();
        let label = group_summary(&rows, false, true);
        assert!(!label.contains('1'), "不显示计数：{label}");
        assert!(label.ends_with('等'), "多于 3 类追加「等」：{label}");
    }

    #[test]
    fn tool_suffix_drops_noise_on_failure() {
        let mut row = tool("x");
        let TimelineRow::Tool(view) = &mut row else {
            unreachable!()
        };
        view.output_chars = 12345;
        view.elapsed_ms = Some(320);
        view.status = dsh_protocol::transcript::ToolStatus::Error;
        assert_eq!(tool_suffix(view), "失败");
        view.status = dsh_protocol::transcript::ToolStatus::Stopped;
        assert_eq!(tool_suffix(view), "已停止");
        view.status = dsh_protocol::transcript::ToolStatus::Ok;
        let suffix = tool_suffix(view);
        assert!(suffix.contains("12345 字"), "{suffix}");
        assert!(suffix.contains("320ms"), "{suffix}");
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
