//! dsh-client-rs 的 GPUI 界面层。
//!
//! 视觉基线是 fx-ui：mono 贯穿正文、层级靠字色、工具行平铺、卡片只给交互块、
//! chrome 零彩色（DECISIONS #7）。表面优先级见 `docs/DESIGN.md`
//! 的「使用定位」：code 为主，工具行与 diff 渲染是 P0。
//!
//! GPUI 只从 `gpui_kit` 取：`use gpui_kit::*` 就是 GPUI 本身，
//! `gpui_kit::base` / `::component` / `::assets` 是各层。
//!
//! 本 crate **不做网络**。外部把 [`UiEvent`] 推进来，把 [`UiCommand`] 收回去，
//! 传输与 host 生命周期都在 `dsh-host` 和 `apps/desktop` 里。

pub mod brand;
pub mod chat;
pub mod completion;
pub mod markdown;
pub mod settings;
pub mod theme;

pub use chat::Chat;
pub use settings::{ChatVerbosity, RenderPolicy, Settings};

use dsh_protocol::context::Occupancy;
use dsh_protocol::stream::AssistantStreamFrame;
use dsh_protocol::transcript::{Entry, ToolCall};
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc::{Receiver, Sender};

/// 侧栏的客户端私有偏好。
///
/// host 的注册表只表达「有哪些工作区、它记账了哪些会话」；「用户主动收起了
/// 哪个工作区」以及「host 账本里没有、但确实属于某个工作区的历史会话」是
/// 客户端自己的事，不塞进 `WorkspaceRegistry`，也不写 host。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarPrefs {
    /// 被移出侧栏的工作区 id。host 注册与账本都还在，只是不显示。
    pub hidden_workspace_ids: HashSet<String>,
    /// `workspace_id -> [session_id]`：补 host 账本缺的历史会话。
    /// 老客户端建会话只传 `cwd`，host 从未把这些 id 记进账本；重建工作区时
    /// 靠它们把会话拉回原组，避免整组在侧栏里消失。
    pub adopted: HashMap<String, Vec<String>>,
}

/// 上下文用量。分子是 `usage.inputTokens`——实测它就是「这轮喂进去多少」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtxMeter {
    /// 已用 token。
    pub used: u64,
}

/// 侧栏里一个会话的摘要。字段来自 `session/list` 的 `SessionSummary`。
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// 会话 id。
    pub id: String,
    /// 投影标题；空白会话为 `None`。
    pub title: Option<String>,
    /// 会话工作目录，侧栏按它分组。
    pub cwd: String,
    /// 最近更新时间（毫秒时间戳）。
    pub updated_at: i64,
    /// 是否正在生成。
    pub running: bool,
    /// 是否空白会话。
    pub blank: bool,
    /// 当前权限 preset（投影 `permissions.currentValue`）。
    pub mode: Option<String>,
    /// 当前模型选择（投影 `modelSelection.next`，缺失时退回 `lastUsed`）。
    pub model: Option<SelectedModel>,
}

/// 一档推理强度（状态的中的 model 芯片右侧那个数）。
#[derive(Debug, Clone)]
pub struct EffortOption {
    /// 强度 id，写回时用它。
    pub id: String,
    /// 显示名。
    pub name: String,
}

/// 目录里的一个模型。
#[derive(Debug, Clone)]
pub struct ModelOption {
    /// 模型 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 选中该模型时默认用的强度。
    pub default_effort: Option<String>,
    /// 该模型支持的全部强度。
    pub efforts: Vec<EffortOption>,
}

/// 一个提供方和它下面的模型。
#[derive(Debug, Clone)]
pub struct ModelGroup {
    /// 提供方 id。
    pub provider: String,
    /// 提供方显示名。
    pub provider_name: String,
    /// 该提供方的模型。
    pub models: Vec<ModelOption>,
}

/// 会话当前选中的模型。
#[derive(Debug, Clone)]
pub struct SelectedModel {
    /// 提供方 id。
    pub provider: String,
    /// 模型 id。
    pub model: String,
    /// 推理强度 id。
    pub effort: Option<String>,
}

/// 一个权限 preset（composer 左侧 mode 的候选项）。
#[derive(Debug, Clone)]
pub struct PresetInfo {
    /// preset key，写回时用它。
    pub value: String,
    /// 显示名。
    pub name: String,
}

