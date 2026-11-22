//! 时间线的数据模型：把会话事件折叠成一串「行」。
//!
//! 第一性原则：一个工具调用**不是一条事件，而是两条**（`tool/call` + `tool/result`，
//! 按 `callId` 配对）。所以历史回放（`session/follow` 的基线快照）和实时流必须
//! 用同一个折叠器，否则「历史里的工具行」和「刚跑完的工具行」一定会长成两个样子。
//!
//! 这里只做**投影**，不做排版：渲染模式（简洁/标准/详细）是视图层对同一份
//! `Entry` 的不同呈现，不在这里分叉。

use serde_json::Value;
use std::collections::HashMap;

/// 工具调用的生命周期。`tool/call` 到就是 `Running`；结果决定收尾状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// 已发起，还没有结果。
    Running,
    /// 正常结束。
    Ok,
    /// 报错结束。
    Error,
    /// 被用户 / 主机中断（官方 `stopped`）。与 `Error` 区分，避免「按了停止却显示失败」。
    Stopped,
}

/// 一次工具调用（已配对）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// `callId`；实时更新时用它做 upsert 的键。
    pub id: String,
    /// wire 上的工具名；来自 assistant 消息的 `tool-call` 块，日志事件只兜底。
    pub name: String,
    /// 单行摘要：参数里最能说明「它在干什么」的那个字符串。
    pub summary: String,
    /// 已解析的工具参数；原始 JSON 字符串在这里解析成对象。
    pub arguments: Value,
    /// 名字/参数是否来自权威内容块（assistant message 或 block-end）。
    /// `tool/call` 日志事件只补时间，不能覆盖内容块。
    pub authoritative: bool,
    /// 工具回执的 `meta`（工作目录、耗时细分等）。
    pub meta: Value,
    /// `tool/call.time`；日志专用事件只提供时间。
    pub started_at: Option<i64>,
    /// `tool/result.time`。
    pub finished_at: Option<i64>,
    /// 所属 `(turn, step)`，用于稳定身份与排查。
    pub turn: u64,
    /// 所属 `(turn, step)`，用于稳定身份与排查。
    pub step: u64,
    /// 状态。
    pub status: ToolStatus,
    /// 结果正文（已截断，见 [`DETAIL_LIMIT`]）；没有结果时为空。
    pub detail: String,
    /// 结果被截断前有多少字符，用于提示「只显示了一部分」。
    pub detail_chars: usize,
}

/// 结果正文的上限。north star 是内存：一条 bash 输出可能是几 MB，
/// 客户端没有理由把它们全留在时间线里。
pub const DETAIL_LIMIT: usize = 32 * 1024;

/// 时间线里的一行的身份，也是渲染层 keyed state 的键。
///
/// `(turn, step)` 优先：实时流（`assistant-stream` 的 start 帧）与持久事件
/// （`assistant/message`）都拿得到，所以流式正在写的那一条与提交后的同一条
/// 是**同一个键**，提交那一帧不重建排版状态。没有 turn/step 的事件
/// （`user/message`）退回事件 `seq`，它在一份会话里唯一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageId {
    /// 持久消息的 `(turn, step)`。
    Turn { turn: u64, step: u64 },
    /// 没有 `(turn, step)` 时用事件序号。
    Seq(u64),
}

impl MessageId {
    /// 从事件里取身份：先看 `data.turn`/`data.step`，否则用顶层 `seq`。
    fn of(data: &Value, event: &Value) -> Self {
        let number = |value: Option<&Value>| value.and_then(Value::as_u64);
        if let (Some(turn), Some(step)) = (number(data.get("turn")), number(data.get("step"))) {
            return Self::Turn { turn, step };
        }
        Self::Seq(number(event.get("seq")).unwrap_or_default())
    }

