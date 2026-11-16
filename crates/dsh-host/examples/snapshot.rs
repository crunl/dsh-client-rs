//! 抓一次 `session/follow` 的 snapshot 记录形状（切会话要靠它回放历史）。
//!
//! ```text
//! cargo run -p dsh-host --example snapshot -- 'http://127.0.0.1:PORT/?token=…'
//! ```

use dsh_host::{Connection, FollowStream};
use dsh_protocol::session::SessionListValue;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ready_url = std::env::args()
        .nth(1)
        .ok_or("用法: snapshot <ready-url>")?;
    let conn = Connection::connect(&ready_url).await?;
    // 先看列表原始 JSON：标题到底在 projections 的哪个键里，不猜。
    let raw: serde_json::Value = conn.call("session/list", json!({ "_request": {} })).await?;
    let items = raw
        .pointer("/items")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    println!("会话数: {}", items.len());
    for item in &items {
        let title = item
            .pointer("/projections/values/title")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let cwd = item.pointer("/cwd").cloned().unwrap_or_default();
        let updated = item.pointer("/updatedAt").cloned().unwrap_or_default();
        println!("title={title} cwd={cwd} updatedAt={updated}");
    }
    let list: SessionListValue = serde_json::from_value(raw)?;
    let Some(first) = list
        .items
        .iter()
        .filter(|item| !item.blank)
        .max_by_key(|item| item.updated_at)
    else {
        println!("没有非空会话，先跑一次对话再来");
        return Ok(());
    };
    println!(
        "采样会话: {} cwd={:?} updatedAt={}",
        first.session_id, first.cwd, first.updated_at
    );

    let mut stream = FollowStream::open(
        &conn,
        "session/follow",
        json!({
            "request": {
                "address": { "kind": "session", "sessionId": first.session_id },
                "maxMessages": 40,
            }
        }),
    )
    .await?;

    while let Some(item) = stream.next_item().await? {
        let Some(value) = item.value else { continue };
        if value.get("type").and_then(|t| t.as_str()) != Some("snapshot") {
            continue;
        }
        let records = value.get("records").cloned().unwrap_or_default();
        println!("records={}", records.as_array().map(Vec::len).unwrap_or(0));
        for record in records.as_array().into_iter().flatten() {
            let kind = record
                .pointer("/event/type")
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            // 只打前两条完整记录，够看清 data 的层级。
            println!("记录类型: {kind}");
            if matches!(kind, "user/message" | "assistant/message") {
                println!("--- {kind} ---");
                println!("{}", serde_json::to_string_pretty(record)?);
            }
        }
        break;
    }
    Ok(())
}