/// host 生命周期的互斥状态。
///
/// 只有 supervisor 发布这些状态：UI 不再从「没收到事件」推断 host 死活。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostLifecycle {
    /// 正在安装 runtime / 拉起 host / 等握手。
    #[default]
    Starting,
    /// host 已握手，可以服务命令。
    Ready,
    /// 空闲后主动收起 host；有命令会重新拉起。
    Sleeping,
    /// 这一代 host 启动或握手失败。
    Failed,
}

/// 外部推给界面的东西。
#[derive(Debug)]
pub enum UiEvent {
    /// host 生命周期变化。启动、休眠、唤醒、失败都从这里来。
    HostState(HostLifecycle),
    /// 切到了某个会话（快照已回放完毕）。
    Connected {
        /// host 的 origin。
        origin: String,
        /// 会话 id。
        session: String,
        /// 会话根目录，侧栏与标题栏都从它取名。
        cwd: String,
    },
    /// 侧栏会话列表（每次重新打开/新建/提交后刷新）。
    Sessions(Vec<SessionInfo>),
    /// 工作区注册表 + 归档/置顶集合（`workspace/follow` 的基线 + 增量）。
    Workspaces(dsh_protocol::workspace::WorkspaceRegistry),
    /// 侧栏私有偏好（隐藏集合 + 历史归属覆盖）。先于首批会话/工作区事件到达。
    SidebarPrefs(SidebarPrefs),
    /// 切换会话后回放的历史。
    Transcript(Vec<Entry>),
    /// 一次工具调用的状态变化（发起或拿到结果）。按 `callId` upsert。
    Tool(ToolCall),
    /// 权限 preset 目录（启动时一次）。
    Presets(Vec<PresetInfo>),
    /// 上下文占用与缓存命中率（来自 `session/projections`）。
    Context {
        /// 占用；分子或分母缺失时为 `None`（此时不画圆环，见 DECISIONS #21）。
        occupancy: Option<Occupancy>,
        /// 缓存命中率百分比；没有输入时为 `None`。
        cache_hit: Option<u8>,
    },
    /// 全局聊天详略；启动从 host profile 读，settings 页写回后回送权威值。
    ChatVerbosity {
        /// 实际生效的档位。
        verbosity: ChatVerbosity,
        /// 读/写 host 设置失败时的说明；成功为 `None`。
        error: Option<String>,
        /// 这是点击后的写回确认（用于显示「已保存」），不是启动/重连时的读取。
        saved: bool,
    },
    /// 模型目录（启动时一次）与未配置会话时的默认选择。
    Models {
        /// 按提供方分组的选择。
        groups: Vec<ModelGroup>,
        /// host 的默认选择；会话自己还没选过时用它显示。
        default: Option<SelectedModel>,
        /// 提供方级故障（已按官方文案格式化，如「DeepSeek 加载失败：…」）。
        /// 必须渲染，否则「有凭据但列举失败」会表现成「什么都没配」。
        failures: Vec<String>,
    },
    /// 一条流式帧，界面自己折叠。
    Frame(AssistantStreamFrame),
    /// 补全候选（`seq` 用来丢掉过期结果）。
    Completions {
        /// 对应请求的序号。
        seq: u64,
        /// 候选列表，已按查询串过滤好。
        items: Vec<CompletionItem>,
    },
    /// 链路或 host 侧失败。
    Failed(String),
}

/// 一条补全候选。
#[derive(Debug, Clone)]
pub struct CompletionItem {
    /// 主标签，如 `/compact` 或 `src/main.rs`。
    pub label: String,
    /// 右侧灰字，如命令摘要。
    pub hint: String,
    /// 接受后插进草稿的文本。
    pub insert: String,
}