    /// `ElementId` 用的稳定字符串。
    pub fn label(self) -> String {
        match self {
            Self::Turn { turn, step } => format!("msg:{turn}:{step}"),
            Self::Seq(seq) if seq & (1 << 63) != 0 => format!("msg:local:{}", seq & !(1 << 63)),
            Self::Seq(seq) => format!("msg:seq:{seq}"),
        }
    }

    /// 本地已显示、host 还没给 `seq` 的消息（刚发出的提问）。
    ///
    /// 高位打标志：真实 `seq` 从 0 单调递增，永远不会撞上来；
    /// 回放时整份时间线会被 host 快照重建，这些临时 id 自然消失。
    pub fn local(n: u64) -> Self {
        Self::Seq((1 << 63) | n)
    }
}

/// 时间线里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
// Tool 变体承载完整工具回执（多个 String/Value + 大字段），确实比正文变体大；
// 但把它 Box 化会波及 15+ 构造/匹配点，且单条回执已受 DETAIL_LIMIT 约束。
#[allow(clippy::large_enum_variant)]
pub enum Entry {
    /// 用户提问。
    User {
        /// 身份。
        id: MessageId,
        /// 正文（不解析 markdown：用户写什么就显示什么）。
        text: String,
    },
    /// 助手回答（已提交）。
    Assistant {
        /// 身份。`(turn, step)` 相同但内容块不同时靠 `index` 区分。
        id: MessageId,
        /// 内容块下标；渲染身份的一部分。
        index: u64,
        /// 正文（markdown 源文）。
        text: String,
    },
    /// assistant 的 reasoning 内容块；默认弱化展示。
    Reasoning {
        /// 身份。
        id: MessageId,
        /// 内容块下标。
        index: u64,
        /// 源文。
        text: String,
    },
    /// 一次工具调用。
    Tool(ToolCall),
    /// 状态提示（链路失败等）。
    Notice(String),
}

impl Entry {
    /// 这条行是否是可绘制的正文（用于分页/日志过滤）。
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            Self::User { .. } | Self::Assistant { .. } | Self::Reasoning { .. }
        )
    }
}

/// 事件 → 行 的折叠器。
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    /// `callId` → `entries` 下标，用于把结果写回调用行。
    tool_index: HashMap<String, usize>,
    /// `callId` → `tool/call.time`；日志专用事件只保留这一点信息。
    call_times: HashMap<String, i64>,
}

impl Transcript {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `session/follow` 快照的记录里重建（历史回放）。
    pub fn from_records(records: &[Value]) -> Self {
        let mut transcript = Self::new();
        for record in records {
            if let Some(event) = record.get("event") {
                transcript.push_event(event);
            }
        }
        transcript
    }

