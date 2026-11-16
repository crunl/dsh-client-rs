//! 斜杠命令目录（`commands/*` 命名空间）。
//!
//! `commands/list` 是补全的唯一来源：只列**当前 agent 可见**的命令（已被作用域
//! 遮蔽后的结果），不是全站清单。

use serde::Deserialize;

/// 一条命令描述。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDescriptor {
    /// 插件自家身份，客户端按它区分特殊命令；大部分命令没有。
    #[serde(default)]
    pub definition_id: Option<String>,
    /// 命令名，**不带**前导斜杠。
    pub name: String,
    /// 发现用的摘要。
    #[serde(default)]
    pub description: Option<String>,
    /// 参数形态提示。
    #[serde(default)]
    pub input: Option<CommandInput>,
}

/// 命令的参数形态。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInput {
    /// 用户还没开始输入时显示的占位提示。
    #[serde(default)]
    pub hint: Option<String>,
    /// 是否接受随行附件；缺失或 `false` 表示带附件会被拒绝。
    #[serde(default)]
    pub attachments: Option<bool>,
}
