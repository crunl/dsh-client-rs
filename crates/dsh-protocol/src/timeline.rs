//! 聊天时间线的 presentation 投影。
//!
//! 这一层只做一件事：把协议事实（持久事件、live stream、工具结果）投成一组
//! 可直接绘制的 [`TimelineRow`]。它不依赖 GPUI，也不做网络。
//!
//! 三条不变量：
//!
//! 1. 行的身份来自数据，不来自数组下标；assistant 块身份含 `(turn, step, index)`。
//! 2. markdown 是 assistant 正文的角色，不是任意文本的解析器。
//! 3. 工具输入/结果在这里被分类、限长、脱敏；renderer 只负责画。

use crate::fold::{AssistantStreamFold, FoldOutcome};
use crate::stream::{AssistantStreamFrame, TokenUsage};
use crate::transcript::{Entry, MessageId, ToolCall, ToolStatus};
use serde_json::Value;

/// 一个可绘制行的稳定身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RenderId {
    /// 真人输入。
    User(MessageId),
    /// assistant 正文块；`index` 是消息内的内容块下标。
    Assistant {
        /// 消息身份。
        id: MessageId,
        /// 内容块下标。
        index: u64,
    },
    /// assistant reasoning 块；与正文分开，默认弱化显示。
    Reasoning {
        /// 消息身份。
        id: MessageId,
        /// 内容块下标。
        index: u64,
    },
    /// 工具调用；身份是 `callId`。
    Tool(String),
    /// 提示 / 失败；身份由内容与位置派生。
    Notice(u64),
}

impl RenderId {
    /// GPUI `ElementId` 使用的稳定字符串。
    pub fn label(&self) -> String {
        match self {
            Self::User(id) => format!("user:{}", id.label()),
            Self::Assistant { id, index } => format!("assistant:{}:b{index}", id.label()),
            Self::Reasoning { id, index } => format!("reasoning:{}:b{index}", id.label()),
            Self::Tool(id) => format!("tool:{id}"),
            Self::Notice(hash) => format!("notice:{hash}"),
        }
    }
}

/// 工具家族。renderer 按它选图标与版式，不再从名字字符串里猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFamily {
    /// 命令执行。
    Shell,
    /// 文件读取。
    FileRead,
    /// 文件写入 / 编辑。
    FileWrite,
    /// 搜索。
    Search,
    /// 网页访问。
    Web,
    /// Office 处理。
    Office,
    /// 子智能体。
    Subagent,
    /// 未知工具。
    Unknown,
}

impl ToolFamily {
    /// 组头用的中文类别。
    pub fn category(self) -> &'static str {
        match self {
            Self::Shell => "命令",
            Self::FileRead => "读取",
            Self::FileWrite => "编辑",
            Self::Search => "搜索",
            Self::Web => "网页",
            Self::Office => "Office",
            Self::Subagent => "子智能体",
            Self::Unknown => "工具",
        }
    }
}

/// 工具行展开后的一行审计字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDetail {
    /// 字段名，如 `command` / `path`。
    pub label: String,
    /// 已脱敏、已限长的值。
    pub value: String,
}

/// 工具结果的结构化展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResultView {
    /// 没有结果正文。
    None,
    /// 普通文本；renderer 里再限行。
    Text {
        /// 结果正文。
        text: String,
        /// 是否已被协议层截断。
        truncated: bool,
    },
    /// 文件修改；由 edit 工具的 old/new 字符串投影出来。
    Diff {
        /// 目标文件。
        path: String,
        /// 增加行数。
        added: usize,
        /// 删除行数。
        removed: usize,
        /// patch 预览。
        preview: String,
    },
    /// 错误正文。
    Error {
        /// 面向用户的错误摘要。
        text: String,
    },
    /// 带标题的预览。
    Preview {
        /// 预览标题。
        title: String,
        /// 预览文本。
        text: String,
    },
}

impl ToolResultView {
    /// 展开区可显示的纯文本；工具输出永不走 markdown。
    pub fn plain_text(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Text { text, .. } | Self::Error { text } | Self::Preview { text, .. } => {
                Some(text)
            }
            Self::Diff { preview, .. } => Some(preview),
        }
    }

    /// 结果是否为空。
    pub fn is_empty(&self) -> bool {
        self.plain_text().is_none_or(|text| text.is_empty())
    }
}

