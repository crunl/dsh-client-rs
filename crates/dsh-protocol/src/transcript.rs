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
}

/// 一次工具调用（已配对）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// `callId`；实时更新时用它做 upsert 的键。
    pub id: String,
    /// wire 上的工具名，原样显示（`bash` / `read` / `web_fetch`）。
    pub name: String,
    /// 单行摘要：参数里最能说明「它在干什么」的那个字符串。
    pub summary: String,
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
        /// 身份。
        id: MessageId,
        /// 正文（markdown 源文）。
        text: String,
    },
    /// 一次工具调用。
    Tool(ToolCall),
    /// 状态提示（链路失败等）。
    Notice(String),
}

/// 事件 → 行 的折叠器。
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    /// `callId` → `entries` 下标，用于把结果写回调用行。
    tool_index: HashMap<String, usize>,
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
    /// 返回受影响的行下标：实时流据此增量更新界面，不必整份重发。
    pub fn push_event(&mut self, event: &Value) -> Option<usize> {
        let kind = event.get("type")?.as_str()?;
        let data = event.get("data")?;
        match kind {
            "user/message" => {
                // 只有真人说的才进时间线；工具回执、系统注入都不算。
                let from_user = data
                    .pointer("/source/kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "user");
                if !from_user {
                    return None;
                }
                let text = text_of(data.get("content"));
                (!text.trim().is_empty()).then(|| {
                    self.entries.push(Entry::User {
                        id: MessageId::of(data, event),
                        text,
                    });
                    self.entries.len() - 1
                })
            }
            "assistant/message" => {
                let text = text_of(data.pointer("/message/content"));
                (!text.trim().is_empty()).then(|| {
                    self.entries.push(Entry::Assistant {
                        id: MessageId::of(data, event),
                        text,
                    });
                    self.entries.len() - 1
                })
            }
            "tool/call" => {
                let id = data.get("callId")?.as_str()?.to_owned();
                let name = data
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let summary = summarize(name.as_str(), data.get("arguments"));
                let index = self.entries.len();
                // 同一个 callId 重放两次（快照 + 增量）时不要重复插入。
                if let Some(existing) = self.tool_index.get(&id) {
                    return Some(*existing);
                }
                self.entries.push(Entry::Tool(ToolCall {
                    id: id.clone(),
                    name,
                    summary,
                    status: ToolStatus::Running,
                    detail: String::new(),
                    detail_chars: 0,
                }));
                self.tool_index.insert(id, index);
                Some(index)
            }
            "tool/result" => {
                // 结果的 `callId` **不在** `data` 顶层：它在工具回执消息里
                // （实测 v4 会话是 `message.toolCallId`，老数据还有 `message.source.callId`）。
                let id = call_id_of(data)?;
                let index = *self.tool_index.get(&id)?;
                let failed = data
                    .pointer("/message/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || data.get("error").is_some_and(|error| !error.is_null());
                let raw = text_of(data.pointer("/message/content"));
                let detail_chars = raw.chars().count();
                let detail = truncate(raw);
                if let Some(Entry::Tool(call)) = self.entries.get_mut(index) {
                    call.status = if failed {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Ok
                    };
                    call.detail = detail;
                    call.detail_chars = detail_chars;
                }
                Some(index)
            }
            _ => None,
        }
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
fn summarize(name: &str, arguments: Option<&Value>) -> String {
    let Some(raw) = arguments.and_then(Value::as_str) else {
        return String::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return first_line(raw);
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
    fn call_and_result_pair_into_one_row() {
        let mut transcript = Transcript::new();
        transcript.push_event(&user("跑一下测试"));
        transcript.push_event(&call("c1", "bash", r#"{"command":"cargo test"}"#));
        assert!(
            matches!(transcript.entries().last(), Some(Entry::Tool(call)) if call.status == ToolStatus::Running)
        );

        transcript.push_event(&result("c1", "ok\n", false));
        let Entry::Tool(call) = transcript.entries().last().expect("有行") else {
            panic!("最后一行应是工具行");
        };
        assert_eq!(call.status, ToolStatus::Ok);
        assert_eq!(call.summary, "cargo test");
        assert_eq!(call.detail, "ok\n");
        // 两条事件只长出一行。
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
        assert_eq!(call.summary, "/tmp/x.txt");
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
    fn repeated_call_does_not_duplicate_the_row() {
        let mut transcript = Transcript::new();
        transcript.push_event(&call("c3", "bash", r#"{"command":"ls"}"#));
        transcript.push_event(&call("c3", "bash", r#"{"command":"ls"}"#));
        assert_eq!(transcript.entries().len(), 1);
    }

    #[test]
    fn result_without_call_is_ignored() {
        let mut transcript = Transcript::new();
        assert!(transcript.push_event(&result("nope", "x", false)).is_none());
        assert!(transcript.entries().is_empty());
    }

    #[test]
    fn summary_falls_back_to_key_value_pairs() {
        let mut transcript = Transcript::new();
        transcript.push_event(&call(
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
