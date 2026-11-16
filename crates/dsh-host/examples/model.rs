//! 摸清模型选择的真实形状：目录、当前选择、以及 `session/selectModel` 的写入口。
//!
//! ```sh
//! cargo run -q --example model -p dsh-host -- "$(cat /tmp/um.txt)"
//! ```

use dsh_host::Connection;
use dsh_protocol::session::SessionListValue;
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ready = std::env::args().nth(1).ok_or("用法: model <ready-url>")?;
    let conn = Connection::connect(&ready).await?;

    // 1. 目录：无参端点，args 给空对象（与 permissionPresets/catalog 同形）。
    let catalog: Value = conn.call("session/modelCatalog", json!({})).await?;
    println!(
        "catalog.default = {}",
        serde_json::to_string(&catalog.get("default")).unwrap_or_default()
    );
    if let Some(groups) = catalog.get("groups").and_then(Value::as_array) {
        for group in groups {
            let provider = group.get("id").and_then(Value::as_str).unwrap_or("?");
            let name = group.get("name").and_then(Value::as_str).unwrap_or("?");
            let models = group
                .get("models")
                .and_then(Value::as_array)
                .map(|models| models.len())
                .unwrap_or(0);
            println!("  组 {provider}（{name}）：{models} 个模型");
            if let Some(models) = group.get("models").and_then(Value::as_array) {
                for model in models.iter().take(3) {
                    println!("    {}", serde_json::to_string(model).unwrap_or_default());
                }
            }
        }
    }
    println!(
        "catalog.failures = {}",
        serde_json::to_string(&catalog.get("failures")).unwrap_or_default()
    );

    // 2. 找一个非空白会话，看它的投影里跟模型有关的键。
    let list = conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await?;
    let target = list
        .items
        .iter()
        .find(|item| !item.blank)
        .ok_or("没有非空白会话")?;
    let session_id = target.session_id.clone();
    println!("\n会话 {session_id}");
    println!(
        "投影键 = {:?}",
        target
            .projections
            .as_ref()
            .map(|hints| hints
                .values
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default())
            .unwrap_or_default()
    );
    if let Some(hints) = target.projections.as_ref() {
        if let Some(model) = hints.values.get("model") {
            println!("  /model = {model}");
        }
        if let Some(selection) = hints.values.get("selection") {
            println!("  /selection = {selection}");
        }
    }

    // 3. 换一个模型：目录里挑一个与当前不同的。
    let Some(group) = catalog
        .get("groups")
        .and_then(Value::as_array)
        .and_then(|groups| groups.first())
    else {
        println!("目录为空，跳过写入");
        return Ok(());
    };
    let provider = group
        .get("id")
        .and_then(Value::as_str)
        .ok_or("目录组缺少 id")?;
    let Some(model) = group
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| models.last())
    else {
        println!("该组没有模型，跳过写入");
        return Ok(());
    };
    let model_id = model
        .get("id")
        .and_then(Value::as_str)
        .ok_or("模型缺少 id")?;
    let effort = model
        .pointer("/reasoning/defaultEffort")
        .and_then(Value::as_str);

    let mut request = json!({
        "sessionId": session_id,
        "provider": provider,
        "model": model_id,
    });
    if let Some(effort) = effort {
        request["reasoningEffort"] = json!(effort);
    }
    println!("\nselectModel request = {request}");
    let selected: Value = conn
        .call("session/selectModel", json!({ "request": request }))
        .await?;
    println!("selectModel = {selected}");

    // 4. 读回投影。
    let after = conn
        .call::<SessionListValue>("session/list", json!({ "_request": {} }))
        .await?;
    if let Some(hints) = after
        .items
        .iter()
        .find(|item| item.session_id == session_id)
        .and_then(|item| item.projections.as_ref())
    {
        for key in ["modelSelection", "tokenUsage"] {
            println!(
                "读回 /{key} = {}",
                serde_json::to_string(&hints.values.get(key)).unwrap_or_default()
            );
        }
    }

    Ok(())
}