/// 一条工具调用的显示模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolView {
    /// `callId`。
    pub id: String,
    /// wire 上的工具名。
    pub name: String,
    /// 归类后的工具家族。
    pub family: ToolFamily,
    /// 单行标题：优先 command / path / pattern。
    pub title: String,
    /// 协议层给出的摘要。
    pub summary: String,
    /// 展开后的审计字段，最多 4 行。
    pub details: Vec<ToolDetail>,
    /// 状态。
    pub status: ToolStatus,
    /// 结果正文原始字符数（结果被截断前的长度）。
    pub output_chars: usize,
    /// 耗时（毫秒）；没有时间信息时为 `None`。
    pub elapsed_ms: Option<i64>,
    /// 结果展示。
    pub result: ToolResultView,
}

/// 一个可绘制的时间线行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineRow {
    /// 真人输入；plain 文本。
    User {
        /// 稳定身份。
        id: RenderId,
        /// 原文。
        text: String,
    },
    /// assistant 正文；markdown。
    Assistant {
        /// 稳定身份。
        id: RenderId,
        /// markdown 源文。
        text: String,
        /// 是否还在流式更新。
        streaming: bool,
    },
    /// assistant reasoning；弱化展示，不当作正文。
    Reasoning {
        /// 稳定身份。
        id: RenderId,
        /// 源文。
        text: String,
        /// 是否还在流式更新。
        streaming: bool,
    },
    /// 一次工具调用。
    Tool(ToolView),
    /// 提示 / 失败；plain 文本。
    Notice {
        /// 稳定身份。
        id: RenderId,
        /// 提示正文。
        text: String,
    },
}

/// 一帧 stream 应用后的副作用。UI 用它更新 meter / phase / 日志。
#[derive(Debug, Clone)]
pub struct StreamEffect {
    /// 本帧是否终结了一次尝试。
    pub outcome: Option<FoldOutcome>,
    /// 终结时的 usage；没有 provider 统计时为 `None`。
    pub usage: Option<TokenUsage>,
    /// 终结时可见正文的长度，仅用于日志。
    pub chars: usize,
}

impl StreamEffect {
    fn pending() -> Self {
        Self {
            outcome: None,
            usage: None,
            chars: 0,
        }
    }
}

/// 时间线投影器的全部状态。
///
/// UI 只拿 [`Self::rows`] 的结果绘制；不再自己保存 `entries` / `fold` / 本地 id。
#[derive(Debug, Default)]
pub struct ChatTimeline {
    entries: Vec<Entry>,
    live: AssistantStreamFold,
    local_seq: u64,
}

impl ChatTimeline {
    /// 新建空时间线。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用 follow 快照替换历史。流式累积随之清空。
    pub fn replace_snapshot(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.live = AssistantStreamFold::new();
        self.local_seq = 0;
    }

    /// 本地乐观用户消息。
    pub fn push_user(&mut self, text: String) {
        self.entries.push(Entry::User {
            id: MessageId::local(self.local_seq),
            text,
        });
        self.local_seq += 1;
    }

    /// 提示 / 失败行。
    pub fn push_notice(&mut self, text: String) {
        self.entries.push(Entry::Notice(text));
    }

