//! `assistant-stream` 折叠状态机的表驱动测试。
//!
//! 主体用例直接回放 `fixtures/follow-stream.jsonl` 的真实帧序列；
//! 其余用例构造边界帧（跳号、重放、放弃、未知类型）。

use dsh_protocol::AssistantStreamFold;
use dsh_protocol::fold::{Apply, FoldOutcome};
use dsh_protocol::stream::{
    AssistantStreamBaseline, AssistantStreamFrame, FollowValue, StreamItem,
};
use serde_json::json;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()))
}

/// 从 fixture 里取出全部 assistant-stream 帧，按线上顺序排列。
fn captured_frames() -> Vec<AssistantStreamFrame> {
    fixture("follow-stream.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<StreamItem>(line).ok())
        .filter_map(|item| item.value)
        .filter_map(|value| serde_json::from_value::<FollowValue>(value).ok())
        .filter_map(|value| match value {
            FollowValue::AssistantStream(stream) => Some(stream.frame),
            _ => None,
        })
        .collect()
}

fn frame(value: serde_json::Value) -> AssistantStreamFrame {
    serde_json::from_value(value).expect("构造的帧应当合法")
}

/// 真实快照里的恢复基线。
fn captured_baseline() -> AssistantStreamBaseline {
    let first = fixture("follow-stream.jsonl");
    let line = first.lines().next().expect("fixture 不应为空");
    let item: StreamItem = serde_json::from_str(line).expect("首行应是 item 帧");
    let value = serde_json::from_value::<FollowValue>(item.value.expect("item 应有 value"))
        .expect("首行应是 snapshot");
    match value {
        FollowValue::Snapshot(snapshot) => {
            snapshot.assistant_stream.expect("快照应带 assistantStream")
        }
        other => panic!("首行应是 snapshot，实际 {other:?}"),
    }
}

#[test]
fn replaying_captured_stream_reaches_committed_state() {
    let mut fold = AssistantStreamFold::new();
    let results: Vec<Apply> = captured_frames().iter().map(|f| fold.apply(f)).collect();

    assert_eq!(results.first(), Some(&Apply::Started));
    assert!(results[1..].iter().all(|r| *r == Apply::Applied));
    assert_eq!(fold.anomalies(), 0, "真实流量不应有结构违例");
    assert_eq!(fold.unsupported(), 0);

    // 两个块：空 reasoning + 有内容的 text。
    let blocks = fold.blocks();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].kind, "reasoning");
    assert!(blocks[0].is_empty());
    assert!(!blocks[0].is_visible(), "空 reasoning 块不渲染");
    assert_eq!(blocks[1].kind, "text");
    assert_eq!(blocks[1].text, "收到");
    assert!(blocks[1].closed);

    // 渲染视图只给出一个块。
    let visible: Vec<&str> = fold.visible_blocks().map(|b| b.kind.as_str()).collect();
    assert_eq!(visible, vec!["text"]);
    assert_eq!(fold.text(), "收到");

    // usage 与终局。
    let usage = fold.usage().expect("应有 usage");
    assert_eq!(usage.input_tokens, 14766);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(usage.total_tokens, 14768);
    assert_eq!(usage.cache_read_tokens, 0);

    assert_eq!(fold.finish_reason(), Some(&json!({ "kind": "stop" })));
    assert!(!fold.is_streaming());
    assert_eq!(fold.committed_seq(), Some(18));
    match fold.outcome() {
        Some(FoldOutcome::Committed { event_type, seq }) => {
            assert_eq!(event_type, "assistant/message");
            assert_eq!(*seq, 18);
        }
        other => panic!("期望 committed，实际 {other:?}"),
    }
}

#[test]
fn start_carries_the_render_identity() {
    // 渲染层用 `(turn, step)` 做 keyed state 的键：流式尾巴与提交后的持久消息
    // 在两侧都能拿到同一个值，所以「流式 → 已提交」不重建排版。
    let mut fold = AssistantStreamFold::new();
    assert_eq!(fold.turn_step(), None, "新折叠器没有身份");
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:9", "revision": 1, "turn": 4, "step": 2, "startedAfterSeq": 12
    })));
    assert_eq!(fold.turn_step(), Some((4, 2)));
}

#[test]
fn gap_in_revision_discards_accumulation() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "block-start", "index": 0, "blockType": "text" }
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 3, "index": 1,
        "chunk": { "type": "text-delta", "index": 0, "text": "半句" }
    })));
    assert_eq!(fold.text(), "半句");

    // 跳过 revision 4，直接 5：空洞。
    let result = fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 5, "index": 2,
        "chunk": { "type": "text-delta", "index": 0, "text": "不该被接上" }
    })));

    assert_eq!(result, Apply::Resynced);
    assert!(fold.blocks().is_empty(), "累积必须被丢弃");
    assert_eq!(fold.attempt_id(), None, "要等新的 start");
    assert!(!fold.is_streaming());
}

#[test]
fn duplicate_frame_is_idempotent() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "block-start", "index": 0, "blockType": "text" }
    })));
    let delta = frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 3, "index": 1,
        "chunk": { "type": "text-delta", "index": 0, "text": "收到" }
    }));

    assert_eq!(fold.apply(&delta), Apply::Applied);
    assert_eq!(fold.apply(&delta), Apply::Ignored, "重放不清空累积");
    assert_eq!(fold.text(), "收到", "文本不能被追加两次");
    assert_eq!(fold.anomalies(), 0);
}