/// 界面推给外部的东西。
#[derive(Debug)]
pub enum UiCommand {
    /// 把这句话发给 agent。
    Prompt(String),
    /// 切到某个已存在的会话。
    OpenSession(String),
    /// 在某个工作区新建会话。
    NewSession {
        /// 工作区路径（标题栏与侧栏显示用）。
        cwd: String,
        /// 工作区 id。`session/create` 必须带它，host 才会把会话记进账本。
        workspace_id: String,
    },
    /// 把一个已有目录注册成工作区。
    AddWorkspace {
        /// 目录绝对路径。
        path: String,
    },
    /// 侧栏私有偏好变化，落本地快照。不碰 host 注册与账本。
    SaveSidebarPrefs(SidebarPrefs),
    /// 归档一个会话：从侧栏收起，记录与目录都留着，随时可取消归档。
    ArchiveSession {
        /// 会话 id。
        session_id: String,
        /// 会话还在跑时是否顺带停掉它（`stopActivity`）。
        stop_activity: bool,
    },
    /// 取消归档。
    UnarchiveSession {
        /// 会话 id。
        session_id: String,
    },
    /// 停掉当前这一轮（`session/cancel`）。
    Cancel {
        /// 会话 id。
        session_id: String,
    },
    /// 重读一次会话列表（`session/list` 的 `running` 会变，侧栏的运行点靠它）。
    RefreshSessions,
    /// 切换当前会话的权限 preset。
    SetMode(String),
    /// 要补全：`@` 查工作区文件，`/` 查斜杠命令。
    Complete {
        /// 哪一类。
        kind: completion::Kind,
        /// 已经打出来的查询串。
        query: String,
        /// 请求序号，回程原样带回。
        seq: u64,
    },
    /// 切换当前会话的模型与推理强度。
    SetModel {
        /// 提供方 id。
        provider: String,
        /// 模型 id。
        model: String,
        /// 推理强度 id；模型不支持推理时为 `None`。
        effort: Option<String>,
    },
    /// 把聊天详略写回 host profile。
    SetChatVerbosity {
        /// 用户点选的档位。
        verbosity: ChatVerbosity,
        /// 点击前生效的档位，写失败时 UI 用它回滚。
        previous: ChatVerbosity,
    },
}

impl UiCommand {
    /// 变体名，只用于日志（排查「哪条命令在刷」时用，不承载语义）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Prompt(_) => "Prompt",
            Self::OpenSession(_) => "OpenSession",
            Self::NewSession { .. } => "NewSession",
            Self::AddWorkspace { .. } => "AddWorkspace",
            Self::SaveSidebarPrefs(_) => "SaveSidebarPrefs",
            Self::ArchiveSession { .. } => "ArchiveSession",
            Self::UnarchiveSession { .. } => "UnarchiveSession",
            Self::Cancel { .. } => "Cancel",
            Self::RefreshSessions => "RefreshSessions",
            Self::SetMode(_) => "SetMode",
            Self::Complete { .. } => "Complete",
            Self::SetModel { .. } => "SetModel",
            Self::SetChatVerbosity { .. } => "SetChatVerbosity",
        }
    }
}

/// 设计稿的画布尺寸。屏幕装得下就用它，装不下再夹。
const DESIGN_WINDOW_SIZE: Size<Pixels> = size(px(1440.0), px(920.0));

/// 窗口下限：再窄就放不下「侧栏 264 + 可读的主区」，再矮就把输入框挤掉。
const MIN_WINDOW_SIZE: Size<Pixels> = size(px(960.0), px(640.0));

/// 窗口与屏幕可用区域之间留的边（两侧各一半的余量）。
const SCREEN_MARGIN: f32 = 96.0;

/// 首屏窗口位置与大小：按设计稿尺寸居中，屏幕小就夹到可用区域内。
///
/// 尺寸不是硬编码常量说了算——窗口服务器本身也会夹，但那样起点是固定的
/// (80,60)、夹完就贴边，看起来像窗口跑偏了。这里自己算清楚。
fn initial_window_bounds(cx: &App) -> WindowBounds {
    let Some(display) = cx.primary_display() else {
        return WindowBounds::Windowed(Bounds::new(point(px(80.0), px(60.0)), DESIGN_WINDOW_SIZE));
    };

    let screen = display.bounds();
    let usable_width = screen.size.width.as_f32() - SCREEN_MARGIN;
    let usable_height = screen.size.height.as_f32() - SCREEN_MARGIN;

    let width = DESIGN_WINDOW_SIZE
        .width
        .as_f32()
        .min(usable_width)
        .max(MIN_WINDOW_SIZE.width.as_f32());
    let height = DESIGN_WINDOW_SIZE
        .height
        .as_f32()
        .min(usable_height)
        .max(MIN_WINDOW_SIZE.height.as_f32());

    let origin = point(
        screen.origin.x + px((screen.size.width.as_f32() - width) / 2.0),
        screen.origin.y + px((screen.size.height.as_f32() - height) / 2.0),
    );

    WindowBounds::Windowed(Bounds::new(origin, size(px(width), px(height))))
}