    /// 按 `callId` upsert 一条工具调用。
    pub fn apply_tool(&mut self, call: ToolCall) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|entry| matches!(entry, Entry::Tool(current) if current.id == call.id))
        {
            let Entry::Tool(current) = existing else {
                return;
            };
            // 内容块 / 日志事件 / 结果三种来源合并，权威来源优先。
            crate::transcript::merge_tool(current, call);
            return;
        }
        self.entries.push(Entry::Tool(call));
    }

    /// 应用一帧流式增量。终结时会把已装配块落成本地 entry 并清空 fold。
    pub fn apply_stream(&mut self, frame: AssistantStreamFrame) -> StreamEffect {
        self.live.apply(&frame);
        let Some(outcome) = self.live.outcome().cloned() else {
            return StreamEffect::pending();
        };

        let usage = self.live.usage().cloned();
        let turn_step = self.live.turn_step();
        let message_id = live_message_id(turn_step);
        let blocks = self.live.blocks().to_vec();
        let mut chars = 0;

        for block in &blocks {
            if block.text.is_empty() {
                continue;
            }
            match block.kind.as_str() {
                "text" => {
                    chars += block.text.chars().count();
                    self.entries.push(Entry::Assistant {
                        id: message_id,
                        index: block.index,
                        text: block.text.clone(),
                    });
                }
                "reasoning" => {
                    self.entries.push(Entry::Reasoning {
                        id: message_id,
                        index: block.index,
                        text: block.text.clone(),
                    });
                }
                "tool-call" => {
                    if let Some(call) = tool_call_from_block(block, turn_step) {
                        self.apply_tool(call);
                    }
                }
                _ => {}
            }
        }
        if outcome == FoldOutcome::Interrupted {
            self.entries.push(Entry::Notice("生成被中断".to_owned()));
        }

        self.live = AssistantStreamFold::new();
        StreamEffect {
            outcome: Some(outcome),
            usage,
            chars,
        }
    }

    /// 当前可绘制的行。live 块按流内顺序追加在历史之后。
    pub fn rows(&self) -> Vec<TimelineRow> {
        let mut rows = Vec::new();
        for entry in &self.entries {
            match entry {
                Entry::User { id, text } => rows.push(TimelineRow::User {
                    id: RenderId::User(*id),
                    text: text.clone(),
                }),
                Entry::Assistant { id, index, text } => rows.push(TimelineRow::Assistant {
                    id: RenderId::Assistant {
                        id: *id,
                        index: *index,
                    },
                    text: text.clone(),
                    streaming: false,
                }),
                Entry::Reasoning { id, index, text } => rows.push(TimelineRow::Reasoning {
                    id: RenderId::Reasoning {
                        id: *id,
                        index: *index,
                    },
                    text: text.clone(),
                    streaming: false,
                }),
                Entry::Tool(call) => rows.push(TimelineRow::Tool(tool_view(call))),
                Entry::Notice(text) => rows.push(TimelineRow::Notice {
                    id: RenderId::Notice(notice_id(text, rows.len())),
                    text: text.clone(),
                }),
            }
        }

        let turn_step = self.live.turn_step();
        for block in self.live.visible_blocks() {
            if block.text.is_empty() {
                continue;
            }
            let message_id = turn_step
                .map(|(turn, step)| MessageId::Turn { turn, step })
                .unwrap_or_else(|| MessageId::local(0));
            match block.kind.as_str() {
                "text" => rows.push(TimelineRow::Assistant {
                    id: RenderId::Assistant {
                        id: message_id,
                        index: block.index,
                    },
                    text: block.text.clone(),
                    streaming: true,
                }),
                "reasoning" => rows.push(TimelineRow::Reasoning {
                    id: RenderId::Reasoning {
                        id: message_id,
                        index: block.index,
                    },
                    text: block.text.clone(),
                    streaming: true,
                }),
                "tool-call" => {
                    if let Some(call) = tool_call_from_block(block, turn_step)
                        && !self.entries.iter().any(
                            |entry| matches!(entry, Entry::Tool(current) if current.id == call.id),
                        )
                    {
                        rows.push(TimelineRow::Tool(tool_view(&call)));
                    }
                }
                _ => {}
            }
        }
        rows
    }
}