    /// 吃一条会话事件。不认识的类型直接忽略（协议是开放轴，见 DECISIONS #8）。
    ///
    /// 返回受影响的行下标：实时流据此增量更新界面，不必整份重发。一条
    /// `assistant/message` 可能包含多个 `tool-call` 块，所以返回的是列表。
    pub fn push_event(&mut self, event: &Value) -> Vec<usize> {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(data) = event.get("data") else {
            return Vec::new();
        };
        match kind {
            "user/message" => {
                // 只有真人说的才进时间线；工具回执、系统注入都不算。
                let from_user = data
                    .pointer("/source/kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "user");
                if !from_user {
                    return Vec::new();
                }
                let text = text_of(data.get("content"));
                if text.trim().is_empty() {
                    return Vec::new();
                }
                self.entries.push(Entry::User {
                    id: MessageId::of(data, event),
                    text,
                });
                vec![self.entries.len() - 1]
            }
            "assistant/message" => self.push_assistant_message(data, event),
            "tool/call" => {
                let Some(id) = data.get("callId").and_then(Value::as_str) else {
                    return Vec::new();
                };
                let id = id.to_owned();
                let time = event_time(event, data);
                if let Some(time) = time {
                    self.call_times.insert(id.clone(), time);
                }
                // `tool/call` 是日志专用事件：只补开始时间，不凭它建内容行。
                // 行由 assistant 消息的 `tool-call` 块建立；只有结果时降级为独立行。
                if let Some(&index) = self.tool_index.get(&id)
                    && let Some(Entry::Tool(call)) = self.entries.get_mut(index)
                    && let Some(time) = time
                {
                    call.started_at = Some(time);
                    return vec![index];
                }
                Vec::new()
            }
            "tool/result" => {
                // 结果的 `callId` **不在** `data` 顶层：它在工具回执消息里
                // （实测 v4 会话是 `message.toolCallId`，老数据还有 `message.source.callId`）。
                let Some(id) = call_id_of(data) else {
                    return Vec::new();
                };
                let failed = data
                    .pointer("/message/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || data.get("error").is_some_and(|error| !error.is_null());
                // 官方把 interrupted 单列为 `stopped`，不混进 error。错误字段可能是
                // 字符串 `"interrupted"`，也可能是 `{ code: "interrupted", ... }`。
                let interrupted = data.get("error").is_some_and(|error| {
                    error.as_str() == Some("interrupted")
                        || error.get("code").and_then(Value::as_str) == Some("interrupted")
                        || error.get("message").and_then(Value::as_str) == Some("interrupted")
                });
                let raw = text_of(data.pointer("/message/content"));
                let detail_chars = raw.chars().count();
                let detail = truncate(raw);
                let status = if interrupted {
                    ToolStatus::Stopped
                } else if failed {
                    ToolStatus::Error
                } else {
                    ToolStatus::Ok
                };
                let meta = data.get("meta").cloned().unwrap_or(Value::Null);
                let finished_at = event_time(event, data);
                if let Some(&index) = self.tool_index.get(&id) {
                    if let Some(Entry::Tool(call)) = self.entries.get_mut(index) {
                        call.status = status;
                        call.detail = detail;
                        call.detail_chars = detail_chars;
                        call.meta = meta;
                        call.finished_at = finished_at;
                    }
                    return vec![index];
                }
                // 没配到调用也绝不丢结果：降级成只有结果的独立工具行。
                let call = ToolCall {
                    id: id.clone(),
                    name: "tool".to_owned(),
                    summary: first_line(&detail),
                    arguments: Value::Null,
                    authoritative: false,
                    meta,
                    started_at: self.call_times.get(&id).copied(),
                    finished_at,
                    turn: data.get("turn").and_then(Value::as_u64).unwrap_or(0),
                    step: data.get("step").and_then(Value::as_u64).unwrap_or(0),
                    status,
                    detail,
                    detail_chars,
                };
                vec![self.push_tool(call)]
            }
            _ => Vec::new(),
        }
    }

    /// 从 assistant 消息的内容块按原顺序投影正文 / reasoning / 工具调用。
    fn push_assistant_message(&mut self, data: &Value, event: &Value) -> Vec<usize> {
        let content = data.pointer("/message/content");
        let id = MessageId::of(data, event);
        let (turn, step) = match id {
            MessageId::Turn { turn, step } => (turn, step),
            MessageId::Seq(_) => (0, 0),
        };
        let mut touched = Vec::new();
        for (index, block) in content_blocks(content) {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match kind {
                "reasoning" if !text.trim().is_empty() => {
                    self.entries.push(Entry::Reasoning { id, index, text });
                    touched.push(self.entries.len() - 1);
                }
                "text" if !text.trim().is_empty() => {
                    self.entries.push(Entry::Assistant { id, index, text });
                    touched.push(self.entries.len() - 1);
                }
                "tool-call" => {
                    let Some(call_id) = block
                        .get("id")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("callId").and_then(Value::as_str))
                    else {
                        continue;
                    };
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned();
                    let arguments = crate::timeline::parse_arguments(
                        block.get("arguments").cloned().unwrap_or(Value::Null),
                    );
                    let summary = summarize(&name, Some(&arguments));
                    let call = ToolCall {
                        id: call_id.to_owned(),
                        name,
                        summary,
                        arguments,
                        authoritative: true,
                        meta: Value::Null,
                        started_at: self.call_times.get(call_id).copied(),
                        finished_at: None,
                        turn,
                        step,
                        status: ToolStatus::Running,
                        detail: String::new(),
                        detail_chars: 0,
                    };
                    touched.push(self.push_tool(call));
                }
                _ => {}
            }
        }
        touched
    }

    /// 插入或合并一条工具行，返回其下标。
    fn push_tool(&mut self, call: ToolCall) -> usize {
        if let Some(&index) = self.tool_index.get(&call.id)
            && let Some(Entry::Tool(existing)) = self.entries.get_mut(index)
        {
            merge_tool(existing, call);
            return index;
        }
        let index = self.entries.len();
        self.tool_index.insert(call.id.clone(), index);
        self.entries.push(Entry::Tool(call));
        index
    }

    /// 按 `callId` 找一行（实时更新后用来回填）。
    pub fn tool(&self, id: &str) -> Option<&ToolCall> {
        let index = *self.tool_index.get(id)?;
        match self.entries.get(index) {
            Some(Entry::Tool(call)) => Some(call),
            _ => None,
        }
    }

    /// 取下标的行（增量推送时用它取出刚变化的那一条）。
    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// 折好的行。
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 取走行（回放交给界面）。
    pub fn into_entries(self) -> Vec<Entry> {
        self.entries
    }
}

/// 事件时间；顶层 `time` 优先，老数据也可能放在 `data.time`。
fn event_time(event: &Value, data: &Value) -> Option<i64> {
    event
        .get("time")
        .or_else(|| data.get("time"))
        .and_then(Value::as_i64)
}

/// 内容块的统一视图；字符串/单块/数组三种历史形状都接受。
fn content_blocks(content: Option<&Value>) -> Vec<(u64, Value)> {
    let Some(content) = content else {
        return Vec::new();
    };
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, block)| (index as u64, block))
            .collect(),
        Value::String(text) => vec![(0, serde_json::json!({ "type": "text", "text": text }))],
        block => vec![(0, block.clone())],
    }
}

