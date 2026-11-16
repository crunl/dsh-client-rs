//! 活流量端到端探针：建会话 → 开 follow → 发 prompt → 用 fold 折叠真实帧。
//!
//! 用法：
//!
//! ```text
//! cargo run -p dsh-host --example follow -- 'http://127.0.0.1:PORT/?token=…' '你的问题'
//! ```
//!
//! 这会消耗一次模型调用。退出码：0 = 断言全过；1 = 断言失败；2 = 用法错误。

use std::time::Duration;

use dsh_host::{Connection, FollowStream};
use dsh_protocol::fold::{AssistantStreamFold, FoldOutcome};
use dsh_protocol::session::SessionCreateValue;
use dsh_protocol::stream::{FollowValue, StreamItemKind};
use serde_json::json;

/// 一次 prompt 最多等这么久。
const DEADLINE: Duration = Duration::from_secs(180);

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(ready_url), Some(prompt)) = (args.next(), args.next()) else {
        eprintln!("用法: follow <ready-url> <prompt>");
        return std::process::ExitCode::from(2);
    };

    match tokio::time::timeout(DEADLINE, run(&ready_url, &prompt)).await {
        Ok(Ok(())) => {
            println!("\n全部通过");
            std::process::ExitCode::SUCCESS
        }
        Ok(Err(error)) => {
            eprintln!("\n失败: {error}");
            std::process::ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("\n超时: {DEADLINE:?} 内没有收到终局");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(ready_url: &str, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect(ready_url).await?;
    println!("握手成功: {}", conn.origin());

    let cwd = std::env::current_dir()?.display().to_string();
    let created: SessionCreateValue = conn
        .call("session/create", json!({ "request": { "cwd": cwd } }))
        .await?;
    println!(
        "会话: {} (preset {:?})",
        created.session_id, created.agent_preset
    );

    // 先开 follow 再发 prompt：反过来会丢掉开头几帧。
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
    println!("已订阅: streamId={}", stream.stream_id());

    let receipt: serde_json::Value = conn
        .call(
            "session/prompt",
            json!({
                "request": {
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "sessionId": created.session_id,
                    "mode": "queue",
                    "content": [{ "type": "text", "text": prompt }],
                }
            }),
        )
        .await?;
    println!("prompt 已投递: {receipt}");

    let mut fold = AssistantStreamFold::new();
    let mut durable: Vec<(String, u64, String)> = Vec::new();
    let mut frames = 0_u32;

    while let Some(item) = stream.next_item().await? {
        match item.kind {
            StreamItemKind::Item => {}
            StreamItemKind::Error => {
                let failure = item.error.map(|e| format!("{e:?}")).unwrap_or_default();
                return Err(format!("流失败: {failure}").into());
            }
            StreamItemKind::End => break,
        }
        let Some(value) = item.value else { continue };
        match serde_json::from_value::<FollowValue>(value)? {
            FollowValue::Snapshot(snapshot) => {
                if let Some(baseline) = snapshot.assistant_stream {
                    fold = AssistantStreamFold::from_baseline(&baseline);
                }
                println!(
                    "snapshot: cursor={} records={} revision={:?}",
                    snapshot.cursor,
                    snapshot.records.len(),
                    fold.attempt_id()
                );
            }
            FollowValue::AssistantStream(stream_value) => {
                frames += 1;
                let before = fold.text();
                let applied = fold.apply(&stream_value.frame);
                let after = fold.text();
                if after != before {
                    println!("  [{applied:?}] {after}");
                }
            }
            FollowValue::Event(entry) => {
                let kind = entry
                    .event
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_owned();
                let seq = entry.event.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                let text = entry
                    .event
                    .pointer("/data/message/content")
                    .and_then(|v| v.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                durable.push((kind, seq, text));
                if fold.outcome().is_some() {
                    break;
                }
            }
            FollowValue::Unknown => {}
        }
    }

    println!("\n--- 折叠结果 ---");
    println!("assistant-stream 帧数: {frames}");
    println!("块数: {}", fold.blocks().len());
    println!("可见块: {}", fold.visible_blocks().count());
    println!("文本: {:?}", fold.text());
    println!(
        "usage: {:?}",
        fold.usage().map(|u| (u.input_tokens, u.output_tokens))
    );
    println!(
        "anomalies={} unsupported={}",
        fold.anomalies(),
        fold.unsupported()
    );

    let Some(outcome) = fold.outcome() else {
        return Err("流结束但没有终局".into());
    };
    let FoldOutcome::Committed { seq, .. } = outcome else {
        return Err(format!("期望 committed，实际 {outcome:?}").into());
    };

    // 交叉验证：fold 给的 seq 必须能在持久事件里找到同一条 assistant/message，
    // 且文本一致——否则流式累积和持久版本对不上。
    println!("committed seq={seq}");
    let committed = durable
        .iter()
        .find(|(kind, event_seq, _)| kind == "assistant/message" && event_seq == seq)
        .ok_or("持久事件里找不到对应 seq 的 assistant/message")?;

    if fold.text().is_empty() {
        return Err("折叠出的文本为空".into());
    }
    if committed.2 != fold.text() {
        return Err(format!(
            "流式文本 {:?} 与持久文本 {:?} 不一致",
            fold.text(),
            committed.2
        )
        .into());
    }
    if fold.usage().is_none() {
        return Err("没有收到 usage".into());
    }

    println!("持久文本与流式文本一致: {:?}", committed.2);
    Ok(())
}