fn notice_id(text: &str, index: usize) -> u64 {
    // 提示不需要跨会话稳定；用内容与位置组合即可，避免相同文案互抢 ElementId。
    let mut hash: u64 = 1469598103934665603;
    for byte in text.as_bytes().iter().copied().chain(index.to_le_bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

fn live_message_id(turn_step: Option<(u64, u64)>) -> MessageId {
    match turn_step {
        Some((turn, step)) => MessageId::Turn { turn, step },
        None => MessageId::local(0),
    }
}

fn tool_call_from_block(
    block: &crate::fold::Block,
    turn_step: Option<(u64, u64)>,
) -> Option<ToolCall> {
    let raw = block.raw.as_ref();
    // 未闭合块没有权威 callId：label 通常只是工具名，拿它当身份会让同名调用
    // 互相覆盖，也无法和 tool/call / tool/result 配对。等 block-end 再投影。
    let id = raw
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_owned();
    let name = raw
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .or(block.label.as_deref())
        .unwrap_or("tool")
        .to_owned();
    let (turn, step) = turn_step.unwrap_or((0, 0));
    let arguments = parse_arguments(
        raw.and_then(|value| value.get("arguments"))
            .cloned()
            .unwrap_or_else(|| Value::String(block.text.clone())),
    );
    Some(ToolCall {
        id,
        name: name.clone(),
        summary: crate::transcript::summarize(&name, Some(&arguments)),
        status: ToolStatus::Running,
        detail: String::new(),
        detail_chars: 0,
        arguments,
        authoritative: true,
        meta: Value::Null,
        started_at: None,
        finished_at: None,
        turn,
        step,
    })
}

/// 原始参数可能是 JSON 字符串，也可能是对象；统一解析成值。
pub(crate) fn parse_arguments(value: Value) -> Value {
    match value {
        Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        value => value,
    }
}

/// 工具家族分类。只看工具名，避免正则误判 `TodoWrite` 里的 `Write`。
fn family(name: &str) -> ToolFamily {
    match name {
        "bash" | "pwsh" | "terminal_send" | "bash_persistent" | "Bash" => ToolFamily::Shell,
        "read" | "read_image" | "Read" => ToolFamily::FileRead,
        "write" | "edit" | "apply_patch" | "str_replace_editor" | "Write" | "Edit" => {
            ToolFamily::FileWrite
        }
        "grep" | "glob" | "search" | "Grep" | "Glob" | "ToolSearch" => ToolFamily::Search,
        "web_fetch" | "web_search" | "WebFetch" | "WebSearch" => ToolFamily::Web,
        "office_render" | "office_convert" | "office_recalculate" => ToolFamily::Office,
        "subagent" | "subagent_fork" | "Agent" => ToolFamily::Subagent,
        _ => ToolFamily::Unknown,
    }
}

/// 把一条折叠后的工具调用投影成显示模型。
pub fn tool_view(call: &ToolCall) -> ToolView {
    let object = call.arguments.as_object();
    let tool_family = family(&call.name);
    let keys: &[&str] = match tool_family {
        ToolFamily::Shell => &["command", "cwd", "timeout"],
        ToolFamily::FileRead => &["file_path", "path", "offset", "limit"],
        ToolFamily::FileWrite => &["file_path", "path", "offset", "limit", "replace_all"],
        ToolFamily::Search => &["pattern", "path", "glob", "output_mode", "type"],
        ToolFamily::Web => &["url", "prompt"],
        ToolFamily::Office => &["path", "engine", "pages"],
        ToolFamily::Subagent => &["description", "subagent_type", "run_in_background"],
        ToolFamily::Unknown => &[
            "description",
            "command",
            "file_path",
            "path",
            "pattern",
            "url",
        ],
    };
    let mut details = Vec::new();
    if let Some(object) = object {
        for key in keys {
            if let Some(value) = object.get(*key)
                && let Some(text) = scalar_text(value, 96)
            {
                details.push(ToolDetail {
                    label: (*key).to_owned(),
                    value: redact_text(&text),
                });
                if details.len() >= 4 {
                    break;
                }
            }
        }
    }
    let title = first_detail(
        &details,
        &[
            "command",
            "file_path",
            "path",
            "pattern",
            "url",
            "description",
        ],
    )
    .map(|detail| detail.value.clone())
    .unwrap_or_else(|| call.summary.clone());

    let result = if call.status == ToolStatus::Error {
        ToolResultView::Error {
            text: bounded(&call.detail, 4096),
        }
    } else if let Some(diff) = diff_from_arguments(object) {
        diff
    } else if call.detail.is_empty() {
        ToolResultView::None
    } else {
        ToolResultView::Text {
            text: call.detail.clone(),
            truncated: call.detail_chars > call.detail.chars().count(),
        }
    };

    let elapsed_ms = match (call.started_at, call.finished_at) {
        (Some(start), Some(finish)) if finish >= start => Some(finish - start),
        _ => None,
    };

    ToolView {
        id: call.id.clone(),
        name: call.name.clone(),
        family: tool_family,
        title,
        summary: redact_text(&call.summary),
        details,
        status: call.status,
        output_chars: call.detail_chars,
        elapsed_ms,
        result,
    }
}

fn first_detail<'a>(details: &'a [ToolDetail], labels: &[&str]) -> Option<&'a ToolDetail> {
    labels
        .iter()
        .find_map(|label| details.iter().find(|detail| detail.label == *label))
}

fn scalar_text(value: &Value, max_chars: usize) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(items) if !items.is_empty() => items
            .iter()
            .filter_map(|item| scalar_text(item, max_chars))
            .take(3)
            .collect::<Vec<_>>()
            .join(", "),
        _ => return None,
    };
    Some(bounded(&text, max_chars))
}

