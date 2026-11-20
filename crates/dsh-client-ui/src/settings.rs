//! 聊天渲染档位与它派生的只读策略。
//!
//! 档位是全局设置：唯一入口在 settings 页面，chat 只消费 [`RenderPolicy`]。
//! 持久化归 dsh host profile 的 `ui-chat.transcriptView`；这里只有内存镜像，
//! 不碰文件、不碰 host / protocol，也不解析会话事件。

/// 聊天的全局详略档位。
///
/// 界面沿用 `low / medium / high` 三档；host profile 里用官方字段值
/// `compact / standard / detailed` 保存。官方第四档 `verbose` 在 native 客户端
/// 按最接近的 `High` 显示，但整轮历史展开的语义未实现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatVerbosity {
    /// 简洁：reasoning 可展开但无预览，组头只给类别摘要。
    Low,
    /// 标准：reasoning 首行预览，组头给类别摘要 + 运行中详情。
    #[default]
    Medium,
    /// 详细：运行中隐藏组头并直接显示组体；历史组保留摘要。
    High,
}

impl ChatVerbosity {
    /// 所有可选项，settings 页面按这个顺序画。
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// native UI 的稳定键，只用于控件 id 与调试覆盖。
    pub const fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// UI 主标签。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "简洁",
            Self::Medium => "标准",
            Self::High => "详细",
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
        }
    }

    /// UI 说明：说清楚它实际改变什么，不写空泛的 marketing copy。
    pub const fn description(self) -> &'static str {
        match self {
            Self::Low => "过程默认收起；reasoning 可展开但不给预览",
            Self::Medium => "过程默认收起；显示 reasoning 首行与运行中工具详情",
            Self::High => "运行中直接显示过程；历史过程保留可展开组头",
        }
    }

    /// 解析 host profile 里的值。
    ///
    /// 官方现在写 `compact / standard / detailed / verbose`；旧版可能留下
    /// `normal / expanded`，按官方规则读成 `detailed`。`verbose` 在本客户端
    /// 按最接近的 `High` 显示，不宣称实现它的历史全展开语义。
    pub fn parse_host(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" | "compact" => Some(Self::Low),
            "medium" | "standard" => Some(Self::Medium),
            "high" | "detailed" | "verbose" | "normal" | "expanded" => Some(Self::High),
            _ => None,
        }
    }

    /// 这一档对应的一组封闭渲染规则。
    pub const fn policy(self) -> RenderPolicy {
        match self {
            Self::Low => RenderPolicy {
                verbosity: self,
                reasoning_preview: false,
                group_header: GroupHeaderMode::Summary,
                group_body_open_while_running: false,
            },
            Self::Medium => RenderPolicy {
                verbosity: self,
                reasoning_preview: true,
                group_header: GroupHeaderMode::SummaryWithRunningDetail,
                group_body_open_while_running: false,
            },
            Self::High => RenderPolicy {
                verbosity: self,
                reasoning_preview: true,
                group_header: GroupHeaderMode::HistoricalOnly,
                group_body_open_while_running: true,
            },
        }
    }
}

/// 过程组头的显示规则。闭集，渲染层只消费它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupHeaderMode {
    /// 始终显示类别摘要；不给实时详情。
    Summary,
    /// 始终显示摘要；运行中额外显示实时工具详情。
    SummaryWithRunningDetail,
    /// 运行中隐藏组头；历史过程保留摘要。
    HistoricalOnly,
}

/// 从 `ChatVerbosity` 派生的只读渲染策略。
///
/// 这里只描述「这些行按什么详略画」。身份、顺序和内容仍由 `ChatTimeline` 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPolicy {
    /// 当前档位，只用于回显。
    pub verbosity: ChatVerbosity,
    /// settled reasoning 是否显示首行预览。
    pub reasoning_preview: bool,
    /// 组头规则。
    pub group_header: GroupHeaderMode,
    /// 运行中的过程组是否默认直接展开。
    pub group_body_open_while_running: bool,
}

impl RenderPolicy {
    /// 是否画过程组头。
    pub const fn group_header_visible(self, running_turn: bool) -> bool {
        !running_turn || !matches!(self.group_header, GroupHeaderMode::HistoricalOnly)
    }

    /// 组头是否追加实时工具详情。
    pub const fn show_running_hint(self, running_turn: bool) -> bool {
        running_turn && matches!(self.group_header, GroupHeaderMode::SummaryWithRunningDetail)
    }

    /// 过程组的默认开合；会话内的显式点选仍然优先。
    pub const fn group_body_open(self, running_turn: bool) -> bool {
        running_turn && self.group_body_open_while_running
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
    use super::{ChatVerbosity, GroupHeaderMode};

    #[test]
    fn host_keys_are_official_values() {
        assert_eq!(ChatVerbosity::Low.host_key(), "compact");
        assert_eq!(ChatVerbosity::Medium.host_key(), "standard");
        assert_eq!(ChatVerbosity::High.host_key(), "detailed");
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
            Some(ChatVerbosity::High)
        );
        assert_eq!(ChatVerbosity::parse_host("bogus"), None);
    }

    #[test]
    fn policy_keeps_rendering_rules_in_one_place() {
        let low = ChatVerbosity::Low.policy();
        let medium = ChatVerbosity::Medium.policy();
        let high = ChatVerbosity::High.policy();

        assert!(!low.reasoning_preview);
        assert_eq!(low.group_header, GroupHeaderMode::Summary);
        assert!(low.group_header_visible(true));
        assert!(!low.show_running_hint(true));
        assert!(!low.group_body_open(true));

        assert!(medium.reasoning_preview);
        assert_eq!(
            medium.group_header,
            GroupHeaderMode::SummaryWithRunningDetail
        );
        assert!(medium.group_header_visible(true));
        assert!(medium.show_running_hint(true));
        assert!(!medium.show_running_hint(false));
        assert!(!medium.group_body_open(true));

        assert!(high.reasoning_preview);
        assert_eq!(high.group_header, GroupHeaderMode::HistoricalOnly);
        assert!(!high.group_header_visible(true));
        assert!(high.group_header_visible(false));
        assert!(!high.show_running_hint(true));
        assert!(high.group_body_open(true));
        assert!(!high.group_body_open(false));
    }

    #[test]
    fn default_is_medium() {
        assert_eq!(ChatVerbosity::default(), ChatVerbosity::Medium);
    }
}
