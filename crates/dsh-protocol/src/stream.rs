//! 流式帧：Gateway 逻辑流信封 + `session/follow` 的业务值。
//!
//! 线上形状见 `docs/MODEL.md` 的「实测帧形态」与
//! `crates/dsh-protocol/fixtures/follow-stream.jsonl`。要点：
//!
//! - WebSocket 每条消息是 Gateway 的 [`StreamItem`]，业务帧在它的 `value` 里；
//! - `value.type` 有三类：`snapshot` / `event` / `assistant-stream`；
//! - `assistant-stream.frame` 走 `start` → `chunk`* → `end`。

use serde::Deserialize;

/// Gateway 逻辑流的一帧。`streamId` 由客户端在 `open` 时生成。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamItem {
    /// `item` / `error` / `end`。
    #[serde(rename = "type")]
    pub kind: StreamItemKind,
    /// 多路复用 id。
    pub stream_id: String,
    /// `item` 携带的业务值。
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// `error` 携带的失败。
    #[serde(default)]
    pub error: Option<StreamFailure>,
}

/// Gateway 逻辑流帧类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamItemKind {
    /// 一个业务值。
    Item,
    /// 流失败。
    Error,
    /// 流正常结束。
    End,
}

/// 流失败。
#[derive(Debug, Clone, Deserialize)]
pub struct StreamFailure {
    /// 稳定错误码。
    pub code: String,
    /// 人类可读描述。
    pub message: String,
    /// 附加信息。
    #[serde(default)]
    pub details: serde_json::Value,
}

/// `session/follow` 的业务值。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum FollowValue {
    /// 打开流时的基线快照。
    Snapshot(FollowSnapshot),
    /// 一条已提交的会话事件。
    Event(FollowEvent),
    /// 流式尝试的一帧。
    AssistantStream(AssistantStreamValue),
    /// 未知类型，按 DECISIONS #8 降级而非报错。
    #[serde(other)]
    Unknown,
}

/// 基线快照。只落当前需要的字段，其余忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowSnapshot {
    /// 快照自身携带一份恢复用的流式基线。
    #[serde(default)]
    pub assistant_stream: Option<AssistantStreamBaseline>,
    /// 游标。
    #[serde(default)]
    pub cursor: u64,
    /// 是否还有更早的页。
    #[serde(default)]
    pub has_more: bool,
    /// 历史记录；形状随记录类型变化，暂不细化。
    #[serde(default)]
    pub records: Vec<serde_json::Value>,
}

/// 一次 follow 开始时的流式基线（`snapshot.assistantStream`）。
///
/// 静止时为 `{"revision": 0}`；重连时可能带着进行中的尝试。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStreamBaseline {
    /// 基线所处的 revision；下一帧应当是 `revision + 1`。
    pub revision: u64,
    /// 进行中的尝试，静止时缺失。
    #[serde(default)]
    pub active_attempt: Option<BaselineAttempt>,
}

/// 基线里进行中的尝试。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineAttempt {
    /// 尝试 id。
    pub attempt_id: String,
    /// 该尝试开始时最后一个持久 seq。
    #[serde(default)]
    pub started_after_seq: u64,
    /// 轮次。
    #[serde(default)]
    pub turn: u64,
    /// 步号。
    #[serde(default)]
    pub step: u64,
    /// 下一个线上 chunk 帧应有的稠密下标。
    pub next_index: u64,
    /// 已累积的脱离态 stream（chunk 对象数组）。
    #[serde(default)]
    pub stream: Vec<serde_json::Value>,
}

/// 一条已提交事件。
#[derive(Debug, Clone, Deserialize)]
pub struct FollowEvent {
    /// 事件本体，`type` 是开放轴，见 DECISIONS #8。
    pub event: serde_json::Value,
}

/// `assistant-stream` 值的包装。
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantStreamValue {
    /// 流式帧。
    pub frame: AssistantStreamFrame,
}

/// 流式帧：`start` → `chunk`* → `end`。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AssistantStreamFrame {
    /// 一次尝试开始。
    Start(StreamStart),
    /// 一个增量。
    Chunk(StreamChunkFrame),
    /// 一次尝试结束。
    End(StreamEnd),
    /// 未知帧类型。
    #[serde(other)]
    Unknown,
}

