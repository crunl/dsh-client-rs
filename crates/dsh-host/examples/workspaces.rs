//! 工作区注册探针：`workspace/follow` / `workspace/create` / `workspace/delete`。
//!
//! ```bash
//! cargo run -p dsh-host --example workspaces -- <ready-url> [目录]
//! ```
//!
//! 不给目录就只用 `/tmp/dsh-ws-probe`（用完自己清理）。这个例子**不碰**你的已有
//! 工作区：只注册、只删除它自己刚建的那一条。

use dsh_host::{Connection, FollowStream};
use dsh_protocol::workspace::{WorkspaceCreated, WorkspaceDeleted, WorkspaceFollowFrame};
use serde_json::json;

/// 拉一帧，最多等 1.5 秒；超时当没有。
async fn next_frame(stream: &mut FollowStream) -> Option<serde_json::Value> {
    match tokio::time::timeout(std::time::Duration::from_millis(1500), stream.next_item()).await {
        Ok(Ok(Some(item))) => item.value,
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("缺少 ready URL")?;
    let path = args
        .next()
        .unwrap_or_else(|| "/tmp/dsh-ws-probe".to_owned());
    std::fs::create_dir_all(&path)?;

    let conn = Connection::connect(&url).await?;

    // 1) 先订阅，再改注册表：这样才能看到 upsert / remove 增量。
    let mut stream = FollowStream::open(&conn, "workspace/follow", json!({})).await?;
    if let Some(item) = stream.next_item().await? {
        let raw = serde_json::to_string_pretty(&item.value)?;
        println!("基线原始帧：\n{raw}");
        let frame: WorkspaceFollowFrame = serde_json::from_value(item.value.unwrap_or_default())?;
        if let WorkspaceFollowFrame::Baseline { value } = frame {
            println!("基线里已有 {} 个工作区：", value.items.len());
            for workspace in &value.items {
                println!("  {}  {}", workspace.workspace_id, workspace.path);
            }
        }
    }

    // 2) 注册临时目录。
    let created: WorkspaceCreated = conn
        .call("workspace/create", json!({ "request": { "path": path } }))
        .await?;
    println!(
        "注册 {}：created={} id={}",
        path, created.created, created.workspace.workspace_id
    );
    while let Some(value) = next_frame(&mut stream).await {
        println!("增量帧：{}", serde_json::to_string(&value)?);
    }

    // 3) 删掉这条注册，目录必须原地保留。
    let deleted: WorkspaceDeleted = conn
        .call(
            "workspace/delete",
            json!({ "request": { "workspaceId": created.workspace.workspace_id } }),
        )
        .await?;
    println!("删除注册：deleted={}", deleted.deleted);
    println!(
        "目录还在吗：{}",
        if std::path::Path::new(&path).is_dir() {
            "在"
        } else {
            "没了（不该）"
        }
    );
    while let Some(value) = next_frame(&mut stream).await {
        println!("增量帧：{}", serde_json::to_string(&value)?);
    }

    Ok(())
}
