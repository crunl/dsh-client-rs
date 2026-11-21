//! cowork 桌面入口。
//!
//! 线程模型：GPUI 在主线程；host 的 I/O 全在一个专用线程里的 tokio runtime 上，
//! 两边只通过两条 channel 说话（[`UiEvent`] 进界面，[`UiCommand`] 出界面）。
//!
//! host 生命周期在 [`host`]：对齐官方 desktop，无视本机已在跑的 dsh web，
//! 永远自己拉起内置运行时（`dsh --profile cowork`），app 退出时自己拉的
//! 子进程跟着收掉（DECISIONS #41）。
//! 开发期 `DSH_READY_URL='http://127.0.0.1:PORT/?token=…'` 仍走 `/connect`。
//!
//! 调试开关：`DSH_DEBUG_PROMPT="…"` 会在窗口打开后自动提交一次，走和用户按回车
//! 完全相同的路径（`Chat::send`），用于无键盘注入环境下的端到端验证。
//! 其余开关集中在 `dsh_client_ui`（`DSH_DEBUG_OPEN` / `_MODE` / `_MODEL` /
//! `_COMPLETE` / `_ACCEPT` / `_KEYS` / `_CLICK` / `_WORKSPACE` / `_ARCHIVE` /
//! `_UNARCHIVE`），含义见 `crates/dsh-client-ui/src/lib.rs` 与 docs/DECISIONS.md。

mod cache;
mod host;
mod host_settings;
mod instance;
mod model_settings;
mod runtime;

use runtime::{RuntimeControl, RuntimeRequest, RuntimeStore};

use dsh_client_ui::completion;
use dsh_client_ui::{
    CompletionItem, DiscoveredChoice, EffortOption, HostLifecycle, ModelGroup, ModelOption,
    PresetInfo, ProviderSetting, SelectedModel, SessionInfo, SidebarPrefs, UiCommand, UiEvent,
};
use dsh_host::{Connection, FollowStream};
use dsh_protocol::catalog::{ModelCatalog, PermissionCatalog};
use dsh_protocol::commands::CommandDescriptor;
use dsh_protocol::context::SessionProjectionsValue;
use dsh_protocol::files::{
    DirectoryListing, FileCandidate, FileLookup, FileSearchRequest, FileSearchResult,
    has_hidden_segment, is_excluded_path, plan_lookup, rank,
};
use dsh_protocol::session::{SessionCreateValue, SessionListValue};
use dsh_protocol::stream::{AssistantStreamFrame, StreamChunk};
use dsh_protocol::stream::{FollowValue, StreamItemKind};
use dsh_protocol::transcript::{Entry, Transcript};
use dsh_protocol::workspace::{
    ArchiveSessionRequest, WorkspaceArchiveValue, WorkspaceCreated, WorkspaceFollowFrame,
    WorkspaceRegistry,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{Receiver, Sender};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // CLI adapter：与 GUI 共用 RuntimeStore，但不拿单实例锁、不开窗。
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("runtime") {
        match runtime::cli(&args[2..]) {
            Ok(output) => print!("{output}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let (command_tx, command_rx) = tokio::sync::mpsc::channel::<UiCommand>(8);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<UiEvent>(256);

    // 侧栏先用本地快照画：host 还在拉/还在休眠时也不至于空着。
    // 只还原侧栏，不还原时间线——历史的真相在 `session/follow`。
    let mut initial_prefs = SidebarPrefs::default();
    if let Some(snapshot) = cache::load() {
        tracing::info!(sessions = snapshot.sessions.len(), "读到本地侧栏快照");
        initial_prefs = snapshot.prefs();
        // 偏好必须排在会话/工作区之前：界面收到首批列表时才能正确收养历史会话。
        let _ = event_tx.try_send(UiEvent::SidebarPrefs(initial_prefs.clone()));
        let _ = event_tx.try_send(UiEvent::Sessions(snapshot.sessions()));
        let _ = event_tx.try_send(UiEvent::Workspaces(snapshot.registry()));
    }
    let sidebar_prefs = Arc::new(Mutex::new(initial_prefs));

    // 同一个 profile 只开一个窗口进程：双击两次不该起两个 host 抢同一个 DSH_HOME。
    let profile = std::env::var("COWORK_PROFILE").unwrap_or_else(|_| host::PROFILE.to_owned());
    let guard = match instance::acquire(&profile) {
        instance::Acquired::Guard(guard) => guard,
        instance::Acquired::AlreadyRunning => {
            // 双击第二次不该发生任何事——但也不该像「没反应」。把已有窗口提到前面。
            tracing::info!(%profile, "已经有一个 cowork 在跑，把它的窗口提上来");
            if let Some(bundle) = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.ancestors().nth(3).map(std::path::Path::to_path_buf))
            {
                let _ = std::process::Command::new("open").arg(bundle).status();
            }
            return;
        }
    };

    // 窗口先出来：host 由 supervisor 在线程里拉起/收起，不再阻塞在开窗前。
    // 它也不再绑定 app 生命周期：空闲一阵就收掉（见 `host_loop` 的 idle 分支）。
    spawn_host_supervisor(command_rx, event_tx, sidebar_prefs);
    dsh_client_ui::run(event_rx, command_tx);

    // 锁在这个作用域里，退出时 drop；host 由 supervisor 线程自己收。
    drop(guard);
}

/// 起一个专用线程跑 tokio，负责全部 host I/O 与 host 的生死。
fn spawn_host_supervisor(
    commands: Receiver<UiCommand>,
    events: Sender<UiEvent>,
    sidebar_prefs: Arc<Mutex<SidebarPrefs>>,
) {
    let failure_events = events.clone();
    let spawned = std::thread::Builder::new()
        .name("dsh-host-io".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = events.blocking_send(UiEvent::HostState(HostLifecycle::Failed));
                    let _ = events
                        .blocking_send(UiEvent::Failed(format!("tokio 运行时启动失败: {error}")));
                    return;
                }
            };
            runtime.block_on(supervisor_loop(commands, events, sidebar_prefs));
        });

    if let Err(error) = spawned {
        let _ = failure_events.try_send(UiEvent::HostState(HostLifecycle::Failed));
        let _ =
            failure_events.try_send(UiEvent::Failed(format!("无法启动 host I/O 线程: {error}")));
        tracing::error!(%error, "无法启动 host I/O 线程");
    }
}

/// 当前订阅的会话（切会话时要先掐掉旧流的读取任务）。
struct ActiveSession {
    id: String,
    reader: tokio::task::JoinHandle<()>,
}

/// 一代 host 的结束方式。
enum Outcome {
    /// 界面退出（命令通道关了）：收掉 host 并结束。
    Closed,
    /// 空闲到点：收掉 host，下次有命令再拉起。带上刚才打开的会话。
    Sleep {
        /// 休眠前活跃的会话 id。
        active: Option<String>,
    },
}

