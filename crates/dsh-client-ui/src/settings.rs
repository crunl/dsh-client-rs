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
            Self::Low => "过程收起为类别统计；推理不给摘要",
            Self::Medium => "过程收起为类别统计；运行中临时展开，推理显示摘要",
            Self::High => "过程默认展开；推理显示摘要",
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
                reasoning_summary: false,
                live_group_summary: false,
                expand_running_group: false,
                expand_history_group: false,
            },
            Self::Medium => RenderPolicy {
                verbosity: self,
                reasoning_summary: true,
                live_group_summary: true,
                expand_running_group: true,
                expand_history_group: false,
            },
            Self::High => RenderPolicy {
                verbosity: self,
                reasoning_summary: true,
                live_group_summary: true,
                expand_running_group: true,
                expand_history_group: true,
            },
        }
    }
}

/// 从 `ChatVerbosity` 派生的只读渲染策略。
///
/// 三档共享同一条基线：过程默认收起成一行类别统计，推理永不灌进 assistant 正文。
/// 档位只调四个旋钮——组头统计与实时摘要的取舍、收起推理是否给摘要、运行中与
/// 历史过程组的默认开合。身份、顺序和内容仍由 `ChatTimeline` 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPolicy {
    /// 当前档位，只用于回显。
    pub verbosity: ChatVerbosity,
    /// 收起态 reasoning 是否给一行摘要。
    pub reasoning_summary: bool,
    /// 运行中组头是否用最后一条工具摘要替代类别统计。
    pub live_group_summary: bool,
    /// 运行中过程组是否默认展开；完成即回折，用户点选优先。
    pub expand_running_group: bool,
    /// 历史过程组是否默认展开。
    pub expand_history_group: bool,
}

impl RenderPolicy {
    /// 收起态 reasoning 是否显示摘要。
    pub const fn show_reasoning_summary(self) -> bool {
        self.reasoning_summary
    }

    /// 运行中组头是否显示实时工具摘要而非类别统计。
    pub const fn show_live_summary(self, running_turn: bool) -> bool {
        running_turn && self.live_group_summary
    }

    /// 过程组的默认开合；会话内的显式点选仍然优先。
    pub const fn group_body_open(self, running_turn: bool) -> bool {
        if running_turn {
            self.expand_running_group
        } else {
            self.expand_history_group
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

        // 共同基线：无档位把推理灌进正文，组头始终存在。
        assert!(!low.show_reasoning_summary());
        assert!(medium.show_reasoning_summary());
        assert!(high.show_reasoning_summary());

        // 只有中/高档在运行中把组头换成实时摘要。
        assert!(!low.show_live_summary(true));
        assert!(medium.show_live_summary(true));
        assert!(!medium.show_live_summary(false));
        assert!(high.show_live_summary(true));

        // 默认开合：低档永不展开；中档运行中展开、完成回折；高档连历史也展开。
        assert!(!low.group_body_open(true));
        assert!(!low.group_body_open(false));
        assert!(medium.group_body_open(true));
        assert!(!medium.group_body_open(false));
        assert!(high.group_body_open(true));
        assert!(high.group_body_open(false));
    }

    #[test]
    fn default_is_medium() {
        assert_eq!(ChatVerbosity::default(), ChatVerbosity::Medium);
    }
}
