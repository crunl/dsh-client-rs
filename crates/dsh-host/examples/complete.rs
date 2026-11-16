//! 补全数据源探针：`commands/list` 与 `workspaceFiles/search`。
//!
//! ```bash
//! cargo run -p dsh-host --example complete -- <ready-url> <sessionId> <query>
//! ```

use dsh_host::Connection;
use dsh_protocol::commands::CommandDescriptor;
use dsh_protocol::files::{FileSearchRequest, FileSearchResult};
use dsh_protocol::session::SessionListValue;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("缺少 ready URL")?;
    let conn = Connection::connect(&url).await?;

    // 不传会话就取列表第一条，省得每次手工找 id。
    let session = match args.next() {
        Some(session) => session,
        None => {
            let list: SessionListValue =
                conn.call("session/list", json!({ "_request": {} })).await?;
            println!("可用会话：");
            for item in &list.items {
                println!("  {}  {:?}", item.session_id, item.cwd);
            }
            list.items
                .first()
                .map(|item| item.session_id.clone())
                .ok_or("host 上没有会话")?
        }
    };
    let query = args.next().unwrap_or_else(|| "main".to_owned());

    let commands: Vec<CommandDescriptor> = conn
        .call("commands/list", json!({ "agentId": session }))
        .await?;
    println!("命令 {} 条", commands.len());
    for command in commands.iter().take(12) {
        println!(
            "  /{:<14} {}{}",
            command.name,
            command.description.clone().unwrap_or_default(),
            command
                .input
                .as_ref()
                .and_then(|input| input.hint.clone())
                .map(|hint| format!("  [hint: {hint}]"))
                .unwrap_or_default()
        );
    }

    let result: FileSearchResult = conn
        .call(
            "workspaceFiles/search",
            json!({
                "workspaceFileScopeId": session,
                "request": FileSearchRequest::new(query),
            }),
        )
        .await?;
    println!(
        "文件命中 {} 条（截断={} 不完整={}）",
        result.matches.len(),
        result.truncated,
        result.incomplete
    );
    for hit in result.matches.iter().take(12) {
        println!(
            "  {:<48} {}",
            hit.path,
            hit.kind.clone().unwrap_or_default()
        );
    }

    Ok(())
}