#[test]
fn abandoned_attempt_keeps_prefix() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "block-start", "index": 0, "blockType": "text" }
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 3, "index": 1,
        "chunk": { "type": "text-delta", "index": 0, "text": "写到一半" }
    })));
    let result = fold.apply(&frame(json!({
        "type": "end", "attemptId": "s:1", "revision": 4, "index": 2,
        "outcome": { "kind": "abandoned" }
    })));

    assert_eq!(result, Apply::Applied);
    assert_eq!(fold.outcome(), Some(&FoldOutcome::Interrupted));
    assert_eq!(fold.text(), "写到一半", "中断保留已显示前缀");
    assert_eq!(fold.committed_seq(), None);
    assert!(!fold.is_streaming());
}

#[test]
fn block_end_overrides_accumulated_text() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "block-start", "index": 0, "blockType": "text" }
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 3, "index": 1,
        "chunk": { "type": "text-delta", "index": 0, "text": "残" }
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 4, "index": 2,
        "chunk": { "type": "block-end", "index": 0,
                   "block": { "type": "text", "text": "完整回答" } }
    })));

    assert_eq!(fold.text(), "完整回答", "装配后的块是权威版本");
}

#[test]
fn unknown_chunk_type_does_not_break_the_stream() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "block-start", "index": 0, "blockType": "text" }
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 3, "index": 1,
        "chunk": { "type": "brand-new-chunk-kind", "index": 0, "payload": 1 }
    })));
    let result = fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 4, "index": 2,
        "chunk": { "type": "text-delta", "index": 0, "text": "继续" }
    })));

    assert_eq!(result, Apply::Applied, "未知增量不该打断连续性");
    assert_eq!(fold.unsupported(), 1);
    assert_eq!(fold.text(), "继续");
}

#[test]
fn restart_of_same_attempt_with_lower_revision_is_ignored() {
    let mut fold = AssistantStreamFold::new();
    fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));
    fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:1", "revision": 2, "index": 0,
        "chunk": { "type": "text-delta", "index": 0, "text": "已累积" }
    })));

    let result = fold.apply(&frame(json!({
        "type": "start", "attemptId": "s:1", "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
    })));

    assert_eq!(result, Apply::Ignored);
    assert_eq!(fold.text(), "已累积");
}

#[test]
fn a_new_attempt_starts_from_a_clean_slate() {
    let mut fold = AssistantStreamFold::new();
    for (attempt, text) in [("s:1", "第一轮"), ("s:2", "第二轮")] {
        fold.apply(&frame(json!({
            "type": "start", "attemptId": attempt, "revision": 1, "turn": 1, "step": 1, "startedAfterSeq": 0
        })));
        fold.apply(&frame(json!({
            "type": "chunk", "attemptId": attempt, "revision": 2, "index": 0,
            "chunk": { "type": "text-delta", "index": 0, "text": text }
        })));
        assert_eq!(fold.text(), text);
    }

    assert_eq!(fold.attempt_id(), Some("s:2"));
    assert_eq!(fold.blocks().len(), 1, "旧尝试的块不残留");
}

#[test]
fn real_baseline_is_empty_and_resumes_cleanly() {
    let baseline = captured_baseline();
    // 静止态基线：只有 revision，没有进行中的尝试。
    assert_eq!(baseline.revision, 0);
    assert!(baseline.active_attempt.is_none());

    let mut fold = AssistantStreamFold::from_baseline(&baseline);
    assert!(fold.blocks().is_empty());
    assert!(!fold.is_streaming());

    // 基线 revision=0，真实流的第一帧正是 revision=1。
    for captured in captured_frames() {
        fold.apply(&captured);
    }
    assert_eq!(fold.text(), "收到");
    assert_eq!(fold.committed_seq(), Some(18));
    assert_eq!(fold.anomalies(), 0);
}

#[test]
fn mid_flight_baseline_restores_prefix_and_next_index() {
    let baseline: AssistantStreamBaseline = serde_json::from_value(json!({
        "revision": 5,
        "activeAttempt": {
            "attemptId": "s:9",
            "startedAfterSeq": 12,
            "turn": 1,
            "step": 1,
            "nextIndex": 2,
            "stream": [
                { "type": "block-start", "index": 0, "blockType": "text" },
                { "type": "text-delta", "index": 0, "text": "半句" }
            ]
        }
    }))
    .expect("基线形状应与源码一致");

    let mut fold = AssistantStreamFold::from_baseline(&baseline);
    assert_eq!(fold.text(), "半句", "基线里的前缀要恢复到界面上");
    assert!(fold.is_streaming());
    assert_eq!(fold.attempt_id(), Some("s:9"));

    // nextIndex=2 的帧必须正好接得上，不能判成空洞。
    let applied = fold.apply(&frame(json!({
        "type": "chunk", "attemptId": "s:9", "revision": 6, "index": 2,
        "chunk": { "type": "text-delta", "index": 0, "text": "补齐" }
    })));
    assert_eq!(applied, Apply::Applied);
    assert_eq!(fold.text(), "半句补齐");
}
