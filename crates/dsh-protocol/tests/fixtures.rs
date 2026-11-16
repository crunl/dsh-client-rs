//! 用真实抓包的 `fixtures` 校验反序列化。
//!
//! fixture 来源见 `fixtures/README.md`：2026-11 对 `@deepseek-ai/dsh@0.2.2-rc.3`
//! 实测抓取。**不是手写的期望值**，所以这些测试同时是 wire 形态的回归护栏。

use dsh_protocol::catalog::{ModelCatalog, PermissionCatalog};
use dsh_protocol::session::SessionListValue;
use dsh_protocol::stream::{
    AssistantStreamFrame, FollowValue, StreamChunk, StreamItem, StreamItemKind,
};
use dsh_protocol::wire::ServerResponse;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()))
}

#[test]
fn model_catalog_matches_captured_catalog() {
    let body: ServerResponse<ModelCatalog> =
        serde_json::from_str(&fixture("model-catalog.json")).expect("模型目录应可解析");
    let catalog = body.result.into_result().expect("该 fixture 是成功响应");

    assert_eq!(catalog.default.provider, "deepseek-official");
    assert_eq!(catalog.default.model, "deepseek-flash");
    assert_eq!(catalog.routable_providers, vec!["deepseek-official"]);
    assert!(!catalog.is_empty());
    // 这次抓包没有 provider 故障，但不能因此假定字段缺失。
    assert!(catalog.failures.is_empty());

    let group = catalog.groups.first().expect("至少一个 provider 组");
    assert_eq!(group.name, "DeepSeek");

    let flash = group
        .models
        .iter()
        .find(|m| m.id == "deepseek-flash")
        .expect("应有 deepseek-flash");

    let reasoning = flash.reasoning.as_ref().expect("该模型支持推理");
    let ids: Vec<&str> = reasoning.efforts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["off", "low", "high", "max"]);
    assert_eq!(reasoning.default_effort.as_deref(), Some("high"));
}

#[test]
fn permission_catalog_has_three_presets_and_no_descriptions() {
    let body: ServerResponse<PermissionCatalog> =
        serde_json::from_str(&fixture("permission-catalog.json")).expect("权限目录应可解析");
    let catalog = body.result.into_result().expect("该 fixture 是成功响应");

    let values: Vec<&str> = catalog.options.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(
        values,
        vec!["read-only", "workspace-write", "danger-full-access"],
        "出厂是三个 preset，不是两个"
    );
    assert_eq!(catalog.default_preset.as_deref(), Some("workspace-write"));
    assert_eq!(catalog.default_options.len(), 3);

    // 实测该构建不返回 description，UI 不能假定有。
    assert!(catalog.options.iter().all(|o| o.description.is_none()));
}

#[test]
fn session_list_parses_empty_result() {
    let body: ServerResponse<SessionListValue> =
        serde_json::from_str(&fixture("session-list.json")).expect("会话列表应可解析");
    let list = body.result.into_result().expect("该 fixture 是成功响应");
    assert!(list.items.is_empty(), "新 DSH_HOME 没有会话");
}

/// 持久化缓存来源的投影块用 `asOfSeq: -1` 表示水位未知（实测）。
/// 带符号接住它，否则整条 `session/list` 解码失败、侧栏直接空。
#[test]
fn session_list_accepts_unknown_watermark() {
    let body: ServerResponse<SessionListValue> = serde_json::from_str(
        r#"{"type":"server-response","rpcId":"x","result":{"ok":true,"value":{"items":[
          {"agentAvailable":false,"formatStatus":"current","sessionId":"s1",
           "updatedAt":1794758499960,"running":false,"blank":false,
           "cwd":"/tmp/ws",
           "projections":{"kind":"cached","asOfSeq":-1,"values":{"title":"旧会话"}}}
        ]}}}"#,
    )
    .expect("带 -1 水位的会话列表应可解析");
    let list = body.result.into_result().expect("成功响应");
    assert_eq!(list.items.len(), 1);
    let hints = list.items[0].projections.as_ref().expect("带投影提示");
    assert_eq!(hints.as_of_seq, -1);
    assert_eq!(list.items[0].title(), Some("旧会话"));
}