/// 合并同名工具调用：内容块负责名字/参数，日志事件负责时间，结果负责状态/正文。
pub(crate) fn merge_tool(existing: &mut ToolCall, incoming: ToolCall) {
    let keep_identity =
        incoming.authoritative || (!existing.authoritative && existing.name == "tool");
    let status = if incoming.status != ToolStatus::Running {
        incoming.status
    } else {
        existing.status
    };
    let detail = if incoming.detail.is_empty() {
        existing.detail.clone()
    } else {
        incoming.detail
    };
    let detail_chars = if detail == existing.detail {
        existing.detail_chars
    } else {
        incoming.detail_chars
    };
    let meta = if incoming.meta.is_null() {
        existing.meta.clone()
    } else {
        incoming.meta
    };
    let finished_at = incoming.finished_at.or(existing.finished_at);
    let started_at = incoming.started_at.or(existing.started_at);
    let name = if keep_identity {
        incoming.name
    } else {
        existing.name.clone()
    };
    let summary = if keep_identity {
        incoming.summary
    } else {
        existing.summary.clone()
    };
    let arguments = if incoming.arguments.is_null() {
        existing.arguments.clone()
    } else {
        incoming.arguments
    };
    let authoritative = existing.authoritative || incoming.authoritative;
    let turn = if incoming.turn == 0 {
        existing.turn
    } else {
        incoming.turn
    };
    let step = if incoming.step == 0 {
        existing.step
    } else {
        incoming.step
    };
    *existing = ToolCall {
        id: existing.id.clone(),
        name,
        summary,
        arguments,
        authoritative,
        meta,
        started_at,
        finished_at,
        turn,
        step,
        status,
        detail,
        detail_chars,
    };
}

