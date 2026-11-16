//! `assistant-stream` 的折叠状态机。
//!
//! 输入是 [`AssistantStreamFrame`] 序列，输出是可直接渲染的块列表与终局。
//! 规则来自 `docs/MODEL.md`「流式状态机规则」，并用
//! `fixtures/follow-stream.jsonl` 的真实帧序列做回归。
//!
//! 三条不变量：
//!
//! 1. `revision` 与 `index` **逐帧连续**。出现空洞说明累积已失效，
//!    必须丢弃并等待下一次 `start`（[`Apply::Resynced`]）。
//! 2. 完全重复的帧视为重放，幂等忽略（[`Apply::Ignored`]），不清空累积。
//! 3. `block-end` 携带的已装配块是**权威版本**，会覆盖增量累积的文本。

use crate::stream::{
    AssistantStreamBaseline, AssistantStreamFrame, StreamChunk, StreamChunkFrame, StreamEnd,
    StreamOutcome, StreamStart, TokenUsage,
};
use serde_json::Value;

/// 一个累积中的内容块。
///
/// `kind` 是开放轴（`text` / `reasoning` / `tool-call` / `image` / …），
/// 未知种类原样保留，见 DECISIONS #8。
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// 块下标。
    pub index: u64,
    /// 块类型。
    pub kind: String,
    /// 累积文本；工具调用块里是参数 JSON 片段。
    pub text: String,
    /// 是否已收到 `block-end`。
    pub closed: bool,
    /// 工具块的显示标签（工具名优先，其次 id）。
    pub label: Option<String>,
    /// `block-end` 携带的原始块，未闭合时为 `None`。
    pub raw: Option<Value>,
}

impl Block {
    /// 是否是空块。**空的 reasoning 块是常态**（实测如此），渲染时必须过滤。
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 是否应当渲染。
    pub fn is_visible(&self) -> bool {
        !self.is_empty()
    }
}

/// 流式尝试的终局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldOutcome {
    /// 已提交为持久消息。调用方应用 `seq` 取持久版本替换流式累积。
    Committed {
        /// 持久事件类型，实测为 `assistant/message`。
        event_type: String,
        /// 持久事件的 seq。
        seq: u64,
    },
    /// 尝试被放弃；保留已显示前缀并标记中断。
    Interrupted,
}

/// 一次 [`AssistantStreamFold::apply`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// 新尝试开始，累积已清空。
    Started,
    /// 已应用。
    Applied,
    /// 重放或无关帧，状态未变。
    Ignored,
    /// 检测到空洞，累积已丢弃，等待新的 `start`。
    Resynced,
}

/// 连续性判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Continuity {
    /// 正是下一帧。
    Proceed,
    /// 已处理过的帧（`revision` 不前进）。
    Duplicate,
    /// 跳号。
    Gap,
    /// 还没有任何尝试。
    NoAttempt,
}

/// `assistant-stream` 折叠器。
#[derive(Debug, Default)]
pub struct AssistantStreamFold {
    attempt_id: Option<String>,
    revision: Option<u64>,
    index: Option<u64>,
    /// 本尝试的 `(turn, step)`：流式期与提交后用同一个渲染身份。
    turn_step: Option<(u64, u64)>,
    blocks: Vec<Block>,
    usage: Option<TokenUsage>,
    finish_reason: Option<Value>,
    replay_state: Option<Value>,
    outcome: Option<FoldOutcome>,
    anomalies: u32,
    unsupported: u32,
}

impl AssistantStreamFold {
    /// 新建一个空折叠器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从快照里的 `assistantStream` 基线恢复。
    ///
    /// 断线重连时不要重放整轮：基线带着已累积的 `stream` 与 `nextIndex`，
    /// 接上之后下一帧的连续性就能直接对上。
    pub fn from_baseline(baseline: &AssistantStreamBaseline) -> Self {
        let mut fold = Self {
            revision: Some(baseline.revision),
            ..Self::default()
        };

        let Some(attempt) = &baseline.active_attempt else {
            return fold;
        };

        fold.attempt_id = Some(attempt.attempt_id.clone());
        // `nextIndex` 是下一个 chunk 帧应有的下标，换算成「已收到最后一个」。
        fold.index = attempt.next_index.checked_sub(1);
        for value in &attempt.stream {
            match serde_json::from_value::<StreamChunk>(value.clone()) {
                Ok(chunk) => fold.absorb(&chunk),
                Err(_) => fold.unsupported += 1,
            }
        }
        fold
    }