#[test]
fn remote_error_keeps_code_and_details() {
    let body: ServerResponse<serde_json::Value> =
        serde_json::from_str(&fixture("error-arguments-invalid.json")).expect("失败响应也应可解析");
    let error = body
        .result
        .into_result()
        .expect_err("这个 fixture 必须是失败");
    match error {
        dsh_protocol::wire::RpcResultError::Remote(remote) => {
            assert_eq!(remote.code, "gateway/arguments-invalid");
            assert_eq!(
                remote.details.get("endpoint").and_then(|v| v.as_str()),
                Some("session/events"),
                "详情里会指出是哪个端点"
            );
        }
        other => panic!("期望 Remote 失败，实际是 {other:?}"),
    }
}

#[test]
fn follow_stream_replays_real_frame_sequence() {
    let raw = fixture("follow-stream.jsonl");
    let items: Vec<StreamItem> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("每行都应是一个 Gateway 帧"))
        .collect();

    assert_eq!(items.len(), 13, "fixture 是 13 行");
    assert!(items.iter().all(|i| i.kind == StreamItemKind::Item));
    assert!(items.iter().all(|i| i.stream_id == "fx1"));

    let values: Vec<FollowValue> = items
        .iter()
        .filter_map(|i| i.value.clone())
        .map(|v| serde_json::from_value(v).expect("业务值应可解析"))
        .collect();

    assert!(matches!(values.first(), Some(FollowValue::Snapshot(_))));

    let mut chunk_types = Vec::new();
    let mut frames = 0;
    let mut committed = None;
    for value in &values {
        match value {
            FollowValue::Snapshot(snapshot) => {
                assert_eq!(snapshot.cursor, 2);
                assert!(!snapshot.has_more);
            }
            FollowValue::AssistantStream(stream) => {
                frames += 1;
                match &stream.frame {
                    AssistantStreamFrame::Start(start) => {
                        assert!(start.attempt_id.ends_with(":1"));
                        assert_eq!(start.revision, 1);
                        assert_eq!(start.turn, Some(1));
                        assert_eq!(start.started_after_seq, 14);
                    }
                    AssistantStreamFrame::Chunk(frame) => {
                        // revision 逐帧递增，所以「不连续即失效」在真实流量里可判定。
                        chunk_types.push(describe(&frame.chunk));
                    }
                    AssistantStreamFrame::End(end) => match &end.outcome {
                        dsh_protocol::stream::StreamOutcome::Committed { event_type, seq } => {
                            assert_eq!(event_type, "assistant/message");
                            committed = Some(*seq);
                        }
                        other => panic!("期望 committed，实际 {other:?}"),
                    },
                    AssistantStreamFrame::Unknown => panic!("不该有未知帧类型"),
                }
            }
            FollowValue::Event(_) | FollowValue::Unknown => {}
        }
    }

    assert_eq!(frames, 9, "9 个 assistant-stream 帧");
    assert_eq!(
        chunk_types,
        vec![
            "block-start:reasoning",
            "block-end",
            "block-start:text",
            "text-delta",
            "block-end",
            "usage",
            "finish",
        ],
        "实测的增量顺序"
    );
    assert_eq!(committed, Some(18));
}

#[test]
fn empty_reasoning_block_is_a_normal_occurrence() {
    let raw = fixture("follow-stream.jsonl");
    let mut saw_empty_reasoning = false;

    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let item: StreamItem = serde_json::from_str(line).expect("帧可解析");
        let Some(value) = item.value else { continue };
        let Ok(FollowValue::AssistantStream(stream)) = serde_json::from_value::<FollowValue>(value)
        else {
            continue;
        };
        if let AssistantStreamFrame::Chunk(frame) = &stream.frame
            && let StreamChunk::BlockEnd { block, .. } = &frame.chunk
            && block.get("type").and_then(|v| v.as_str()) == Some("reasoning")
            && block.get("text").and_then(|v| v.as_str()) == Some("")
        {
            saw_empty_reasoning = true;
        }
    }

    assert!(
        saw_empty_reasoning,
        "实测里 reasoning 块是空的，UI 必须容忍；这条断言防止有人把它当异常删掉"
    );
}

fn describe(chunk: &StreamChunk) -> String {
    match chunk {
        StreamChunk::BlockStart { block_type, .. } => format!("block-start:{block_type}"),
        StreamChunk::TextDelta { .. } => "text-delta".into(),
        StreamChunk::ReasoningDelta { .. } => "reasoning-delta".into(),
        StreamChunk::ToolCallDelta { .. } => "tool-call-delta".into(),
        StreamChunk::BlockEnd { .. } => "block-end".into(),
        StreamChunk::Usage { .. } => "usage".into(),
        StreamChunk::Finish { .. } => "finish".into(),
        StreamChunk::Unknown => "unknown".into(),
    }
}