/// 结果的 `callId` 可能在三个地方（实测 v4 会话：`message.toolCallId`）。
fn call_id_of(data: &Value) -> Option<String> {
    for pointer in ["/callId", "/message/toolCallId", "/message/source/callId"] {
        if let Some(id) = data.pointer(pointer).and_then(Value::as_str) {
            return Some(id.to_owned());
        }
    }
    None
}

/// 结果正文按字符截断，避免把几 MB 的输出留在内存里。
fn truncate(text: String) -> String {
    if text.chars().count() <= DETAIL_LIMIT {
        return text;
    }
    let cut: String = text.chars().take(DETAIL_LIMIT).collect();
    format!("{cut}\n…（输出过长，只保留前 {DETAIL_LIMIT} 个字符）")
}

/// 从参数里挑一句能代表「在干什么」的话。
///
/// 工具成千上万，不可能逐个写摘要；所以按优先级挑参数名，挑不到就退化成
/// `k=v` 列表。宁可朴素也不要猜错。
pub(crate) fn summarize(name: &str, arguments: Option<&Value>) -> String {
    let Some(raw) = arguments else {
        return String::new();
    };
    let parsed = match raw {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => value,
            Err(_) => return first_line(text),
        },
        Value::Object(_) => raw.clone(),
        _ => return String::new(),
    };
    // 命令行工具：命令本身比模型写的 description 更能说明发生了什么。
    let shell = matches!(name, "bash" | "pwsh" | "bash_persistent" | "terminal_send");
    if shell
        && let Some(command) = parsed.get("command").and_then(Value::as_str)
        && !command.trim().is_empty()
    {
        return first_line(command);
    }
    const KEYS: [&str; 9] = [
        "description",
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
        "title",
    ];
    if let Some(object) = parsed.as_object() {
        for key in KEYS {
            if let Some(value) = object.get(key).and_then(Value::as_str)
                && !value.trim().is_empty()
            {
                return first_line(value);
            }
        }
        // 兜底：`k=v` 三个，足够让人认出是哪一类调用。
        let pairs: Vec<String> = object
            .iter()
            .filter_map(|(key, value)| match value {
                Value::String(text) => Some(format!("{key}={}", first_line(text))),
                Value::Number(number) => Some(format!("{key}={number}")),
                Value::Bool(flag) => Some(format!("{key}={flag}")),
                _ => None,
            })
            .take(3)
            .collect();
        if !pairs.is_empty() {
            return pairs.join(" ");
        }
    }
    let _ = name;
    first_line(&parsed.to_string())
}

/// 取第一行并收紧长度：工具行是单行展示，换行会撑破布局。
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    const MAX: usize = 96;
    if line.chars().count() <= MAX {
        return line.to_owned();
    }
    let cut: String = line.chars().take(MAX).collect();
    format!("{cut}…")
}