    /// 应用一帧。
    pub fn apply(&mut self, frame: &AssistantStreamFrame) -> Apply {
        match frame {
            AssistantStreamFrame::Unknown => {
                self.unsupported += 1;
                Apply::Ignored
            }
            AssistantStreamFrame::Start(start) => self.apply_start(start),
            AssistantStreamFrame::Chunk(chunk) => self.apply_chunk(chunk),
            AssistantStreamFrame::End(end) => self.apply_end(end),
        }
    }

    /// 当前累积的块，按出现顺序。
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// 需要渲染的块：跳过空块（空 reasoning 是常态）。
    pub fn visible_blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks.iter().filter(|block| block.is_visible())
    }

    /// 把可见的 `text` 块拼成一段文本，块间用换行分隔。
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self
            .blocks
            .iter()
            .filter(|block| block.kind == "text" && block.is_visible())
            .map(|block| block.text.as_str())
            .collect();
        parts.join("\n")
    }

    /// 是否仍在流式中（有尝试且尚无终局）。
    pub fn is_streaming(&self) -> bool {
        self.attempt_id.is_some() && self.outcome.is_none()
    }

    /// 当前尝试的 id。
    pub fn attempt_id(&self) -> Option<&str> {
        self.attempt_id.as_deref()
    }

    /// 终局。
    pub fn outcome(&self) -> Option<&FoldOutcome> {
        self.outcome.as_ref()
    }

    /// 已提交时返回持久事件的 seq。
    pub fn committed_seq(&self) -> Option<u64> {
        match self.outcome {
            Some(FoldOutcome::Committed { seq, .. }) => Some(seq),
            _ => None,
        }
    }

    /// 本尝试的 `(turn, step)`。
    ///
    /// 渲染层拿它做 keyed state 的键：流式正在写的那一条与提交后的持久消息
    /// 在两边都能拿到同一个 `(turn, step)`，所以提交那一帧不必重建排版。
    pub fn turn_step(&self) -> Option<(u64, u64)> {
        self.turn_step
    }

    /// 最后一次 `usage`。
    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// `finish` 的原因（形状随 provider 变化）。
    pub fn finish_reason(&self) -> Option<&Value> {
        self.finish_reason.as_ref()
    }

    /// `finish` 携带的重放状态。
    pub fn replay_state(&self) -> Option<&Value> {
        self.replay_state.as_ref()
    }

    /// 结构性违例计数：孤立帧、重复的 `block-start`、未知块的增量等。
    ///
    /// 非零不代表渲染不可用，代表 host 侧行为与文档不一致，值得记日志。
    pub fn anomalies(&self) -> u32 {
        self.anomalies
    }

    /// 未知帧 / 未知增量类型的计数。
    pub fn unsupported(&self) -> u32 {
        self.unsupported
    }

    fn apply_start(&mut self, start: &StreamStart) -> Apply {
        // 同一尝试的低版本 start 是重放：不能清空已累积的内容。
        if self.attempt_id.as_deref() == Some(start.attempt_id.as_str())
            && self
                .revision
                .is_some_and(|revision| start.revision <= revision)
        {
            return Apply::Ignored;
        }

        self.reset_accumulation();
        self.attempt_id = Some(start.attempt_id.clone());
        self.revision = Some(start.revision);
        self.index = None;
        if let (Some(turn), Some(step)) = (start.turn, start.step) {
            self.turn_step = Some((turn, step));
        }
        Apply::Started
    }

    fn apply_chunk(&mut self, frame: &StreamChunkFrame) -> Apply {
        match self.check(frame.revision, frame.index) {
            Continuity::Proceed => {}
            Continuity::Duplicate => return Apply::Ignored,
            Continuity::Gap => {
                self.reset_accumulation();
                return Apply::Resynced;
            }
            Continuity::NoAttempt => {
                // 没有 start 就来的增量：不可信，丢弃并记账。
                self.anomalies += 1;
                return Apply::Ignored;
            }
        }

        self.revision = Some(frame.revision);
        self.index = Some(frame.index);
        self.absorb(&frame.chunk);
        Apply::Applied
    }

    fn apply_end(&mut self, end: &StreamEnd) -> Apply {
        match self.check(end.revision, end.index) {
            Continuity::Duplicate => return Apply::Ignored,
            Continuity::Gap => {
                self.reset_accumulation();
                return Apply::Resynced;
            }
            Continuity::NoAttempt => self.anomalies += 1,
            Continuity::Proceed => {}
        }

        self.revision = Some(end.revision);
        self.index = Some(end.index);

        self.outcome = Some(match &end.outcome {
            StreamOutcome::Committed { event_type, seq } => FoldOutcome::Committed {
                event_type: event_type.clone(),
                seq: *seq,
            },
            StreamOutcome::Abandoned => FoldOutcome::Interrupted,
            StreamOutcome::Unknown => {
                self.unsupported += 1;
                return Apply::Ignored;
            }
        });

        Apply::Applied
    }

    fn absorb(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                if self.blocks.iter().any(|block| block.index == *index) {
                    self.anomalies += 1;
                    return;
                }
                self.blocks.push(Block {
                    index: *index,
                    kind: block_type.clone(),
                    text: String::new(),
                    closed: false,
                    label: None,
                    raw: None,
                });
            }
            StreamChunk::TextDelta { index, text }
            | StreamChunk::ReasoningDelta { index, text } => {
                self.block_mut(*index).text.push_str(text);
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let block = self.block_mut(*index);
                block.kind = "tool-call".to_owned();
                if block.label.is_none() {
                    block.label = Some(name.clone().unwrap_or_else(|| id.clone()));
                }
                block.text.push_str(arguments_delta);
            }
            StreamChunk::BlockEnd { index, block } => {
                let target = self.block_mut(*index);
                target.closed = true;
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    target.kind = kind.to_owned();
                }
                // 装配后的块是权威版本，覆盖增量累积。
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text.clone_into(&mut target.text);
                }
                target.raw = Some(block.clone());
            }
            StreamChunk::Usage { usage } => self.usage = Some(usage.clone()),
            StreamChunk::Finish {
                reason,
                replay_state,
            } => {
                self.finish_reason = Some(reason.clone());
                self.replay_state = Some(replay_state.clone());
            }
            StreamChunk::Unknown => self.unsupported += 1,
        }
    }

    /// 取指定下标的块；缺失时补一个并记账（不丢增量数据）。
    fn block_mut(&mut self, index: u64) -> &mut Block {
        if let Some(position) = self.blocks.iter().position(|block| block.index == index) {
            return &mut self.blocks[position];
        }

        self.anomalies += 1;
        self.blocks.push(Block {
            index,
            kind: "text".to_owned(),
            text: String::new(),
            closed: false,
            label: None,
            raw: None,
        });
        let position = self.blocks.len() - 1;
        &mut self.blocks[position]
    }

    fn check(&self, revision: u64, index: u64) -> Continuity {
        match (self.revision, self.index) {
            (None, _) => Continuity::NoAttempt,
            (Some(previous), last_index) => {
                if revision == previous + 1 {
                    let next_index = last_index.map_or(0, |last| last + 1);
                    if index == next_index {
                        Continuity::Proceed
                    } else {
                        Continuity::Gap
                    }
                } else if revision <= previous {
                    Continuity::Duplicate
                } else {
                    Continuity::Gap
                }
            }
        }
    }

    /// 丢掉累积，但保留 `outcome`——它是持久事实，不属于累积。
    fn reset_accumulation(&mut self) {
        self.attempt_id = None;
        self.revision = None;
        self.index = None;
        // `turn_step` 不在这里清：重同步（空洞撞上新的 start）与真正的新一轮
        // 都会紧跟一次 `apply_start` 重写它；保留到那时能让渲染身份少跳一次。
        self.blocks.clear();
        self.usage = None;
        self.finish_reason = None;
        self.replay_state = None;
    }
}