/// host 的生死管理：拉起（含 handshake）→ 服务一代命令 → 空闲收掉 → 等命令再来。
///
/// 为什么不让 host 活到 app 退出：一个空闲 host 常驻 ~385MB（实测：web 系
/// profile、无对话、三次中位数），而客户端自己 ~85MB。north star 是 RAM，
/// 所以 agent 运行时只在「有活要干」时存在；重拉一次到 ready 约 2.4s。
async fn supervisor_loop(
    mut commands: Receiver<UiCommand>,
    events: Sender<UiEvent>,
    sidebar_prefs: Arc<Mutex<SidebarPrefs>>,
) {
    // 休眠前记住在哪个会话里，醒来接着跟（`session/follow` 会重放快照）。
    let mut resume: Option<String> = std::env::var("DSH_DEBUG_OPEN").ok();
    let mut pending: Option<UiCommand> = None;
    let store = RuntimeStore::from_env();

    loop {
        let _ = events
            .send(UiEvent::HostState(HostLifecycle::Starting))
            .await;

        let startup = match host::start(&store, RuntimeRequest::Default) {
            Ok(startup) => startup,
            Err(error) => {
                tracing::error!(%error, "无法拉起 host");
                let _ = events.send(UiEvent::HostState(HostLifecycle::Failed)).await;
                let _ = events
                    .send(UiEvent::Failed(format!("无法拉起 host: {error}")))
                    .await;
                return;
            }
        };

        let conn = match Connection::connect(&startup.ready_url).await {
            Ok(conn) => Arc::new(conn),
            Err(error) => {
                let _ = events.send(UiEvent::HostState(HostLifecycle::Failed)).await;
                let _ = events
                    .send(UiEvent::Failed(format!("握手失败: {error}")))
                    .await;
                // 看门狗与 node 一起收掉，别留一个没人连的 host。
                drop(startup.process);
                return;
            }
        };

        if let Some(id) = &startup.runtime_id
            && let Err(error) = store.control(RuntimeControl::MarkReady(id.clone()))
        {
            tracing::warn!(%error, id, "记录 runtime last_good 失败");
        }

        let outcome = host_loop(
            conn,
            resume.clone(),
            pending.take(),
            &mut commands,
            &events,
            Arc::clone(&sidebar_prefs),
        )
        .await;
        // 这一代 host（含看门狗）在这里收。
        drop(startup.process);

        match outcome {
            Outcome::Closed => return,
            Outcome::Sleep { active } => {
                resume = active;
                tracing::info!("空闲，收起 host");
                let _ = events
                    .send(UiEvent::HostState(HostLifecycle::Sleeping))
                    .await;
                // 命令不能丢：拿到的第一条当作下一代的第一条命令。
                match commands.recv().await {
                    Some(command) => {
                        pending = Some(command);
                        tracing::info!("有活了，重新拉起 host");
                    }
                    None => return,
                }
            }
        }
    }
}

