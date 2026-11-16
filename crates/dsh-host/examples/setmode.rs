//! 验证权限预设的写入路径：斜杠命令 `/permission <preset>` 是否被 host 收下，
//! 以及改完之后从哪里读回当前值。
//!
//! ```text
//! cargo run -p dsh-host --example setmode -- 'http://127.0.0.1:PORT/?token=…'
//! ```

use std::time::Duration;

use dsh_host::{Connection, FollowStream};
use dsh_protocol::catalog::PermissionCatalog;
use dsh_protocol::session::{SessionCreateValue, SessionListValue};
use dsh_protocol::stream::StreamItemKind;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ready_url = std::env::args().nth(1).ok_or("用法: setmode <ready-url>")?;
    let conn = Connection::connect(&ready_url).await?;

    let catalog: PermissionCatalog = conn.call("permissionPresets/catalog", json!({})).await?;
    println!("catalog: {} 个选项", catalog.options.len());

    let created: SessionCreateValue = conn
        .call("session/create", json!({ "request": { "cwd": "/tmp" } }))
        .await?;
    println!("会话: {}", created.session_id);
    println!(
        "初始 preset: {:?}",
        current_preset(&conn, &created.session_id).await?
    );

    let mut stream = FollowStream::open(
        &conn,
        "session/follow",
        json!({
            "request": {
                "address": { "kind": "session", "sessionId": created.session_id },
                "assistantStream": true,
                "activate": true,
            }
        }),
    )
    .await?;

    // 官方 UI 走的是 `commands/execute`（不是 session/prompt——实测那样会被喂给模型）：
    // `{ agentId, line, submittedAttachments }`，返回 undefined 表示没有命令匹配。
    let execution: serde_json::Value = conn
        .call(
            "commands/execute",
            json!({
                "agentId": created.session_id,
                "line": "/permission read-only",
                "submittedAttachments": [],
            }),
        )
        .await?;
    println!("commands/execute 返回: {execution}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut permission_events = 0_u32;
    let mut assistant_frames = 0_u32;
    while tokio::time::Instant::now() < deadline {
        let Ok(Ok(Some(item))) =
            tokio::time::timeout(Duration::from_secs(5), stream.next_item()).await
        else {
            break;
        };
        if item.kind == StreamItemKind::End {
            break;
        }
        let Some(value) = item.value else { continue };
        let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        match kind {
            "event" => {
                let event_type = value
                    .pointer("/event/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                if event_type.contains("permission") || event_type.contains("sandbox") {
                    permission_events += 1;
                    println!(
                        "  事件 {event_type}: {}",
                        value.pointer("/event/data").cloned().unwrap_or_default()
                    );
                }
            }
            "assistant-stream" => {
                assistant_frames += 1;
                if assistant_frames == 1 {
                    println!("  ⚠ 收到 assistant-stream：说明这句话被当成普通提示喂给模型了");
                }
            }
            _ => {}
        }
        if permission_events > 0 && assistant_frames == 0 {
            break;
        }
    }

    println!("permission 相关事件 {permission_events} 条，assistant 帧 {assistant_frames} 帧");
    println!(
        "改后 preset: {:?}",
        current_preset(&conn, &created.session_id).await?
    );
    Ok(())
}

/// 从 `session/list` 的投影里读当前 preset。
async fn current_preset(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let list: SessionListValue = conn.call("session/list", json!({ "_request": {} })).await?;
    let item = list.items.iter().find(|item| item.session_id == session_id);
    Ok(item.and_then(|item| {
        item.projections
            .as_ref()?
            .values
            .pointer("/permissions/currentValue")?
            .as_str()
            .map(str::to_owned)
    }))
}
