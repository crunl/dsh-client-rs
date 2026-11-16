//! `workspace/create` 的实测形状（2026-11，0.2.2-rc.3）。

use dsh_protocol::workspace::{
    ArchiveSessionRequest, WorkspaceArchiveValue, WorkspaceCreated, WorkspaceFollowFrame,
    WorkspaceRegistry,
};

/// `workspace/follow` 的真实帧（2026-11，0.2.2-rc.3 实测）。
#[test]
fn parses_follow_baseline_and_increments() {
    let baseline = r#"{"type":"baseline","value":{"items":[{"workspaceId":"w1","path":"/private/tmp/a","title":"a","sessionIds":["s1"],"createdAt":"2026-11-14T20:20:32.559Z","updatedAt":"2026-11-14T20:20:32.559Z"}],"archivedSessionIds":[],"pinnedSessionIds":[]}}"#;
    let upsert = r#"{"type":"upsert","workspace":{"workspaceId":"w2","path":"/private/tmp/b","title":"b","sessionIds":[],"createdAt":"2026-11-14T21:32:51.695Z","updatedAt":"2026-11-14T21:32:51.695Z"}}"#;
    let order = r#"{"type":"order","workspaceIds":["w2","w1"]}"#;
    let remove = r#"{"type":"remove","workspaceId":"w1"}"#;

    let mut registry = WorkspaceRegistry::default();
    for frame in [baseline, upsert, order, remove] {
        let parsed: WorkspaceFollowFrame = serde_json::from_str(frame).unwrap();
        registry.apply(parsed);
    }

    assert_eq!(registry.items.len(), 1);
    assert_eq!(
        registry
            .items
            .first()
            .map(|item| item.workspace_id.as_str()),
        Some("w2")
    );
}

/// `order` 是权威顺序：`upsert` 先到、`order` 后到时两边都得对上。
#[test]
fn order_frame_reorders_known_items() {
    let mut registry = WorkspaceRegistry::default();
    for frame in [
        r#"{"type":"upsert","workspace":{"workspaceId":"a","path":"/a","title":"a"}}"#,
        r#"{"type":"upsert","workspace":{"workspaceId":"b","path":"/b","title":"b"}}"#,
        r#"{"type":"order","workspaceIds":["b","a"]}"#,
    ] {
        let parsed: WorkspaceFollowFrame = serde_json::from_str(frame).unwrap();
        registry.apply(parsed);
    }
    let ids: Vec<&str> = registry
        .items
        .iter()
        .map(|item| item.workspace_id.as_str())
        .collect();
    assert_eq!(ids, ["b", "a"]);
}

/// 归档帧给的是**整份**集合：多一条要重画，重复一份不重画。
#[test]
fn archived_frame_replaces_the_whole_set() {
    let mut registry = WorkspaceRegistry::default();
    let frame: WorkspaceFollowFrame =
        serde_json::from_str(r#"{"type":"archived","archivedSessionIds":["s1","s2"]}"#).unwrap();
    assert!(registry.apply(frame));
    assert!(registry.is_archived("s2"));

    let same: WorkspaceFollowFrame =
        serde_json::from_str(r#"{"type":"archived","archivedSessionIds":["s1","s2"]}"#).unwrap();
    assert!(!registry.apply(same));

    // 取消归档后回的是更短的一份。
    let fewer: WorkspaceFollowFrame =
        serde_json::from_str(r#"{"type":"archived","archivedSessionIds":["s2"]}"#).unwrap();
    assert!(registry.apply(fewer));
    assert!(!registry.is_archived("s1"));
}

/// 基线带归档集合时要一起装进来（重连后侧栏不该把已归档的又列出来）。
#[test]
fn baseline_carries_archived_and_pinned() {
    let mut registry = WorkspaceRegistry::default();
    let frame: WorkspaceFollowFrame = serde_json::from_str(
        r#"{"type":"baseline","value":{"items":[],"archivedSessionIds":["s9"],"pinnedSessionIds":["s1"]}}"#,
    )
    .unwrap();
    assert!(registry.apply(frame));
    assert!(registry.is_archived("s9"));
    assert_eq!(registry.pinned_session_ids, ["s1"]);
}

/// `archiveSession` 的入参键是 `sessionId` + `stopActivity`。
#[test]
fn archive_request_serializes_camel_case() {
    let raw = serde_json::to_value(ArchiveSessionRequest {
        session_id: "s1",
        stop_activity: true,
    })
    .unwrap();
    assert_eq!(raw["sessionId"], "s1");
    assert_eq!(raw["stopActivity"], true);
}

/// 归档两个接口都回 `{archivedSessionIds}`。
#[test]
fn parses_archive_receipt() {
    let parsed: WorkspaceArchiveValue =
        serde_json::from_str(r#"{"archivedSessionIds":["s1","s2"]}"#).unwrap();
    assert_eq!(parsed.archived_session_ids.len(), 2);
}

/// 置顶帧这一版不用，但得存下来、且重复一份不算变化。
#[test]
fn pinned_frame_updates_without_hiding_sessions() {
    let mut registry = WorkspaceRegistry::default();
    let frame: WorkspaceFollowFrame =
        serde_json::from_str(r#"{"type":"pinned","pinnedSessionIds":["s1"]}"#).unwrap();
    assert!(registry.apply(frame));
    assert_eq!(registry.pinned_session_ids, ["s1"]);
    assert!(!registry.is_archived("s1"));
}

/// `workspace/delete` 只回一个 `deleted`。
#[test]
fn parses_delete_receipt() {
    let parsed: dsh_protocol::workspace::WorkspaceDeleted =
        serde_json::from_str(r#"{"deleted":true}"#).unwrap();
    assert!(parsed.deleted);
}

#[test]
fn parses_registered_workspace() {
    let raw = r#"{
        "workspace": {
            "workspaceId": "6f1c0d5a-1c1e-4f11-9d1f-0a5f43a0e111",
            "path": "/tmp/dsh-ws-demo",
            "title": "dsh-ws-demo",
            "sessionIds": ["3a3e3d5e-0000-4000-8000-000000000001"],
            "createdAt": "2026-11-15T04:21:33.120Z",
            "updatedAt": "2026-11-15T04:21:33.120Z"
        },
        "created": true
    }"#;

    let parsed: WorkspaceCreated = serde_json::from_str(raw).unwrap();
    assert!(parsed.created);
    assert_eq!(parsed.workspace.path, "/tmp/dsh-ws-demo");
    assert_eq!(parsed.workspace.title, "dsh-ws-demo");
    assert_eq!(parsed.workspace.session_ids.len(), 1);
}

#[test]
fn tolerates_missing_timestamps_and_unknown_fields() {
    // 幂等注册（created=false）时各字段照样齐全，但缺时间戳不该让整次注册失败。
    let raw = r#"{"workspace":{"workspaceId":"w1","path":"/tmp","title":"tmp"},"created":false,"extra":1}"#;

    let parsed: WorkspaceCreated = serde_json::from_str(raw).unwrap();
    assert!(!parsed.created);
    assert!(parsed.workspace.session_ids.is_empty());
    assert!(parsed.workspace.created_at.is_none());
}