/// 空闲多久就收掉 host。`COWORK_SLEEP_IDLE_SECS=0` 关闭休眠（开发排查时用）。
fn idle_timeout() -> Option<std::time::Duration> {
    let seconds = std::env::var("COWORK_SLEEP_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
    (seconds > 0).then(|| std::time::Duration::from_secs(seconds))
}

/// 启动时把 host profile 里的聊天详略投影给 UI。读失败也发一次，
/// 让 settings 页显示原因；UI 自己不持久化这个偏好。
///
/// 休眠醒来时若第一条就是切档命令，不要让旧值先把用户的乐观选择盖回去；
/// 那条命令写完会回送权威值。
async fn load_chat_verbosity(
    conn: &Connection,
    events: &Sender<UiEvent>,
    first: Option<&UiCommand>,
) {
    if matches!(first, Some(UiCommand::SetChatVerbosity { .. })) {
        return;
    }
    let event = match host_settings::describe(conn).await {
        Ok(setting) => UiEvent::ChatVerbosity {
            verbosity: setting.verbosity,
            error: None,
            saved: false,
        },
        Err(error) => {
            tracing::warn!(%error, "读聊天详略失败");
            UiEvent::ChatVerbosity {
                verbosity: dsh_client_ui::ChatVerbosity::default(),
                error: Some(error),
                saved: false,
            }
        }
    };
    let _ = events.send(event).await;
}

/// settings 页点选后的权威写回。先乐观画 UI，再把 host 确认值（或失败回滚值）发回去。
async fn write_chat_verbosity(
    conn: &Connection,
    events: &Sender<UiEvent>,
    verbosity: dsh_client_ui::ChatVerbosity,
    previous: dsh_client_ui::ChatVerbosity,
) {
    let result = async {
        let setting = host_settings::describe(conn).await?;
        host_settings::update(conn, setting.revision, verbosity).await
    }
    .await;
    let event = match result {
        Ok(actual) => {
            tracing::info!(verbosity = actual.host_key(), "聊天详略已写入 host profile");
            UiEvent::ChatVerbosity {
                verbosity: actual,
                error: None,
                saved: true,
            }
        }
        Err(error) => {
            tracing::warn!(%error, "写聊天详略失败，回滚");
            UiEvent::ChatVerbosity {
                verbosity: previous,
                error: Some(error),
                saved: false,
            }
        }
    };
    let _ = events.send(event).await;
}

/// 还有没有在跑的轮？收 host 前得问一次：服务端才是权威。
///
/// 出错时按「有活」处理：宁可多留一会儿，也不要在用户用着的时候把 host 抽掉。
async fn any_turn_running(conn: &Connection) -> bool {
    match conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await
    {
        Ok(list) => {
            let busy = list.items.iter().any(|item| item.running);
            tracing::debug!(busy, sessions = list.items.len(), "休眠前检查");
            busy
        }
        Err(error) => {
            tracing::warn!(%error, "休眠前检查失败，按「有活」处理");
            true
        }
    }
}

/// 服务一代 host：bootstrap → 命令循环 → 空闲退出。
async fn host_loop(
    conn: Arc<Connection>,
    resume: Option<String>,
    mut first: Option<UiCommand>,
    commands: &mut Receiver<UiCommand>,
    events: &Sender<UiEvent>,
    sidebar_prefs: Arc<Mutex<SidebarPrefs>>,
) -> Outcome {
    // 启动时不再随手在 `$HOME` 里建会话：那样建出来的会话既不在任何注册过的
    // 工作区里（侧栏落进「未分组」），cwd 也不是用户想干活的地方。现在主区先
    // 显示空状态，由用户挑一个工作区再建会话（对齐官方 desktop 先选工作区）。
    // `events` 是个克隆的发送端：后面的 bootstrap 与命令分支照旧用 `&events`。
    let events: Sender<UiEvent> = events.clone();

    let _ = events.send(UiEvent::HostState(HostLifecycle::Ready)).await;
    load_chat_verbosity(&conn, &events, first.as_ref()).await;
    load_presets(&conn, &events).await;
    load_models(&conn, &events).await;
    refresh_model_settings(&conn, &events).await;
    let mut known = refresh_sessions(&conn, &events, None).await;
    let mut active: Option<ActiveSession> = None;

    // 上一个 host 里打开的会话（或 `DSH_DEBUG_OPEN`）：醒来就先把它接回来。
    // 启动时不随手在 `$HOME` 里建会话：那样建出来的会话既不在任何注册过的
    // 工作区里（侧栏落进「未分组」），cwd 也不是用户想干活的地方。没有会话时
    // 主区是空状态，由用户挑一个工作区（对齐官方 desktop 先选工作区）。
    if let Some(session_id) = resume {
        let cwd = std::env::var("DSH_WORKSPACE")
            .ok()
            .or_else(|| {
                known
                    .iter()
                    .find(|item| item.id == session_id)
                    .map(|item| item.cwd.clone())
            })
            .unwrap_or_default();
        tracing::info!(origin = conn.origin(), session = %session_id, "打开会话");
        switch_to(&conn, &events, &mut active, &session_id, &cwd).await;
        refresh_context(&conn, &events, &session_id).await;
        known = refresh_sessions(&conn, &events, Some(&session_id)).await;
    } else if std::env::var("DSH_DEBUG_PROMPT").is_ok() {
        // 仅调试：`DSH_DEBUG_PROMPT` 要往会话里发一句话，没指定 id 时就按老规则
        // （`DSH_WORKSPACE` / 非根的当前目录 / 家目录）建一个；正常启动不走这里。
        let cwd = std::env::var("DSH_WORKSPACE")
            .ok()
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| path.display().to_string())
                    .filter(|path| path != "/")
            })
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_else(|| ".".to_owned());
        match conn
            .call::<SessionCreateValue>("session/create", json!({ "request": { "cwd": cwd } }))
            .await
        {
            Ok(created) => {
                tracing::info!(
                    origin = conn.origin(),
                    session = created.session_id,
                    "会话就绪"
                );
                switch_to(&conn, &events, &mut active, &created.session_id, &cwd).await;
                refresh_context(&conn, &events, &created.session_id).await;
                known = refresh_sessions(&conn, &events, Some(&created.session_id)).await;
            }
            Err(error) => {
                let _ = events
                    .send(UiEvent::Failed(format!("建会话失败: {error}")))
                    .await;
            }
        }
    }
    // 侧栏那份注册表只有一个主人：follow 与归档/取消归档的调用方共写同一份，
    // 否则自己发起的变化不会回流浪，下一次帧会把乐观状态盖回去。
    let registry = Arc::new(Mutex::new(WorkspaceRegistry::default()));
    // host 被我们自己收掉时，订阅报的「连接关闭」不是故障：用这个标志区分。
    // 否则 UI 会把它当一轮失败，收尾时再发一条 `RefreshSessions`，
    // 把刚睡下的 host 立刻叫起来（实测确实这么打转过）。
    let shutting_down = Arc::new(AtomicBool::new(false));
    follow_workspaces(
        Arc::clone(&conn),
        events.clone(),
        Arc::clone(&registry),
        Arc::clone(&shutting_down),
    );

    // 命令循环：空闲到点就主动收掉 host（RAM 只在「有活」时付）。
    let idle = idle_timeout();
    let mut last = tokio::time::Instant::now();

    loop {
        let command = match first.take().or(None) {
            Some(command) => command,
            None => {
                let received = match idle {
                    Some(idle) => tokio::select! {
                        received = commands.recv() => received,
                        () = tokio::time::sleep_until(last + idle) => {
                            tracing::debug!("空闲到点");
                            // 服务端才是权威：有轮在跑就不收。
                            if any_turn_running(&conn).await {
                                last = tokio::time::Instant::now();
                                continue;
                            }
                            let active_id = active.as_ref().map(|current| current.id.clone());
                            // 下一次醒来可能不再是同一个进程：先把侧栏拍下来。
                            save_snapshot(&known, &registry, &sidebar_prefs);
                            shutting_down.store(true, Ordering::Relaxed);
                            // 先停回放任务；socket 随 `conn` 一起 drop。
                            if let Some(current) = active.take() {
                                current.reader.abort();
                            }
                            return Outcome::Sleep { active: active_id };
                        }
                    },
                    None => commands.recv().await,
                };
                match received {
                    Some(command) => command,
                    None => {
                        save_snapshot(&known, &registry, &sidebar_prefs);
                        return Outcome::Closed;
                    }
                }
            }
        };
        last = tokio::time::Instant::now();
        tracing::debug!(kind = command.kind(), "收到命令");

        // 会改变侧栏的命令才落盘（输字引发的 `Complete` 不写）。
        let persist = touches_sidebar(&command);

        match command {
            UiCommand::Prompt(text) => {
                let Some(session) = active.as_ref().map(|current| current.id.clone()) else {
                    continue;
                };
                // 斜杠命令不是提示：喂给 model 只会换来一次真实问答（实测 425 帧
                // assistant-stream 且 preset 不变）。走 commands/execute。
                if text.trim_start().starts_with('/') {
                    let executed = conn
                        .call::<serde_json::Value>(
                            "commands/execute",
                            json!({
                                "agentId": session,
                                "line": text.trim(),
                                "submittedAttachments": [],
                            }),
                        )
                        .await;
                    match executed {
                        Ok(value) if value.is_null() => {
                            let _ = events
                                .send(UiEvent::Failed(format!(
                                    "host 不认这条命令: {}",
                                    text.trim()
                                )))
                                .await;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = events
                                .send(UiEvent::Failed(format!("命令执行失败: {error}")))
                                .await;
                        }
                    }
                    known =
                        refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str()))
                            .await;
                    continue;
                }
                let call = conn.call::<serde_json::Value>(
                    "session/prompt",
                    json!({
                        "request": {
                            "requestId": uuid::Uuid::new_v4().to_string(),
                            "sessionId": session,
                            "mode": "queue",
                            "content": [{ "type": "text", "text": text }],
                        }
                    }),
                );
                if let Err(error) = call.await {
                    let _ = events
                        .send(UiEvent::Failed(format!("投递失败: {error}")))
                        .await;
                }
                known =
                    refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str())).await;
            }
            UiCommand::OpenSession(id) => {
                let cwd = known
                    .iter()
                    .find(|item| item.id == id)
                    .map(|item| item.cwd.clone())
                    .unwrap_or_default();
                switch_to(&conn, &events, &mut active, &id, &cwd).await;
                known =
                    refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str())).await;
                refresh_context(&conn, &events, &id).await;
            }
            UiCommand::SetMode(mode) => {
                let Some(session) = active.as_ref().map(|current| current.id.clone()) else {
                    continue;
                };
                // 官方 UI 走 `commands/execute` 提交斜杠命令；直接喂 session/prompt
                // 会被当成普通提示发给模型（实测退化成一次真实问答）。
                let executed = conn
                    .call::<serde_json::Value>(
                        "commands/execute",
                        json!({
                            "agentId": session,
                            "line": format!("/permission {mode}"),
                            "submittedAttachments": [],
                        }),
                    )
                    .await;
                match executed {
                    Ok(value) => {
                        if value.is_null() {
                            let _ = events
                                .send(UiEvent::Failed("host 没有 /permission 命令".to_owned()))
                                .await;
                        }
                    }
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("切换 mode 失败: {error}")))
                            .await;
                    }
                }
                known =
                    refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str())).await;
            }
            // 停轮：`session/cancel` 只取消当前这一轮，排队消息（inbox）不动。
            UiCommand::Cancel { session_id } => {
                let cancelled = conn
                    .call::<serde_json::Value>(
                        "session/cancel",
                        json!({ "request": { "sessionId": session_id } }),
                    )
                    .await;
                match cancelled {
                    Ok(_) => {
                        // 真正收尾还靠 `turn/end`；这里顺手把侧栏的运行点刷一下。
                        refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str()))
                            .await;
                    }
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("停止失败: {error}")))
                            .await;
                    }
                }
            }
            // 侧栏的运行点：轮次结束/切换会话后重读一次 `session/list`。
            UiCommand::RefreshSessions => {
                refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str())).await;
            }
            // 补全：`/` 查命令目录，`@` 查工作区文件。两者都按当前会话取作用域。
            UiCommand::Complete { kind, query, seq } => {
                let Some(session) = active.as_ref().map(|current| current.id.clone()) else {
                    let _ = events
                        .send(UiEvent::Completions {
                            seq,
                            items: Vec::new(),
                        })
                        .await;
                    continue;
                };
                let items = match kind {
                    completion::Kind::Command => list_commands(&conn, &session, &query).await,
                    completion::Kind::File => search_files(&conn, &session, &query).await,
                };
                // 补全的命中数就靠这一行查（界面上只看得到前 8 行）。
                tracing::debug!(
                    kind = ?kind,
                    %query,
                    count = items.len(),
                    first = items.first().map(|item| item.label.as_str()).unwrap_or("-"),
                    "补全结果"
                );
                let _ = events.send(UiEvent::Completions { seq, items }).await;
            }
            UiCommand::SetChatVerbosity {
                verbosity,
                previous,
            } => {
                write_chat_verbosity(&conn, &events, verbosity, previous).await;
            }
            UiCommand::AddProvider {
                route,
                display_name,
                api,
                base_url,
                api_key,
                model_ids,
            } => {
                let snapshot = model_settings::describe(&conn).await;
                let result: Result<(), String> = async {
                    let snapshot = snapshot?;
                    if !snapshot.writable {
                        return Err("host profile 不接受设置写入".to_owned());
                    }
                    // 先存 key 再写 profile：写 profile 失败只残留一个未引用的 key（无害）；
                    // 反过来会落盘一个「已配置却无凭据」的 provider，UI 还没有编辑入口修。
                    let with_key = !api_key.is_empty();
                    if with_key {
                        model_settings::store_credential(&conn, &route, &api_key).await?;
                    }
                    let base = dsh_protocol::models::ProviderProfile {
                        api,
                        base_url: (!base_url.is_empty()).then_some(base_url),
                        // 留空 key 时不写一个用户看不见的引用名，否则请求会以
                        // MISSING_CREDENTIAL 失败；留空即走已存凭据或环境变量。
                        api_key_env: with_key.then(|| model_settings::credential_ref(&route)),
                        display_name: (!display_name.is_empty()).then_some(display_name),
                        models: Vec::new(),
                    };
                    let profile = model_settings::append_models(&base, &model_ids);
                    model_settings::write_provider(&conn, &route, &profile, snapshot.revision)
                        .await?;
                    Ok(())
                }
                .await;
                match result {
                    Ok(()) => tracing::info!(%route, "已添加 provider"),
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("添加 provider 失败: {error}")))
                            .await;
                    }
                }
                refresh_model_settings(&conn, &events).await;
            }
            UiCommand::RemoveProvider { route } => {
                let snapshot = model_settings::describe(&conn).await;
                let result: Result<(), String> = async {
                    let snapshot = snapshot?;
                    model_settings::remove_provider(&conn, &route, snapshot.revision).await?;
                    // 凭据删除失败不阻断 profile 删除：它只是残留一个未引用的 key。
                    if let Err(error) = model_settings::remove_credential(&conn, &route).await {
                        tracing::warn!(%error, %route, "删除凭据失败（profile 已删）");
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    let _ = events
                        .send(UiEvent::Failed(format!("删除 provider 失败: {error}")))
                        .await;
                }
                refresh_model_settings(&conn, &events).await;
            }
            UiCommand::DiscoverModels {
                base_url,
                api,
                api_key,
                seq,
            } => {
                let found = model_settings::discover_models(
                    &conn,
                    model_settings::LLM_PI_AI_NS,
                    &base_url,
                    &api,
                    &api_key,
                )
                .await;
                let event = match found {
                    Ok(models) => UiEvent::ModelDiscovery {
                        seq,
                        choices: models
                            .into_iter()
                            .map(|model| DiscoveredChoice {
                                id: model.id,
                                name: model.name,
                            })
                            .collect(),
                        error: None,
                    },
                    Err(error) => UiEvent::ModelDiscovery {
                        seq,
                        choices: Vec::new(),
                        error: Some(error),
                    },
                };
                let _ = events.send(event).await;
            }
            UiCommand::AddModel { route, model_ids } => {
                match model_settings::add_model(&conn, &route, &model_ids).await {
                    Ok(()) => tracing::info!(%route, "已追加模型"),
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("添加模型失败: {error}")))
                            .await;
                    }
                }
                refresh_model_settings(&conn, &events).await;
            }
            UiCommand::SetModel {
                provider,
                model,
                effort,
            } => {
                let Some(session) = active.as_ref().map(|current| current.id.clone()) else {
                    continue;
                };
                // 形参键是 `request`（不是 `_request`），与 session/list 相反。
                // `reasoningEffort` 是可选的**字符串**，不是可空字段：没有等级时
                // 必须整条省略，发 `null` 会被 host 的边界校验拒成
                // `gateway/input-invalid`（目录里未声明推理等级的模型就会踩到）。
                let mut request = json!({
                    "sessionId": session,
                    "provider": provider,
                    "model": model,
                });
                if let Some(effort) = effort {
                    request["reasoningEffort"] = json!(effort);
                }
                let selected = conn
                    .call::<serde_json::Value>("session/selectModel", json!({ "request": request }))
                    .await;
                if let Err(error) = selected {
                    let _ = events
                        .send(UiEvent::Failed(format!("切换模型失败: {error}")))
                        .await;
                }
                known =
                    refresh_sessions(&conn, &events, active.as_ref().map(|a| a.id.as_str())).await;
            }
            // 「添加目录」：把选中的目录注册成工作区。`workspace/create` 只认
            // 已有目录（幂等），目录选择器由界面自己弹（官方是 host 端弹）。
            UiCommand::AddWorkspace { path } => {
                let registered = conn
                    .call::<WorkspaceCreated>(
                        "workspace/create",
                        json!({ "request": { "path": path } }),
                    )
                    .await;
                let created = match registered {
                    Ok(created) => created,
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("添加工作区失败: {error}")))
                            .await;
                        continue;
                    }
                };
                let cwd = created.workspace.path.clone();
                let workspace_id = created.workspace.workspace_id.clone();
                // `workspace/create` 按 canonical path 幂等：同一目录重新添加时
                // `created=false`，host 注册与账本原封不动。取消客户端隐藏即可，
                // 老会话随账本一起回到侧栏；只有真正新建才需要后续收养。
                if !created.created {
                    update_sidebar_prefs(&events, &sidebar_prefs, |prefs| {
                        prefs.hidden_workspace_ids.remove(&workspace_id);
                    });
                }
                // 注册了目录却没有会话就看不见它，所以开一个空白会话。
                // 传 `workspaceId` 而不是 `cwd`：host 只有在前者时才会写账本，
                // 否则新会话会在账本分组下掉进「未分组」。
                match conn
                    .call::<SessionCreateValue>(
                        "session/create",
                        json!({ "request": { "workspaceId": workspace_id } }),
                    )
                    .await
                {
                    Ok(session) => {
                        switch_to(&conn, &events, &mut active, &session.session_id, &cwd).await;
                        known = refresh_sessions(&conn, &events, Some(&session.session_id)).await;
                    }
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("建会话失败: {error}")))
                            .await;
                    }
                }
            }
            UiCommand::SaveSidebarPrefs(next) => {
                let mut guard = sidebar_prefs
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *guard = next;
            }
            UiCommand::ArchiveSession {
                session_id,
                stop_activity,
            } => {
                archive_session(
                    &conn,
                    &events,
                    &registry,
                    ArchiveSessionRequest {
                        session_id: &session_id,
                        stop_activity,
                    },
                )
                .await;
            }
            UiCommand::UnarchiveSession { session_id } => {
                unarchive_session(&conn, &events, &registry, &session_id).await;
            }
            UiCommand::NewSession { cwd, workspace_id } => {
                match conn
                    .call::<SessionCreateValue>(
                        "session/create",
                        json!({ "request": { "workspaceId": workspace_id } }),
                    )
                    .await
                {
                    Ok(created) => {
                        switch_to(&conn, &events, &mut active, &created.session_id, &cwd).await;
                        known = refresh_sessions(&conn, &events, Some(&created.session_id)).await;
                    }
                    Err(error) => {
                        let _ = events
                            .send(UiEvent::Failed(format!("建会话失败: {error}")))
                            .await;
                    }
                }
            }
        }

        // 侧栏状态变了才拍快照：~20KB 一串 JSON，原子替换写。
        if persist {
            save_snapshot(&known, &registry, &sidebar_prefs);
        }
    }
}

