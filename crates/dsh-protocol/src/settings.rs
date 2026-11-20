//! dsh host settings API 的最小 wire 形状。
//!
//! 这里只描述 `settings/describe` / `settings/update` 返回中本项目要用的字段；
//! schema、secrets 等由 host 自己解释。配置文件（profile 的 patch layer）由 host
//! 独占，客户端只通过这两个端点读写 `ui-chat.transcriptView`。

use serde::Deserialize;
use serde_json::Value;

/// `settings/describe` 的结果。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDescribeValue {
    /// profile 是否接受写入。
    pub writable: bool,
    /// 是否有本地配置文档。仅用于诊断，客户端不拼它的路径。
    #[serde(default)]
    pub has_document: bool,
    /// 每个 profile entry 一份视图。
    #[serde(default)]
    pub namespaces: Vec<SettingsNamespaceView>,
}

/// 一个 namespace 的 redacted 视图。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsNamespaceView {
    /// namespace key，例如 `ui-chat`。
    pub ns: String,
    /// schema 默认值 + base + user 的解析结果。
    #[serde(default)]
    pub value: Value,
    /// 用户显式写过的那一层；字段存在即表示用户覆盖。
    #[serde(default)]
    pub user: Option<Value>,
    /// 写入时用来拒绝陈旧编辑的 revision。
    #[serde(default)]
    pub revision: u64,
}

/// 从 namespace 视图里取用户真正保存的值，没有覆盖时再看解析后的值。
pub fn setting_value<'a>(view: &'a SettingsNamespaceView, field: &str) -> Option<&'a Value> {
    view.user
        .as_ref()
        .and_then(|user| user.get(field))
        .or_else(|| view.value.get(field))
}
