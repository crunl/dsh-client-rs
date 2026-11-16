//! 归档探针：`workspace/archiveSession` / `workspace/unarchiveSession`。
//!
//! ```bash
//! cargo run -p dsh-host --example archive -- <ready-url> [目录]
//! cargo run -p dsh-host --example archive -- <ready-url> --unarchive <sessionId>
//! ```
//!
//! 用例自建一个**一次性目录与会话**，归档完再取消归档，不碰你的已有会话。
//! host 没有删会话的接口——所谓「删除会话」到顶就是归档，记录与目录都留着。

use dsh_host::Connection;
use dsh_protocol::session::SessionCreateValue;
use dsh_protocol::workspace::{ArchiveSessionRequest, WorkspaceArchiveValue};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("缺少 ready URL")?;
    let rest: Vec<String> = args.collect();

    let conn = Connection::connect(&url).await?;

    // `--unarchive <id>`：只把某条已归档的会话放回来（清理试验残留用）。
    if rest.first().map(String::as_str) == Some("--unarchive") {
        let session_id = rest.get(1).ok_or("缺少 sessionId")?.clone();
        let restored: WorkspaceArchiveValue = conn
            .call(
                "workspace/unarchiveSession",
                json!({ "request": { "sessionId": session_id } }),
            )
            .await?;
        println!(
            "取消归档回执：{} 条，还含它={}",
            restored.archived_session_ids.len(),
            restored.archived_session_ids.contains(&session_id)
        );
        return Ok(());
    }

    // `--keep`：归档完就留着（看界面里的「已归档」区用）。
    let keep = rest.iter().any(|arg| arg == "--keep");
    let path = rest
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "/tmp/dsh-archive-probe".to_owned());
    std::fs::create_dir_all(&path)?;

    let created: SessionCreateValue = conn
        .call("session/create", json!({ "request": { "cwd": path } }))
        .await?;
    println!("建会话：{}", created.session_id);

    // 1) 归档。回的是整份集合，不是增量。
    let archived: WorkspaceArchiveValue = conn
        .call(
            "workspace/archiveSession",
            json!({ "request": ArchiveSessionRequest {
                session_id: &created.session_id,
                stop_activity: true,
            }}),
        )
        .await?;
    println!(
        "归档回执：{} 条，含新会话={}",
        archived.archived_session_ids.len(),
        archived.archived_session_ids.contains(&created.session_id)
    );

    // 2) 归档后它还在 `session/list` 里（记录没删，只是不在常驻列表显示）。
    let listed: dsh_protocol::session::SessionListValue =
        conn.call("session/list", json!({ "_request": {} })).await?;
    println!(
        "归档后仍在 session/list={}",
        listed
            .items
            .iter()
            .any(|item| item.session_id == created.session_id)
    );

    // 3) 取消归档。
    if keep {
        println!("留着归档不动（--keep）");
        return Ok(());
    }
    let restored: WorkspaceArchiveValue = conn
        .call(
            "workspace/unarchiveSession",
            json!({ "request": { "sessionId": created.session_id } }),
        )
        .await?;
    println!(
        "取消归档回执：{} 条，还含它={}",
        restored.archived_session_ids.len(),
        restored.archived_session_ids.contains(&created.session_id)
    );

    Ok(())
}