/// 改侧栏私有偏好，并把权威值回送界面（界面自己也会乐观改，这里保证重连后一致）。
fn update_sidebar_prefs(
    events: &Sender<UiEvent>,
    prefs: &Arc<Mutex<SidebarPrefs>>,
    change: impl FnOnce(&mut SidebarPrefs),
) {
    let next = {
        let mut guard = prefs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        change(&mut guard);
        guard.clone()
    };
    let _ = events.try_send(UiEvent::SidebarPrefs(next));
}

/// 这条命令会对侧栏产生影响吗（新建/切会话、工作区增删、归档、一轮结束刷列表）。
///
/// `Complete` 不写：打字时它每 130ms 就来一条，写盘是纯浪费。
fn touches_sidebar(command: &UiCommand) -> bool {
    matches!(
        command,
        UiCommand::Prompt(_)
            | UiCommand::OpenSession(_)
            | UiCommand::NewSession { .. }
            | UiCommand::AddWorkspace { .. }
            | UiCommand::SaveSidebarPrefs(_)
            | UiCommand::ArchiveSession { .. }
            | UiCommand::UnarchiveSession { .. }
            | UiCommand::RefreshSessions
            | UiCommand::SetMode(_)
            | UiCommand::SetModel { .. }
    )
}

/// 把当前侧栏状态写成本地快照。
fn save_snapshot(
    known: &[dsh_client_ui::SessionInfo],
    registry: &Arc<Mutex<WorkspaceRegistry>>,
    prefs: &Arc<Mutex<SidebarPrefs>>,
) {
    let registry = match registry.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let prefs = match prefs.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    cache::save(&cache::Snapshot::capture(known, &registry, &prefs));
    tracing::debug!(
        sessions = known.len(),
        workspaces = registry.items.len(),
        "侧栏快照已写"
    );
}

