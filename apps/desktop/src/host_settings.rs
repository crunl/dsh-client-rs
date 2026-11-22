//! `ui-chat.transcriptView` 的 host settings 适配层。
//!
//! verbosity 的真值归 dsh profile：这里只通过 `settings/describe` /
//! `settings/update` 读写它，不碰 `cordis.patch.yml` 的路径，也不在本地落盘。
//! 写时带 `expectedRevision`，并发编辑会被 host 拒绝而不是静默覆盖。

use dsh_client_ui::ChatVerbosity;
use dsh_host::Connection;
use dsh_protocol::settings::{SettingsDescribeValue, SettingsNamespaceView, setting_value};
use serde_json::json;

const NAMESPACE: &str = "ui-chat";
const FIELD: &str = "transcriptView";

/// 读到的档位与它对应的 revision。写回时必须带上 revision。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatSetting {
    /// 当前生效档位；host 没写过时用客户端默认。
    pub verbosity: ChatVerbosity,
    /// namespace 的当前 revision。
    pub revision: u64,
}

/// 从 host 的 settings 视图里读 `ui-chat.transcriptView`。
pub async fn describe(conn: &Connection) -> Result<ChatSetting, String> {
    let value: SettingsDescribeValue = conn
        .call("settings/describe", json!({}))
        .await
        .map_err(|error| format!("读 host 设置失败: {error}"))?;
    if !value.writable {
        return Err("host profile 不接受设置写入".to_owned());
    }
    let namespace = value
        .namespaces
        .into_iter()
        .find(|item| item.ns == NAMESPACE)
        .ok_or_else(|| format!("host 没有 {NAMESPACE} 设置项"))?;
    Ok(from_namespace(&namespace))
}

/// 把档位写回 host profile，返回 host 确认后的实际档位。
pub async fn update(
    conn: &Connection,
    revision: u64,
    verbosity: ChatVerbosity,
) -> Result<ChatVerbosity, String> {
    let view: SettingsNamespaceView = conn
        .call(
            "settings/update",
            json!({
                "ns": NAMESPACE,
                "patch": { FIELD: verbosity.host_key() },
                "expectedRevision": revision,
            }),
        )
        .await
        .map_err(|error| format!("写 host 设置失败: {error}"))?;
    if view.ns != NAMESPACE {
        return Err(format!("host 返回了错误的设置 namespace: {}", view.ns));
    }
    Ok(from_namespace(&view).verbosity)
}

fn from_namespace(view: &SettingsNamespaceView) -> ChatSetting {
    let verbosity = setting_value(view, FIELD)
        .and_then(|value| value.as_str())
        .and_then(ChatVerbosity::parse_host)
        .unwrap_or_default();
    ChatSetting {
        verbosity,
        revision: view.revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(value: serde_json::Value, user: Option<serde_json::Value>) -> SettingsNamespaceView {
        SettingsNamespaceView {
            ns: NAMESPACE.to_owned(),
            value,
            user,
            revision: 7,
        }
    }

    #[test]
    fn respected_user_value_over_resolved_value() {
        let setting = from_namespace(&view(
            json!({ FIELD: "detailed" }),
            Some(json!({ FIELD: "compact" })),
        ));
        assert_eq!(setting.verbosity, ChatVerbosity::Low);
        assert_eq!(setting.revision, 7);
    }

    #[test]
    fn missing_or_unknown_value_uses_client_default() {
        let setting = from_namespace(&view(json!({}), None));
        assert_eq!(setting.verbosity, ChatVerbosity::Medium);
        let setting = from_namespace(&view(json!({ FIELD: "bogus" }), None));
        assert_eq!(setting.verbosity, ChatVerbosity::Medium);
    }

    #[test]
    fn all_four_official_values_round_trip() {
        for (value, expected) in [
            ("compact", ChatVerbosity::Low),
            ("standard", ChatVerbosity::Medium),
            ("detailed", ChatVerbosity::High),
            ("verbose", ChatVerbosity::Full),
        ] {
            let setting = from_namespace(&view(json!({ FIELD: value }), None));
            assert_eq!(setting.verbosity, expected, "读 {value}");
            assert_eq!(expected.host_key(), value, "写 {value}");
        }
    }
}
