//! 聊天渲染档位与它派生的详略深度。
//!
//! 档位是全局设置：唯一入口在 settings 页面，chat 通过 [`Detail`] 消费。
//! 持久化归 dsh host profile 的 `ui-chat.transcriptView`；这里只有内存镜像，
//! 不碰文件、不碰 host / protocol，也不解析会话事件。
//!
//! 档位名沿用官方四档；真正决定呈现的是它映射到的展开深度——见
//! `dsh_protocol::presentation::Detail`。

use dsh_protocol::presentation::Detail;

/// 聊天的全局详略档位，对应官方 `transcriptView` 四档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatVerbosity {
    /// 简洁：整轮折叠，只留轮次控件与回复。对应 `compact`。
    Low,
    /// 标准：整轮展开、过程组折叠，组头给摘要。对应 `standard`。
    #[default]
    Medium,
    /// 详细：过程组也展开，显示每条推理与工具。对应 `detailed`。
    High,
    /// 完全展开：全部平铺，连工具 / 推理正文也默认展开。对应 `verbose`。
    Full,
}

impl ChatVerbosity {
    /// 所有可选项，settings 页面按这个顺序画。
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Full];

    /// native UI 的稳定键，只用于控件 id 与调试覆盖。
    pub const fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Full => "full",
        }
    }

    /// UI 主标签。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "简洁",
            Self::Medium => "标准",
            Self::High => "详细",
            Self::Full => "完全展开",
        }
    }

    /// UI 里的英文档位名，便于和 CLI / 环境变量对上。
    pub const fn code(self) -> &'static str {
        self.key()
    }

    /// host profile 里 `ui-chat.transcriptView` 的官方值。
    pub const fn host_key(self) -> &'static str {
        match self {
            Self::Low => "compact",
            Self::Medium => "standard",
            Self::High => "detailed",
            Self::Full => "verbose",
        }
    }

    /// UI 说明：说清楚它实际改变什么，不写空泛的 marketing copy。
    pub const fn description(self) -> &'static str {
        match self {
            Self::Low => "已完成轮次收成一行，只显示你的输入与助手回复",
            Self::Medium => "已完成轮次收成一行；展开后只到过程组头，带实时详情",
            Self::High => "已完成轮次收成一行；展开后过程组也展开，看到每条推理与工具",
            Self::Full => "全部平铺、不折叠：推理与工具正文默认展开",
        }
    }

    /// 解析 host profile 里的值。
    ///
    /// 官方现在写 `compact / standard / detailed / verbose`；旧版可能留下
    /// `normal / expanded`，按官方规则读成 `detailed`。
    pub fn parse_host(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" | "compact" => Some(Self::Low),
            "medium" | "standard" => Some(Self::Medium),
            "high" | "detailed" | "normal" | "expanded" => Some(Self::High),
            "full" | "verbose" => Some(Self::Full),
            _ => None,
        }
    }

    /// 这一档对应的展开深度；档位与呈现之间唯一的桥。
    pub const fn detail(self) -> Detail {
        match self {
            Self::Low => Detail::Quiet,
            Self::Medium => Detail::Brief,
            Self::High => Detail::Standard,
            Self::Full => Detail::Full,
        }
    }
}

/// 客户端内存里的设置镜像。
///
/// 真值在 dsh host profile 的 `ui-chat.transcriptView`：启动时从
/// `settings/describe` 读入，settings 页面点击后经 supervisor 写回
/// `settings/update`。这里不读写 `config.toml` / `settings.json`，也没有文件
/// watcher；外部改动要等下一次 host 启动或设置页点击才会反映。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    /// 聊天详略档位。
    pub verbosity: ChatVerbosity,
}

#[cfg(test)]
mod tests {
    use super::ChatVerbosity;
    use dsh_protocol::presentation::Detail;

    #[test]
    fn host_keys_are_official_values() {
        assert_eq!(ChatVerbosity::Low.host_key(), "compact");
        assert_eq!(ChatVerbosity::Medium.host_key(), "standard");
        assert_eq!(ChatVerbosity::High.host_key(), "detailed");
        assert_eq!(ChatVerbosity::Full.host_key(), "verbose");
    }

    #[test]
    fn host_values_and_legacy_aliases_parse() {
        assert_eq!(
            ChatVerbosity::parse_host("compact"),
            Some(ChatVerbosity::Low)
        );
        assert_eq!(
            ChatVerbosity::parse_host("STANDARD"),
            Some(ChatVerbosity::Medium)
        );
        assert_eq!(
            ChatVerbosity::parse_host("detailed"),
            Some(ChatVerbosity::High)
        );
        assert_eq!(
            ChatVerbosity::parse_host("normal"),
            Some(ChatVerbosity::High)
        );
        assert_eq!(
            ChatVerbosity::parse_host("expanded"),
            Some(ChatVerbosity::High)
        );
        assert_eq!(
            ChatVerbosity::parse_host("verbose"),
            Some(ChatVerbosity::Full)
        );
        assert_eq!(ChatVerbosity::parse_host("bogus"), None);
    }

    #[test]
    fn each_verbosity_maps_to_a_distinct_detail_depth() {
        // 第一性不变量：四档必须落在四个不同的展开深度，否则切档「无差异」。
        let details = [
            ChatVerbosity::Low.detail(),
            ChatVerbosity::Medium.detail(),
            ChatVerbosity::High.detail(),
            ChatVerbosity::Full.detail(),
        ];
        assert_eq!(
            details,
            [Detail::Quiet, Detail::Brief, Detail::Standard, Detail::Full]
        );
    }

    #[test]
    fn default_is_medium() {
        assert_eq!(ChatVerbosity::default(), ChatVerbosity::Medium);
    }
}