/// 启动 GPUI 应用并打开主窗口。
///
/// `events` 是外部事件入口，`commands` 是用户输入出口。
pub fn run(events: Receiver<UiEvent>, commands: Sender<UiCommand>) {
    // 调试钩子要用一份命令发送端；真正的那个会随 `Chat` 进去。
    let debug_commands = commands.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let bounds = initial_window_bounds(cx);

            // 标题栏透明、红绿灯按设计稿坐标（侧栏左上 24,20）摆放。
            let options = WindowOptions {
                window_bounds: Some(bounds),
                // 拖到小于这个尺寸，侧栏和输入框就会互相挤掉。
                window_min_size: Some(MIN_WINDOW_SIZE),
                titlebar: Some(TitlebarOptions {
                    title: Some("cowork".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(24.0), px(20.0))),
                }),
                ..WindowOptions::default()
            };

            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                let wake_commands = debug_commands.clone();
                let chat = cx.new(|cx| Chat::new(window, cx, events, commands));

                // 仅调试：等一会儿发一条无害命令（`RefreshSessions`），
                // 验证「host 休眠后有命令会重新拉起来」。走的是真实命令通道。
                if let Some(seconds) = std::env::var("DSH_DEBUG_WAKE_AFTER")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(seconds))
                            .await;
                        tracing::info!(seconds, "调试：发一条命令以唤醒 host");
                        let _ = wake_commands.try_send(UiCommand::RefreshSessions);
                    })
                    .detach();
                }

                // 仅调试：命令行给一句话就自动提交一次，走和回车完全相同的路径。
                if let Ok(prompt) = std::env::var("DSH_DEBUG_PROMPT") {
                    chat.update(cx, |chat, cx| chat.debug_send(prompt, window, cx));
                }
                // 仅调试：启动后切一次权限 preset，验证命令 → 投影 → 芯片的回路。
                if let Ok(mode) = std::env::var("DSH_DEBUG_MODE") {
                    chat.update(cx, |chat, _| chat.debug_set_mode(mode));
                }
                // 仅调试：把草稿设成指定内容并触发一次补全（等 host 接上再生效）。
                if let Ok(draft) = std::env::var("DSH_DEBUG_COMPLETE") {
                    let handle = window.window_handle();
                    let accept = std::env::var("DSH_DEBUG_ACCEPT").is_ok();
                    let chat = chat.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(3000))
                            .await;
                        let _ = handle.update(cx, |_, window, cx| {
                            chat.update(cx, |chat, cx| chat.debug_complete(&draft, window, cx));
                        });
                        // 仅调试：合成按键，验证补全的键位与选中逻辑。
                        // 跑这个开关时 `send` 自动进测试模式，绝不会真的投递。
                        if let Ok(keys) = std::env::var("DSH_DEBUG_KEYS") {
                            // Keystroke::parse 要 &'static str，这里只认这几个键。
                            let names: Vec<&'static str> = keys
                                .split_whitespace()
                                .map(|name| match name {
                                    "up" => "up",
                                    "enter" => "enter",
                                    "tab" => "tab",
                                    "escape" => "escape",
                                    "shift-tab" => "shift-tab",
                                    "shift-enter" => "shift-enter",
                                    _ => "down",
                                })
                                .collect();
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(4000))
                                .await;
                            for name in &names {
                                let Ok(keystroke) = Keystroke::parse(name) else {
                                    continue;
                                };
                                let _ = handle.update(cx, |_, window, cx| {
                                    window.dispatch_event(
                                        PlatformInput::KeyDown(KeyDownEvent {
                                            keystroke,
                                            is_held: false,
                                            prefer_character_input: false,
                                        }),
                                        cx,
                                    );
                                });
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(250))
                                    .await;
                            }
                            let _ = handle.update(cx, |_, _window, cx| {
                                chat.update(cx, |chat, cx| {
                                    tracing::info!(draft = %chat.debug_draft(cx), "按键后的草稿");
                                });
                            });
                            return;
                        }
                        if !accept {
                            return;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(4000))
                            .await;
                        let _ = handle.update(cx, |_, window, cx| {
                            chat.update(cx, |chat, cx| {
                                chat.debug_accept(window, cx);
                                tracing::info!(draft = %chat.debug_draft(cx), "接受补全后的草稿");
                            });
                        });
                    })
                    .detach();
                }
                // 仅调试：在给定窗口坐标上合成左键点击（验证浮层/候选可点）。
                // 坐标是 points，可多击：`DSH_DEBUG_CLICK="243,74;500,543"`。
                if let Ok(spec) = std::env::var("DSH_DEBUG_CLICK") {
                    let clicks: Vec<(f32, f32)> = spec
                        .split(';')
                        .filter_map(|pair| {
                            let (x, y) = pair.split_once(',')?;
                            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                        })
                        .collect();
                    if !clicks.is_empty() {
                        let handle = window.window_handle();
                        let chat = chat.clone();
                        cx.spawn(async move |cx| {
                            // 等补全面板真的开（走 host 要几百毫秒），再点。
                            for _ in 0..40 {
                                if handle
                                    .update(cx, |_, _, cx| chat.read(cx).debug_completion_open())
                                    .unwrap_or(false)
                                {
                                    break;
                                }
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(250))
                                    .await;
                            }
                            for (x, y) in clicks {
                                // 面板刚开时，它的命中框要等下一帧上屏才生效：等久一点再点。
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(1200))
                                    .await;
                                tracing::info!(x, y, "合成点击");
                                let _ = handle.update(cx, |_, window, cx| {
                                    // 先移动再按下：hover 状态靠 MouseMove 建立。
                                    window.dispatch_event(
                                        PlatformInput::MouseMove(MouseMoveEvent {
                                            position: point(px(x), px(y)),
                                            pressed_button: None,
                                            modifiers: Modifiers::default(),
                                        }),
                                        cx,
                                    );
                                    window.dispatch_event(
                                        PlatformInput::MouseDown(MouseDownEvent {
                                            button: MouseButton::Left,
                                            position: point(px(x), px(y)),
                                            modifiers: Modifiers::default(),
                                            click_count: 1,
                                            first_mouse: false,
                                        }),
                                        cx,
                                    );
                                });
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(500))
                                    .await;
                            }
                            let _ = handle.update(cx, |_, _window, cx| {
                                chat.update(cx, |chat, cx| {
                                    tracing::info!(
                                        draft = %chat.debug_draft(cx),
                                        mode = %chat.debug_mode(),
                                        "点击后的状态"
                                    );
                                });
                            });
                        })
                        .detach();
                    }
                }
                // 仅调试：注册一个目录当工作区（跳过系统选目录框）。
                if let Ok(path) = std::env::var("DSH_DEBUG_WORKSPACE") {
                    chat.update(cx, |chat, _| chat.debug_add_workspace(path));
                }
                // 仅调试：同上，切模型。
                if let Ok(model) = std::env::var("DSH_DEBUG_MODEL") {
                    chat.update(cx, |chat, _| chat.debug_set_model(model));
                }
                // 仅调试：归档 / 取消归档一条会话（走和图标完全相同的命令通道）。
                // 等等 host 把侧栏、注册表都送到再动手。
                for (key, archive) in [("DSH_DEBUG_ARCHIVE", true), ("DSH_DEBUG_UNARCHIVE", false)]
                {
                    let Ok(id) = std::env::var(key) else { continue };
                    let handle = window.window_handle();
                    let chat = chat.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(3000))
                            .await;
                        let _ = handle.update(cx, |_, _, cx| {
                            chat.update(cx, |chat, cx| {
                                if archive {
                                    chat.debug_archive(id);
                                } else {
                                    chat.debug_unarchive(id);
                                }
                                cx.notify();
                            });
                        });
                    })
                    .detach();
                }

                // 仅调试：等价于点 composer 的「停」（同一段 `cancel_turn`）。
                // 本机合成鼠标点击成功率约一半，所以用这个钩子验停轮链路。
                if let Ok(delay) = std::env::var("DSH_DEBUG_CANCEL") {
                    let millis = delay.parse::<u64>().unwrap_or(3000);
                    let handle = window.window_handle();
                    let chat = chat.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(millis))
                            .await;
                        let _ = handle.update(cx, |_, _, cx| {
                            chat.update(cx, |chat, cx| chat.debug_cancel(cx));
                        });
                    })
                    .detach();
                }

                chat
            });

            if let Err(error) = opened {
                eprintln!("无法打开窗口: {error}");
                std::process::exit(1);
            }
        });
}