/// 切到某个会话：掐掉旧流、重开 follow，把快照里的历史回放成时间线。
async fn switch_to(
    conn: &Arc<Connection>,
    events: &Sender<UiEvent>,
    active: &mut Option<ActiveSession>,
    session_id: &str,
    cwd: &str,
) {
    if let Some(previous) = active.take() {
        previous.reader.abort();
    }

    // 先订阅再投递：反过来会丢开头几帧（PROTOCOL.md「四步链」）。
    let mut stream = match FollowStream::open(
        conn,
        "session/follow",
        json!({
            "request": {
                "address": { "kind": "session", "sessionId": session_id },
                "assistantStream": true,
                "activate": true,
                "maxMessages": 60,
            }
        }),
    )
    .await
    {
        Ok(stream) => stream,
        Err(error) => {
            let _ = events
                .send(UiEvent::Failed(format!("订阅失败: {error}")))
                .await;
            return;
        }
    };

    let _ = events
        .send(UiEvent::Connected {
            origin: conn.origin().trim_end_matches('/').to_owned(),
            session: session_id.to_owned(),
            cwd: cwd.to_owned(),
        })
        .await;

    let reader_events = events.clone();
    // reader 需要在拿到 usage 帧时补一次投影读取，所以要带一份连接。
    let reader_conn = Arc::clone(conn);
    let reader_session = session_id.to_owned();
    let reader = tokio::spawn(async move {
        // 实时工具行也走同一个折叠器：`tool/call` 插行、`tool/result` 原地补。
        let mut live = Transcript::new();
        while let Ok(Some(item)) = stream.next_item().await {
            match item.kind {
                StreamItemKind::Item => {
                    let Some(value) = item.value else { continue };
                    let Ok(parsed) = serde_json::from_value::<FollowValue>(value) else {
                        continue;
                    };
                    match parsed {
                        FollowValue::Snapshot(snapshot) => {
                            let items = transcript_from_records(&snapshot.records);
                            let _ = reader_events.send(UiEvent::Transcript(items)).await;
                        }
                        FollowValue::Event(event) => {
                            // 只推工具行：用户/助手消息由快照与流式帧负责，
                            // 否则同一个事件会在界面上出现两次。
                            for index in live.push_event(&event.event) {
                                if let Some(Entry::Tool(call)) = live.entry(index) {
                                    let _ = reader_events.send(UiEvent::Tool(call.clone())).await;
                                }
                            }
                        }
                        FollowValue::AssistantStream(value) => {
                            let used = matches!(
                                &value.frame,
                                AssistantStreamFrame::Chunk(frame)
                                    if matches!(frame.chunk, StreamChunk::Usage { .. })
                            );
                            let _ = reader_events.send(UiEvent::Frame(value.frame)).await;
                            if used {
                                // `contextPressure` 不在 session/list 的投影摘要里，
                                // 每轮拿到 provider 的用量后单独补读一次。
                                let conn = Arc::clone(&reader_conn);
                                let events = reader_events.clone();
                                let session = reader_session.clone();
                                tokio::spawn(async move {
                                    refresh_context(&conn, &events, &session).await;
                                });
                            }
                        }
                        _ => {}
                    }
                }
                StreamItemKind::Error => {
                    let detail = item
                        .error
                        .map(|error| format!("{}: {}", error.code, error.message))
                        .unwrap_or_default();
                    let _ = reader_events
                        .send(UiEvent::Failed(format!("流失败 {detail}")))
                        .await;
                    break;
                }
                StreamItemKind::End => break,
            }
        }
    });

    *active = Some(ActiveSession {
        id: session_id.to_owned(),
        reader,
    });
}

