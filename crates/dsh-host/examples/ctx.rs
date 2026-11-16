//! 摸清「上下文占用 + 缓存命中率」的真实来源。
//!
//! ```sh
//! cargo run -q --example ctx -p dsh-host -- "$(cat /tmp/uh.txt)"
//! ```

use dsh_host::Connection;
use dsh_protocol::session::SessionListValue;
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ready = std::env::args().nth(1).ok_or("用法: ctx <ready-url>")?;
    let conn = Connection::connect(&ready).await?;

    let list = conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await?;
    let target = list
        .items
        .iter()
        .find(|item| !item.blank)
        .ok_or("没有非空白会话")?;
    let session_id = target.session_id.clone();
    println!("会话 {session_id}");

    // `contextPressure` 是 SessionProjectionSummaryExcluded：session/list 里没有，
    // 只能单独用 session/projections 读。
    let value: Value = conn
        .call(
            "session/projections",
            json!({ "request": { "sessionId": session_id } }),
        )
        .await?;
    println!(
        "kind = {}",
        value.get("kind").cloned().unwrap_or(Value::Null)
    );

    if let Some(values) = value.get("values") {
        for key in ["contextPressure", "contextBreakdown", "tokenUsage"] {
            println!(
                "{key} = {}",
                serde_json::to_string(&values.get(key)).unwrap_or_default()
            );
        }

        // 官方公式（dsh-token-meter + ui-conversation）：
        //   used = projectedTokens ?? pressureTokens
        //   percent = min(100, round(used / contextWindow * 100))
        //   contextTokens = input + cacheRead + cacheWrite
        //   cacheHitRate = min(1, cacheRead / contextTokens)
        if let Some(pressure) = values.get("contextPressure") {
            let used = pressure
                .get("projectedTokens")
                .or_else(|| pressure.get("pressureTokens"))
                .and_then(Value::as_f64);
            let window = pressure.get("contextWindow").and_then(Value::as_f64);
            match (used, window) {
                (Some(used), Some(window)) if window > 0.0 => println!(
                    "占用 = {used} / {window} = {:.0}%（四舍五入 {}）",
                    used / window * 100.0,
                    (used / window * 100.0).round().min(100.0)
                ),
                _ => println!("占用：分子或分母缺失（used={used:?}, window={window:?}）"),
            }
        }

        if let Some(usage) = values.get("tokenUsage") {
            let read = usage
                .get("cacheReadTokens")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let context = read
                + usage
                    .get("uncachedInputTokens")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                + usage
                    .get("cacheWriteTokens")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
            if context > 0.0 {
                println!(
                    "缓存命中率 = {read} / {context} = {:.1}%",
                    (read / context).min(1.0) * 100.0
                );
            }
        }
    }

    Ok(())
}