/// 把内容块拼成正文（`content` 可能是字符串、块数组或单块）。
fn text_of(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let blocks = match value {
        Value::Array(blocks) => blocks.as_slice(),
        other => std::slice::from_ref(other),
    };
    let mut out = String::new();
    for block in blocks {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "reasoning" {
            continue;
        }
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            out.push_str(text);
        } else if let Some(text) = block.as_str() {
            out.push_str(text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(call_id: &str, name: &str, arguments: &str) -> Value {
        json!({
            "type": "tool/call",
            "time": 1794675833000i64,
            "data": { "turn": 1, "step": 1, "callId": call_id, "name": name, "arguments": arguments }
        })
    }

    /// 真实的 `tool/result` 形状：`callId` 在 `message.toolCallId`，失败靠 `message.isError`。
    fn result(call_id: &str, text: &str, failed: bool) -> Value {
        json!({
            "type": "tool/result",
            "data": {
                "turn": 1,
                "step": 1,
                "message": {
                    "role": "tool",
                    "toolCallId": call_id,
                    "source": { "kind": "tool", "callId": call_id },
                    "content": [{ "type": "text", "text": text }],
                    "isError": failed,
                },
                "meta": { "cwd": "/tmp" },
            }
        })
    }

    fn user(text: &str) -> Value {
        json!({
            "type": "user/message",
            "data": { "role": "user", "source": { "kind": "user" }, "content": [{ "type": "text", "text": text }] }
        })
    }

    fn assistant_with_tool(
        turn: u64,
        step: u64,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Value {
        json!({
            "type": "assistant/message",
            "seq": 200 + turn * 10 + step,
            "time": 1794675833100i64,
            "data": {
                "turn": turn,
                "step": step,
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool-call",
                        "id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }]
                }
            }
        })
    }

    fn assistant(turn: u64, step: u64, text: &str) -> Value {
        json!({
            "type": "assistant/message",
            "seq": 100 + turn * 10 + step,
            "data": {
                "turn": turn,
                "step": step,
                "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
            }
        })
    }

    #[test]
    fn assistant_identity_is_turn_and_step() {
        // 渲染层拿这个键去复用排版状态：流式与提交后必须一致，
        // 所以身份不能是数组下标（重放会漂移），也不能是 seq（流式期拿不到）。
        let mut transcript = Transcript::new();
        transcript.push_event(&assistant(3, 2, "第一段"));
        let Some(Entry::Assistant { id, .. }) = transcript.entries().first() else {
            panic!("应有助手行");
        };
        assert_eq!(*id, MessageId::Turn { turn: 3, step: 2 });
        assert_eq!(id.label(), "msg:3:2");
    }

    #[test]
    fn user_identity_falls_back_to_seq() {
        // `user/message.data` 里没有 turn/step（实测），只能退回事件 seq。
        let mut transcript = Transcript::new();
        transcript.push_event(&json!({
            "type": "user/message",
            "seq": 8,
            "data": { "source": { "kind": "user" }, "content": [{ "type": "text", "text": "hi" }] }
        }));
        let Some(Entry::User { id, .. }) = transcript.entries().first() else {
            panic!("应有用户行");
        };
        assert_eq!(*id, MessageId::Seq(8));
        assert_eq!(id.label(), "msg:seq:8");
    }

    #[test]
    fn local_identity_never_collides_with_real_seq() {
        // 刚发出的提问还没有 seq；本地 id 高位打标志，真实 seq 单调从 0 增长，不会撞。
        let local = MessageId::local(1);
        assert_eq!(local.label(), "msg:local:1");
        assert_ne!(local, MessageId::Seq(1));
        assert_ne!(MessageId::local(0).label(), MessageId::Seq(0).label());
    }

    #[test]
    fn assistant_tool_block_and_result_pair_into_one_row() {
        let mut transcript = Transcript::new();
        transcript.push_event(&user("跑一下测试"));
        transcript.push_event(&assistant_with_tool(
            1,
            1,
            "c1",
            "bash",
            r#"{"command":"cargo test"}"#,
        ));
        assert!(
            matches!(transcript.entries().last(), Some(Entry::Tool(call)) if call.status == ToolStatus::Running)
        );
        transcript.push_event(&call("c1", "bash", r#"{"command":"cargo test"}"#));
        let Some(Entry::Tool(call)) = transcript.entries().last() else {
            panic!("最后一行应是工具行");
        };
        assert_eq!(call.started_at, Some(1794675833000));
        transcript.push_event(&result("c1", "ok\n", false));
        let Entry::Tool(call) = transcript.entries().last().expect("有行") else {
            panic!("最后一行应是工具行");
        };
        assert_eq!(call.status, ToolStatus::Ok);
        assert_eq!(call.summary, "cargo test");
        assert_eq!(call.detail, "ok\n");
        // user + 一条工具行；日志时间并入工具行，不新增行。
        assert_eq!(transcript.entries().len(), 2);
    }

    #[test]
    fn error_result_marks_failure() {
        let mut transcript = Transcript::new();
        transcript.push_event(&call("c2", "read", r#"{"file_path":"/tmp/x.txt"}"#));
        transcript.push_event(&result("c2", "", true));
        let Some(Entry::Tool(call)) = transcript.entries().first() else {
            panic!("应有工具行");
        };
        assert_eq!(call.status, ToolStatus::Error);
        assert_eq!(call.detail, "");
    }

    #[test]
    fn legacy_result_shape_still_pairs() {
        // 老数据把 callId 放 data 顶层、用 error 字段表示失败。
        let mut transcript = Transcript::new();
        transcript.push_event(&call("c6", "bash", r#"{"command":"ls"}"#));
        transcript.push_event(&json!({
            "type": "tool/result",
            "data": {
                "callId": "c6",
                "message": { "content": [{ "type": "text", "text": "boom" }] },
                "error": { "message": "exit 1" },
            }
        }));
        let Some(Entry::Tool(call)) = transcript.entries().first() else {
            panic!("应有工具行");
        };
        assert_eq!(call.status, ToolStatus::Error);
        assert_eq!(call.detail, "boom");
    }

    #[test]
    fn interrupted_result_is_stopped_not_error() {
        for error in [
            json!("interrupted"),
            json!({ "code": "interrupted", "message": "user stopped" }),
            json!({ "message": "interrupted" }),
        ] {
            let mut transcript = Transcript::new();
            transcript.push_event(&call("c7", "bash", r#"{"command":"sleep 10"}"#));
            transcript.push_event(&json!({
                "type": "tool/result",
                "data": {
                    "callId": "c7",
                    "message": { "content": [{ "type": "text", "text": "已停止" }] },
                    "error": error,
                }
            }));
            let Some(Entry::Tool(call)) = transcript.entries().first() else {
                panic!("应有工具行");
            };
            assert_eq!(call.status, ToolStatus::Stopped, "error={error}");
        }
    }

    #[test]
    fn repeated_call_does_not_duplicate_the_row() {
        let mut transcript = Transcript::new();
        transcript.push_event(&call("c3", "bash", r#"{"command":"ls"}"#));
        transcript.push_event(&call("c3", "bash", r#"{"command":"ls"}"#));
        assert!(transcript.entries().is_empty());
    }

    #[test]
    fn result_without_call_becomes_standalone_row() {
        let mut transcript = Transcript::new();
        transcript.push_event(&result("nope", "x", false));
        // 调用缺失时结果不丢：降级成只有结果的独立行。
        let Some(Entry::Tool(call)) = transcript.entries().first() else {
            panic!("应有独立工具行");
        };
        assert_eq!(call.id, "nope");
        assert_eq!(call.detail, "x");
        assert!(!call.authoritative);
    }

    #[test]
    fn summary_falls_back_to_key_value_pairs() {
        let mut transcript = Transcript::new();
        transcript.push_event(&assistant_with_tool(
            2,
            1,
            "c4",
            "office_render",
            r#"{"engine":"wasm","pages":3}"#,
        ));
        let Some(Entry::Tool(call)) = transcript.entries().first() else {
            panic!("应有工具行");
        };
        assert!(
            call.summary.contains("engine=wasm"),
            "实际: {}",
            call.summary
        );
    }

    #[test]
    fn long_output_is_truncated() {
        let mut transcript = Transcript::new();
        transcript.push_event(&call("c5", "bash", r#"{"command":"seq"}"#));
        let huge = "x".repeat(DETAIL_LIMIT + 10);
        transcript.push_event(&result("c5", &huge, false));
        let Some(Entry::Tool(call)) = transcript.entries().first() else {
            panic!("应有工具行");
        };
        assert_eq!(call.detail_chars, DETAIL_LIMIT + 10);
        assert!(call.detail.chars().count() < DETAIL_LIMIT + 120);
        assert!(call.detail.contains("只保留前"));
    }
}