/// 归档一条会话：回的是**整份**归档集合，直接当基线用。
async fn archive_session(
    conn: &Connection,
    events: &Sender<UiEvent>,
    registry: &Arc<Mutex<WorkspaceRegistry>>,
    request: ArchiveSessionRequest<'_>,
) {
    match conn
        .call::<WorkspaceArchiveValue>("workspace/archiveSession", json!({ "request": request }))
        .await
    {
        Ok(value) => publish_archived(events, registry, value.archived_session_ids).await,
        Err(error) => {
            let _ = events
                .send(UiEvent::Failed(format!("归档失败: {error}")))
                .await;
        }
    }
}

/// 取消归档；同样回整份集合。
async fn unarchive_session(
    conn: &Connection,
    events: &Sender<UiEvent>,
    registry: &Arc<Mutex<WorkspaceRegistry>>,
    session_id: &str,
) {
    match conn
        .call::<WorkspaceArchiveValue>(
            "workspace/unarchiveSession",
            json!({ "request": { "sessionId": session_id } }),
        )
        .await
    {
        Ok(value) => publish_archived(events, registry, value.archived_session_ids).await,
        Err(error) => {
            let _ = events
                .send(UiEvent::Failed(format!("取消归档失败: {error}")))
                .await;
        }
    }
}

/// 把归档集合写回共享注册表再整份推给侧栏。
///
/// 关键：**自己发起的归档/取消归档不会变成 `workspace/follow` 的增量帧**
/// （host 只把别人造成的变更推过来），所以必须由调用方把回执写回去；
/// 否则下一次无关的帧会带着旧的归档集合整份覆盖，侧栏就跳回上一个状态。
async fn publish_archived(
    events: &Sender<UiEvent>,
    registry: &Arc<Mutex<WorkspaceRegistry>>,
    archived_session_ids: Vec<String>,
) {
    let snapshot = {
        // 锁中毒时照旧取里面的值：这份注册表是可重建的缓存，不值得因一次 panic 卡死。
        let mut guard = match registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.archived_session_ids = archived_session_ids;
        guard.clone()
    };
    let _ = events.send(UiEvent::Workspaces(snapshot)).await;
}

/// 跟工作区注册表：基线 + 增量都转成 `UiEvent::Workspaces`。
///
/// 侧栏分组的「有哪些组」以它为准（`workspace/delete` 删掉注册后，它的会话
/// 会落到「未分组」）。流断了就重连，但保留上一份列表，不把侧栏清空。
fn follow_workspaces(
    conn: Arc<Connection>,
    events: Sender<UiEvent>,
    registry: Arc<Mutex<WorkspaceRegistry>>,
    shutting_down: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            match FollowStream::open(&conn, "workspace/follow", json!({})).await {
                Ok(mut stream) => {
                    while let Ok(Some(item)) = stream.next_item().await {
                        tracing::debug!(?item.kind, "workspace/follow 帧");
                        match item.kind {
                            StreamItemKind::Item => {
                                let Some(value) = item.value else { continue };
                                let Ok(frame) =
                                    serde_json::from_value::<WorkspaceFollowFrame>(value)
                                else {
                                    continue;
                                };
                                let snapshot = {
                                    let mut guard = match registry.lock() {
                                        Ok(guard) => guard,
                                        Err(poisoned) => poisoned.into_inner(),
                                    };
                                    guard.apply(frame).then(|| guard.clone())
                                };
                                if let Some(snapshot) = snapshot {
                                    let _ = events.send(UiEvent::Workspaces(snapshot)).await;
                                }
                            }
                            StreamItemKind::Error => break,
                            StreamItemKind::End => return,
                        }
                    }
                }
                Err(error) => {
                    if shutting_down.load(Ordering::Relaxed) {
                        return;
                    }
                    let _ = events
                        .send(UiEvent::Failed(format!("工作区订阅失败: {error}")))
                        .await;
                }
            }
            if shutting_down.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if shutting_down.load(Ordering::Relaxed) {
                return;
            }
            tracing::debug!("workspace/follow 重开");
        }
    });
}

/// 读一遍会话列表推给侧栏，同时把结果留给调用方做 cwd 查询。
async fn refresh_sessions(
    conn: &Connection,
    events: &Sender<UiEvent>,
    active: Option<&str>,
) -> Vec<SessionInfo> {
    let list: SessionListValue = match conn.call("session/list", json!({ "_request": {} })).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "会话列表读取失败");
            return Vec::new();
        }
    };

    let items: Vec<SessionInfo> = list
        .items
        .into_iter()
        .map(|item| {
            // 先把借用字段全部取成 owned，再移动 `session_id`。
            let title = item.title().map(str::to_owned);
            let cwd = item.working_directory().unwrap_or_default().to_owned();
            let mode = item.permission_preset().map(str::to_owned);
            let model = item
                .model_selection()
                .map(|(provider, model, effort)| SelectedModel {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    effort: effort.map(str::to_owned),
                });
            SessionInfo {
                id: item.session_id,
                title,
                cwd,
                updated_at: item.updated_at,
                running: item.running,
                blank: item.blank,
                mode,
                model,
            }
        })
        .collect();

    let _ = events.send(UiEvent::Sessions(items.clone())).await;

    // 当前 preset 由 `SessionInfo::mode` 一起带出；`active` 只用于日志定位。
    if let Some(item) = active.and_then(|active| items.iter().find(|item| item.id == active)) {
        tracing::debug!(session = %item.id, mode = ?item.mode, "当前会话投影");
    }
    items
}

/// 补全最多给几条候选。
const COMPLETION_LIMIT: usize = 8;

