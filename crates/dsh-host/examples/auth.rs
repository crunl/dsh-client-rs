//! 验证认证失效（401）的处置：重新握手 + 重试，以及最后的可行动报错。
//!
//! ```sh
//! cargo run -q --example auth -p dsh-host -- "$(cat /tmp/ready-url.txt)"
//! ```
//!
//! 设 `AUTH_WAIT=<秒>` 可让脚本在中间把 host 换掉（同端口新进程），
//! 复现「host 重启后旧 cookie 一律 401」这一实际故障。

use std::time::Duration;

use dsh_host::{Connection, FollowStream};
use dsh_protocol::session::SessionListValue;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ready = std::env::args().nth(1).ok_or("用法: auth <ready-url>")?;

    let conn = Connection::connect(&ready).await?;
    println!("connect: ok");

    let list = conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await?;
    println!("list1: ok（{} 个会话）", list.items.len());

    if let Ok(seconds) = std::env::var("AUTH_WAIT") {
        let seconds: u64 = seconds.parse()?;
        println!("等待 {seconds}s，期间脚本换掉 host…");
        tokio::time::sleep(Duration::from_secs(seconds)).await;
    }

    match conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await
    {
        Ok(list) => println!("list2: ok（{} 个会话）", list.items.len()),
        Err(error) => println!("list2: {error}"),
    }

    match conn.refresh().await {
        Ok(()) => println!("refresh: ok"),
        Err(error) => println!("refresh: {error}"),
    }

    // 重新握手成功后，流层拿的必须是新 cookie。列表流不会自己结束，读几帧就够。
    match FollowStream::open(&conn, "session/list", json!({ "_request": {} })).await {
        Ok(mut stream) => {
            let mut frames = 0;
            let deadline = Duration::from_secs(3);
            let read = async {
                while let Ok(Some(item)) = stream.next_item().await {
                    frames += 1;
                    if item.kind == dsh_protocol::stream::StreamItemKind::End {
                        break;
                    }
                }
            };
            match tokio::time::timeout(deadline, read).await {
                Ok(()) => println!("stream: ok（{frames} 帧，正常收尾）"),
                Err(_) => println!("stream: ok（{frames} 帧，{deadline:?} 内未收尾）"),
            }
        }
        Err(error) => println!("stream: {error}"),
    }

    Ok(())
}