fn diff_from_arguments(object: Option<&serde_json::Map<String, Value>>) -> Option<ToolResultView> {
    let object = object?;
    let before = object
        .get("old_string")
        .and_then(Value::as_str)
        .or_else(|| object.get("oldText").and_then(Value::as_str))?;
    let after = object
        .get("new_string")
        .and_then(Value::as_str)
        .or_else(|| object.get("newText").and_then(Value::as_str))?;
    let path = object
        .get("file_path")
        .and_then(Value::as_str)
        .or_else(|| object.get("path").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    let added = after.lines().count();
    let removed = before.lines().count();
    let mut preview = String::new();
    for line in before.lines().take(4) {
        preview.push('-');
        preview.push_str(line);
        preview.push('\n');
    }
    for line in after.lines().take(4) {
        preview.push('+');
        preview.push_str(line);
        preview.push('\n');
    }
    Some(ToolResultView::Diff {
        path,
        added,
        removed,
        preview,
    })
}

fn bounded(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}…")
}

/// 对常见的 `key=value` 秘密做最小脱敏；渲染层永远不接触原始参数。
fn redact_text(text: &str) -> String {
    const SENSITIVE: [&str; 8] = [
        "token",
        "secret",
        "passwd",
        "authorization",
        "cookie",
        "credential",
        "private_key",
        "api_key",
    ];
    text.split_inclusive(char::is_whitespace)
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            if SENSITIVE
                .iter()
                .any(|key| lower.contains(key) && (lower.contains('=') || lower.contains(':')))
            {
                let prefix = part
                    .split_once(['=', ':'])
                    .map(|(key, _)| key)
                    .unwrap_or(part);
                format!("{prefix}=***")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "c1".to_owned(),
            name: name.to_owned(),
            summary: "summary".to_owned(),
            arguments: parse_arguments(Value::String(arguments.to_owned())),
            authoritative: true,
            meta: Value::Null,
            started_at: Some(1000),
            finished_at: Some(1250),
            turn: 1,
            step: 1,
            status: ToolStatus::Ok,
            detail: String::new(),
            detail_chars: 0,
        }
    }

    #[test]
    fn tool_view_uses_allowlisted_details() {
        let view = tool_view(&call(
            "read",
            r#"{"file_path":"/tmp/a.txt","token":"password","offset":2}"#,
        ));
        assert_eq!(view.family, ToolFamily::FileRead);
        assert_eq!(view.title, "/tmp/a.txt");
        assert_eq!(view.details.len(), 2);
        assert!(view.details.iter().all(|detail| detail.label != "token"));
        assert_eq!(view.elapsed_ms, Some(250));
    }

    #[test]
    fn tool_view_redacts_summary_values() {
        let mut current = call("Bash", r#"{"command":"echo ok"}"#);
        current.summary = "curl token=abc123".to_owned();
        let view = tool_view(&current);
        assert!(!view.summary.contains("abc123"));
        assert!(view.summary.contains("***"));
    }

    #[test]
    fn tool_view_projects_edit_diff() {
        let view = tool_view(&call(
            "edit",
            r#"{"file_path":"src/a.rs","old_string":"a\nb","new_string":"a\nc"}"#,
        ));
        match view.result {
            ToolResultView::Diff {
                path,
                added,
                removed,
                preview,
            } => {
                assert_eq!(path, "src/a.rs");
                assert_eq!((added, removed), (2, 2));
                assert!(preview.contains("-b"));
                assert!(preview.contains("+c"));
            }
            other => panic!("expected diff, got {other:?}"),
        }
    }

    #[test]
    fn tool_upsert_does_not_resurrect_running_state() {
        let mut timeline = ChatTimeline::new();
        let mut first = call("read", r#"{"file_path":"/tmp/a"}"#);
        first.status = ToolStatus::Ok;
        first.detail = "done".to_owned();
        timeline.apply_tool(first);
        let mut replay = call("read", r#"{"file_path":"/tmp/a"}"#);
        replay.status = ToolStatus::Running;
        timeline.apply_tool(replay);
        let rows = timeline.rows();
        match &rows[0] {
            TimelineRow::Tool(view) => {
                assert_eq!(view.status, ToolStatus::Ok);
                match &view.result {
                    ToolResultView::Text { text, .. } => assert_eq!(text, "done"),
                    other => panic!("expected text, got {other:?}"),
                }
            }
            other => panic!("expected tool, got {other:?}"),
        }
    }

    #[test]
    fn timeline_keeps_user_assistant_tool_order() {
        let mut timeline = ChatTimeline::new();
        timeline.entries.push(Entry::User {
            id: MessageId::Seq(1),
            text: "hi".to_owned(),
        });
        timeline.apply_tool(call("read", r#"{"file_path":"/tmp/a"}"#));
        timeline.entries.push(Entry::Assistant {
            id: MessageId::Turn { turn: 1, step: 1 },
            index: 0,
            text: "done".to_owned(),
        });
        let rows = timeline.rows();
        assert!(matches!(rows[0], TimelineRow::User { .. }));
        assert!(matches!(rows[1], TimelineRow::Tool(_)));
        assert!(matches!(rows[2], TimelineRow::Assistant { .. }));
    }

    #[test]
    fn render_ids_include_assistant_block_index() {
        let id = MessageId::Turn { turn: 3, step: 2 };
        let first = RenderId::Assistant { id, index: 4 };
        let second = RenderId::Assistant { id, index: 5 };
        assert_ne!(first.label(), second.label());
        assert!(first.label().contains(":b4"));
    }

    #[test]
    fn empty_reasoning_is_not_a_row() {
        let view = ToolResultView::None;
        assert!(view.is_empty());
    }

    #[test]
    fn stream_blocks_commit_in_order_with_stable_ids() {
        let frames = [
            json!({"type":"start","attemptId":"a","revision":1,"turn":3,"step":2,"startedAfterSeq":0}),
            json!({"type":"chunk","attemptId":"a","revision":2,"index":0,"chunk":{"type":"block-start","index":0,"blockType":"text"}}),
            json!({"type":"chunk","attemptId":"a","revision":3,"index":1,"chunk":{"type":"text-delta","index":0,"text":"先"}}),
            json!({"type":"chunk","attemptId":"a","revision":4,"index":2,"chunk":{"type":"block-end","index":0,"block":{"type":"text","text":"先"}}}),
            json!({"type":"chunk","attemptId":"a","revision":5,"index":3,"chunk":{"type":"block-start","index":1,"blockType":"tool-call"}}),
            json!({"type":"chunk","attemptId":"a","revision":6,"index":4,"chunk":{"type":"tool-call-delta","index":1,"id":"c9","name":"read","argumentsDelta":"{\"file_path\":\"/tmp/a\"}"}}),
            json!({"type":"chunk","attemptId":"a","revision":7,"index":5,"chunk":{"type":"block-end","index":1,"block":{"type":"tool-call","id":"c9","name":"read","arguments":"{\"file_path\":\"/tmp/a\"}"}}}),
            json!({"type":"chunk","attemptId":"a","revision":8,"index":6,"chunk":{"type":"block-start","index":2,"blockType":"text"}}),
            json!({"type":"chunk","attemptId":"a","revision":9,"index":7,"chunk":{"type":"text-delta","index":2,"text":"后"}}),
            json!({"type":"chunk","attemptId":"a","revision":10,"index":8,"chunk":{"type":"block-end","index":2,"block":{"type":"text","text":"后"}}}),
            json!({"type":"end","attemptId":"a","revision":11,"index":9,"outcome":{"kind":"committed","eventType":"assistant/message","seq":18}}),
        ];
        let mut timeline = ChatTimeline::new();
        for frame in frames {
            let frame = serde_json::from_value(frame).expect("stream frame");
            timeline.apply_stream(frame);
        }
        let rows = timeline.rows();
        assert_eq!(rows.len(), 3);
        assert!(matches!(
            &rows[0],
            TimelineRow::Assistant {
                text,
                streaming: false,
                id: RenderId::Assistant {
                    id: MessageId::Turn { turn: 3, step: 2 },
                    index: 0,
                },
                ..
            } if text == "先"
        ));
        assert!(matches!(&rows[1], TimelineRow::Tool(view) if view.id == "c9"));
        assert!(matches!(
            &rows[2],
            TimelineRow::Assistant {
                text,
                id: RenderId::Assistant { index: 2, .. },
                ..
            } if text == "后"
        ));
    }

    #[test]
    fn unclosed_tool_block_has_no_row() {
        let block = crate::fold::Block {
            index: 4,
            kind: "tool-call".to_owned(),
            text: "{\"file_path\":".to_owned(),
            closed: false,
            label: Some("read".to_owned()),
            raw: None,
        };
        assert!(tool_call_from_block(&block, Some((1, 1))).is_none());
    }

    #[test]
    fn structured_arguments_are_used_directly() {
        let mut current = call("bash", "{}");
        current.arguments = json!({"command": "cargo test"});
        let view = tool_view(&current);
        assert_eq!(view.title, "cargo test");
    }
}