/// 斜杠命令候选：`commands/list` 只列当前 agent 可见的命令。
async fn list_commands(conn: &Connection, session: &str, query: &str) -> Vec<CompletionItem> {
    let listed = conn
        .call::<Vec<CommandDescriptor>>("commands/list", json!({ "agentId": session }))
        .await;
    let commands = match listed {
        Ok(commands) => commands,
        Err(error) => {
            tracing::warn!(%error, "命令目录读取失败");
            return Vec::new();
        }
    };

    let needle = query.to_ascii_lowercase();
    let mut items: Vec<CompletionItem> = commands
        .into_iter()
        .filter(|command| command.name.to_ascii_lowercase().contains(&needle))
        .map(|command| CompletionItem {
            label: format!("/{}", command.name),
            hint: command
                .description
                .or_else(|| command.input.and_then(|input| input.hint))
                .unwrap_or_default(),
            insert: completion::command_insert(&command.name),
        })
        .collect();
    // 前缀命中的排前面，其余按包含命中跟在后面。
    items.sort_by_key(|item| {
        let name = item.label.trim_start_matches('/').to_ascii_lowercase();
        (!name.starts_with(&needle), name)
    });
    items.truncate(COMPLETION_LIMIT);
    items
}

/// `@` 的文件候选：host 用打包的 ripgrep 在工作区里按名/按路径找。
async fn search_files(conn: &Connection, session: &str, query: &str) -> Vec<CompletionItem> {
    // `@` 一个字符就要有东西：官方在这里换成「列当前目录」，而不是等用户再打字。
    let (candidates, fragment) = match plan_lookup(query) {
        FileLookup::Directory { path, fragment } => {
            (list_directory(conn, session, &path).await, fragment)
        }
        FileLookup::Search(text) => {
            let found = conn
                .call::<FileSearchResult>(
                    "workspaceFiles/search",
                    json!({
                        "workspaceFileScopeId": session,
                        "request": FileSearchRequest::new(&text),
                    }),
                )
                .await;
            match found {
                Ok(result) => (
                    result
                        .matches
                        .into_iter()
                        .map(|hit| FileCandidate {
                            path: hit.path,
                            directory: hit.kind.as_deref() == Some("directory"),
                        })
                        .collect(),
                    text,
                ),
                Err(error) => {
                    tracing::warn!(%error, "工作区检索失败");
                    (Vec::new(), String::new())
                }
            }
        }
    };

    // 隐藏项与构建目录不给（官方同款规则）：除非用户自己就在打 `.`，否则
    // `target/`、`node_modules/` 这种会白占前几行。
    let want_hidden = fragment.starts_with('.') || fragment.contains("/.");
    let visible: Vec<FileCandidate> = candidates
        .into_iter()
        .filter(|candidate| {
            !is_excluded_path(&candidate.path)
                && (want_hidden || !has_hidden_segment(&candidate.path))
        })
        .collect();

    rank(visible, &fragment, COMPLETION_LIMIT)
        .into_iter()
        .filter_map(|candidate| {
            let insert = completion::file_insert(&candidate.path, candidate.directory, false)?;
            Some(CompletionItem {
                label: candidate.path,
                hint: if candidate.directory {
                    "目录".to_owned()
                } else {
                    String::new()
                },
                insert,
            })
        })
        .collect()
}

/// 列一个目录的直接子项（`workspaceFiles/list`）。根目录的 path 是 `.`，
/// 空串会被 host 拒（`gateway/bad-request: path is required`）。
async fn list_directory(conn: &Connection, session: &str, path: &str) -> Vec<FileCandidate> {
    let listed = conn
        .call::<DirectoryListing>(
            "workspaceFiles/list",
            json!({ "workspaceFileScopeId": session, "path": path }),
        )
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => {
            // 目录不存在、在 gitignore 之外都不算错，只是没有候选。
            tracing::debug!(%error, %path, "列目录失败");
            return Vec::new();
        }
    };
    listed
        .entries
        .into_iter()
        .filter(|entry| entry.kind != "other")
        .map(|entry| FileCandidate {
            path: if listed.path.is_empty() {
                entry.name
            } else {
                format!("{}/{}", listed.path, entry.name)
            },
            directory: entry.kind == "directory",
        })
        .collect()
}

/// 读一次上下文压力与累计用量，推给状态行。
///
/// `contextPressure` 标了 `SessionProjectionSummaryExcluded`，`session/list` 里没有它，
/// 所以只能单独打 `session/projections`（形参键是 `request`）。
async fn refresh_context(conn: &Connection, events: &Sender<UiEvent>, session_id: &str) {
    let value = conn
        .call::<SessionProjectionsValue>(
            "session/projections",
            json!({ "request": { "sessionId": session_id } }),
        )
        .await;

    let value = match value {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "上下文投影读取失败");
            return;
        }
    };

    let occupancy = value
        .values
        .context_pressure
        .as_ref()
        .and_then(|pressure| pressure.occupancy());
    let cache_hit = value
        .values
        .token_usage
        .as_ref()
        .and_then(|usage| usage.cache_hit_percent());

    let _ = events
        .send(UiEvent::Context {
            occupancy,
            cache_hit,
        })
        .await;
}

