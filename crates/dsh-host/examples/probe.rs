//! headless 探针：不建窗口，只用真实 host 验证传输层。
//!
//! 用法（ready URL 由 `dsh … --no-open --port 0` 打到 stdout）：
//!
//! ```text
//! cargo run -p dsh-host --example probe -- 'http://127.0.0.1:PORT/?token=…'
//! ```
//!
//! 退出码：0 = 全部通过；1 = 有断言失败；2 = 用法/连接错误。

use dsh_host::Connection;
use dsh_protocol::catalog::PermissionCatalog;
use dsh_protocol::session::SessionListValue;
use serde_json::json;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Some(ready_url) = std::env::args().nth(1) else {
        eprintln!("用法: probe <ready-url>");
        return std::process::ExitCode::from(2);
    };

    match run(&ready_url).await {
        Ok(()) => {
            println!("\n全部通过");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("\n失败: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(ready_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    // 1) /connect 换 cookie。要收审批就必须声明 answer。
    let conn = Connection::connect(ready_url).await?;
    println!("握手成功: origin={}", conn.origin());

    // 2) session/list —— 参数名是 _request，不是 request。
    let list: SessionListValue = conn.call("session/list", json!({ "_request": {} })).await?;
    println!("session/list: {} 个会话", list.items.len());
    for item in &list.items {
        println!(
            "  {} running={} blank={} cwd={}",
            item.session_id,
            item.running,
            item.blank,
            item.cwd.as_deref().unwrap_or("-")
        );
    }

    // 3) permissionPresets/catalog —— 无参，出厂是三个 preset。
    let presets: PermissionCatalog = conn.call("permissionPresets/catalog", json!({})).await?;
    let values: Vec<&str> = presets.options.iter().map(|o| o.value.as_str()).collect();
    println!("permissionPresets/catalog: {values:?}");
    if values.len() != 3 {
        return Err(format!("期望 3 个 preset，实际 {}", values.len()).into());
    }

    // 4) 故意漏参，确认错误码与 details 形状（与 fixture 一致）。
    let failed: Result<serde_json::Value, _> = conn.call("session/events", json!({})).await;
    match failed {
        Err(dsh_host::HostError::Result(dsh_protocol::wire::RpcResultError::Remote(remote))) => {
            let endpoint = remote
                .details
                .get("endpoint")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            println!("漏参被拒: code={} endpoint={endpoint}", remote.code);
            if remote.code != "gateway/arguments-invalid" {
                return Err(format!("期望 gateway/arguments-invalid，实际 {}", remote.code).into());
            }
        }
        other => return Err(format!("期望 arguments-invalid，实际 {other:?}").into()),
    }

    Ok(())
}