/// 尝试开始。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStart {
    /// 尝试 id，形如 `<sessionId>:<n>`。
    pub attempt_id: String,
    /// 逐帧递增；不连续即失效信号。
    pub revision: u64,
    /// 轮次序号。
    #[serde(default)]
    pub turn: Option<u64>,
    /// 步序号。
    #[serde(default)]
    pub step: Option<u64>,
    /// 该尝试开始时观察到的最后一个持久 seq。
    #[serde(default)]
    pub started_after_seq: u64,
}

/// 携带一个 [`StreamChunk`] 的帧。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamChunkFrame {
    /// 尝试 id。
    pub attempt_id: String,
    /// 逐帧递增。
    pub revision: u64,
    /// 帧序号，稠密递增。
    pub index: u64,
    /// host 侧时间戳。
    #[serde(default)]
    pub time: Option<i64>,
    /// 增量本体。
    pub chunk: StreamChunk,
}

/// 尝试结束。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEnd {
    /// 尝试 id。
    pub attempt_id: String,
    /// 逐帧递增。
    pub revision: u64,
    /// 帧序号。
    pub index: u64,
    /// 结束结果。
    pub outcome: StreamOutcome,
}

/// 尝试的结束结果。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StreamOutcome {
    /// 已提交为持久消息；用它替换流式累积。
    Committed {
        /// 持久事件类型，实测为 `assistant/message`。
        #[serde(rename = "eventType")]
        event_type: String,
        /// 该持久事件的 seq。
        seq: u64,
    },
    /// 尝试被放弃；保留已显示前缀并标记中断。
    Abandoned,
    /// 未知结果。
    #[serde(other)]
    Unknown,
}

/// 增量文法（`StreamChunk`）。
///
/// 实测出现顺序：`block-start` → `text-delta` → `block-end` → `usage` → `finish`。
/// **空的 reasoning 块是常态**（`block-start` 后立刻 `block-end`、文本为空串），
/// 消费方必须容忍。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StreamChunk {
    /// 一个内容块开始。
    BlockStart {
        /// 块下标。
        index: u64,
        /// 块类型，实测见过 `reasoning` / `text`。
        #[serde(rename = "blockType")]
        block_type: String,
    },
    /// 文本增量。
    TextDelta {
        /// 块下标。
        index: u64,
        /// 增量文本。
        text: String,
    },
    /// 推理增量。
    ReasoningDelta {
        /// 块下标。
        index: u64,
        /// 增量文本。
        text: String,
    },
    /// 工具调用增量。
    ToolCallDelta {
        /// 块下标。
        index: u64,
        /// 工具调用 id（源码里是必填）。
        id: String,
        /// 工具名，仅当调用开始时带。
        #[serde(default)]
        name: Option<String>,
        /// 参数 JSON 片段（源码里是必填）。
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    /// 一个内容块结束，携带已装配的完整块。
    BlockEnd {
        /// 块下标。
        index: u64,
        /// 装配后的块，`type` 是开放轴。
        block: serde_json::Value,
    },
    /// token 用量。
    Usage {
        /// 用量明细。
        usage: TokenUsage,
    },
    /// 生成结束。
    Finish {
        /// 结束原因，例如 `{"kind":"stop"}`。
        reason: serde_json::Value,
        /// 重放状态，可能缺失。
        #[serde(default)]
        replay_state: serde_json::Value,
    },
    /// 未知增量类型。
    #[serde(other)]
    Unknown,
}

/// token 用量。字段名线上是 camelCase。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// **未命中缓存**的输入 token。四个桶互不重叠：提示长度 = 这个 +
    /// `cache_read_tokens` + `cache_write_tokens`（官方 `contextTokens`）。
    #[serde(default)]
    pub input_tokens: u64,
    /// 输出 token。
    #[serde(default)]
    pub output_tokens: u64,
    /// 缓存命中读取量。
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 缓存写入量。
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// 合计。
    #[serde(default)]
    pub total_tokens: u64,
}