/// 读一次模型目录推给界面（启动时一次就够）。
async fn load_models(conn: &Connection, events: &Sender<UiEvent>) {
    let catalog = match conn
        .call::<ModelCatalog>("session/modelCatalog", json!({}))
        .await
    {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "模型目录读取失败");
            return;
        }
    };

    let failures = catalog.failures.len();
    if failures > 0 {
        tracing::warn!(failures, "有提供方的模型目录拉取失败");
    }
    // 官方 `warning.groupLoad` 的文案形状：`{name} 加载失败：{message}`。
    // 形状随 provider 变化，取不到就退回 id / 原始 JSON，不丢信息。
    let failure_rows: Vec<String> = catalog
        .failures
        .iter()
        .map(|failure| {
            let name = failure
                .get("name")
                .or_else(|| failure.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("提供方");
            let message = failure
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| failure.to_string());
            format!("{name} 加载失败：{message}")
        })
        .collect();

    // 官方 `orderModelProviders`：DeepSeek 账号第一、DeepSeek 第二，其余保持目录顺序。
    let mut groups: Vec<ModelGroup> = catalog
        .groups
        .into_iter()
        .map(|group| ModelGroup {
            provider: group.id,
            provider_name: group.name,
            models: group
                .models
                .into_iter()
                .map(|model| ModelOption {
                    id: model.id,
                    name: model.name,
                    default_effort: model
                        .reasoning
                        .as_ref()
                        .and_then(|reasoning| reasoning.default_effort.clone()),
                    efforts: model
                        .reasoning
                        .map(|reasoning| {
                            reasoning
                                .efforts
                                .into_iter()
                                .map(|effort| EffortOption {
                                    id: effort.id,
                                    name: effort.name,
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
        })
        .collect();
    groups.sort_by_key(|group| provider_rank(&group.provider));

    let default = SelectedModel {
        provider: catalog.default.provider,
        model: catalog.default.model,
        effort: catalog.default.reasoning_effort,
    };

    let _ = events
        .send(UiEvent::Models {
            groups,
            default: Some(default),
            failures: failure_rows,
        })
        .await;
}

/// 把快照映射成 UI 行的纯函数（可单测）：目录里的 provider + 已声明但目录里没有的路由。
fn snapshot_to_settings(snapshot: &model_settings::ModelSettingsSnapshot) -> Vec<ProviderSetting> {
    let mut providers: Vec<ProviderSetting> = snapshot
        .providers
        .iter()
        .map(|provider| {
            let cred = snapshot.credentials.get(&provider.provider);
            let models = snapshot
                .configured
                .get(&provider.provider)
                .map(|profile| {
                    profile
                        .models
                        .iter()
                        .map(|model| model.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ProviderSetting {
                id: provider.provider.clone(),
                display_name: provider.label().to_owned(),
                declared: provider.declared || snapshot.configured.contains_key(&provider.provider),
                default_base_url: provider.default_base_url.clone(),
                key_configured: cred.is_some_and(|info| info.configured),
                key_source: cred.and_then(|info| info.source.clone()),
                models,
            }
        })
        .collect();
    // 已声明但目录里没有的自定义路由也要显示（自定网关）。
    for (route, profile) in &snapshot.configured {
        if providers.iter().any(|item| item.id == *route) {
            continue;
        }
        providers.push(ProviderSetting {
            id: route.clone(),
            display_name: profile
                .display_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| route.clone()),
            declared: true,
            default_base_url: profile.base_url.clone(),
            key_configured: snapshot
                .credentials
                .get(route)
                .is_some_and(|info| info.configured),
            key_source: snapshot
                .credentials
                .get(route)
                .and_then(|info| info.source.clone()),
            models: profile
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect(),
        });
    }
    providers.sort_by(|a, b| {
        provider_rank(&a.id)
            .cmp(&provider_rank(&b.id))
            .then_with(|| a.id.cmp(&b.id))
    });
    providers
}

async fn refresh_model_settings(conn: &Connection, events: &Sender<UiEvent>) {
    let event = match model_settings::describe(conn).await {
        Ok(snapshot) => UiEvent::ModelSettings {
            providers: snapshot_to_settings(&snapshot),
            writable: snapshot.writable,
            error: None,
        },
        Err(error) => {
            tracing::warn!(%error, "读模型设置失败");
            UiEvent::ModelSettings {
                providers: Vec::new(),
                writable: false,
                error: Some(error),
            }
        }
    };
    let _ = events.send(event).await;
}

/// 官方 `orderModelProviders` 的排序键。
fn provider_rank(provider: &str) -> u8 {
    match provider {
        "deepseek-account" => 0,
        "deepseek-official" => 1,
        _ => 2,
    }
}

/// 读一次权限 preset 目录推给界面（启动时一次就够）。
async fn load_presets(conn: &Connection, events: &Sender<UiEvent>) {
    match conn
        .call::<PermissionCatalog>("permissionPresets/catalog", json!({}))
        .await
    {
        Ok(catalog) => {
            let options: Vec<PresetInfo> = catalog
                .options
                .into_iter()
                .map(|option| PresetInfo {
                    value: option.value,
                    name: option.name,
                })
                .collect();
            let _ = events.send(UiEvent::Presets(options)).await;
        }
        Err(error) => tracing::warn!(%error, "权限目录读取失败"),
    }
}

/// 把 follow 快照里的持久事件还原成时间线。
///
/// 具体规则（用户侧只留真人说的、`reasoning` 不显示、工具 call/result 配对）
/// 全在 [`dsh_protocol::transcript`] 里；这里只是把它接到快照上——
/// 历史回放与实时流共用一个折叠器，才不会出现「刚跑完的行」与「重开后的行」两个样。
fn transcript_from_records(records: &[serde_json::Value]) -> Vec<Entry> {
    Transcript::from_records(records).into_entries()
}

#[cfg(test)]
mod model_settings_view_tests {
    use super::*;
    use dsh_protocol::models::{
        ConfigurableProvider, CredentialInfo, ProviderModel, ProviderProfile,
    };
    use std::collections::HashMap;

    fn provider(id: &str, declared: bool) -> ConfigurableProvider {
        ConfigurableProvider {
            provider: id.to_owned(),
            display_name: format!("Name-{id}"),
            settings_ns: "llm-pi-ai".to_owned(),
            settings_path: vec!["providers".to_owned(), id.to_owned()],
            declared,
            default_base_url: None,
            base_url_env: vec![],
        }
    }

    #[test]
    fn maps_directory_and_marks_declared_and_credentials() {
        let mut credentials = HashMap::new();
        credentials.insert(
            "acme".to_owned(),
            CredentialInfo {
                configured: true,
                writable: true,
                masked: None,
                source: Some("file".to_owned()),
            },
        );
        let mut configured = HashMap::new();
        configured.insert(
            "acme".to_owned(),
            ProviderProfile {
                api: "openai-completions".to_owned(),
                base_url: None,
                api_key_env: None,
                display_name: None,
                models: vec![ProviderModel {
                    id: "m1".to_owned(),
                    ..ProviderModel::default()
                }],
            },
        );
        let snapshot = model_settings::ModelSettingsSnapshot {
            providers: vec![
                provider("deepseek-official", false),
                provider("acme", false),
            ],
            configured,
            credentials,
            revision: 3,
            writable: true,
        };
        let rows = snapshot_to_settings(&snapshot);
        // DeepSeek 排前，acme 排后（provider_rank）。
        assert_eq!(rows[0].id, "deepseek-official");
        assert_eq!(rows[1].id, "acme");
        assert!(rows[0].display_name.starts_with("Name-"));
        assert!(!rows[0].declared);
        assert!(rows[1].declared, "profile 里已写入 → 标记为已配置");
        assert!(rows[1].key_configured);
        assert_eq!(rows[1].key_source.as_deref(), Some("file"));
        assert_eq!(rows[1].models, ["m1"]);
    }

    #[test]
    fn includes_configured_route_missing_from_directory() {
        let mut configured = HashMap::new();
        configured.insert(
            "my-gateway".to_owned(),
            ProviderProfile {
                api: "openai-completions".to_owned(),
                base_url: Some("https://my/v1".to_owned()),
                api_key_env: None,
                display_name: Some("My GW".to_owned()),
                models: vec![],
            },
        );
        let snapshot = model_settings::ModelSettingsSnapshot {
            providers: vec![],
            configured,
            credentials: HashMap::new(),
            revision: 0,
            writable: true,
        };
        let rows = snapshot_to_settings(&snapshot);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "my-gateway");
        assert_eq!(rows[0].display_name, "My GW");
        assert_eq!(rows[0].default_base_url.as_deref(), Some("https://my/v1"));
        assert!(rows[0].declared);
    }

    #[test]
    fn blank_display_name_falls_back_to_route() {
        let mut configured = HashMap::new();
        configured.insert(
            "gw".to_owned(),
            ProviderProfile {
                api: "openai-completions".to_owned(),
                base_url: None,
                api_key_env: None,
                display_name: Some(String::new()),
                models: vec![],
            },
        );
        let snapshot = model_settings::ModelSettingsSnapshot {
            providers: vec![],
            configured,
            credentials: HashMap::new(),
            revision: 0,
            writable: true,
        };
        assert_eq!(snapshot_to_settings(&snapshot)[0].display_name, "gw");
    }
}
