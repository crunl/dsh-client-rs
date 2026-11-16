//! 会话列表相关形状。
//!
//! 字段对应 `dsh-api-session-controller` 的 `SessionListValue` / `SessionSummary`。
//! 只落当前用得到的部分，其余字段被 serde 忽略，不会导致解析失败。

use serde::Deserialize;

/// `session/create` 的返回值。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateValue {
    /// 实际建出或复用的会话 id（可能与请求的不同）。
    pub session_id: String,
    /// 该会话运行的 preset，未组合 roster 时缺失。
    #[serde(default)]
    pub agent_preset: Option<String>,
}

/// `session/list` 的返回值。
#[derive(Debug, Clone, Deserialize)]
pub struct SessionListValue {
    /// 可见会话摘要，按活跃度排序。
    #[serde(default)]
    pub items: Vec<SessionSummary>,
}

/// 一条会话摘要。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// 该会话当前是否持有活着的 Agent。
    pub agent_available: bool,
    /// 持久化格式状态，仅在上报时出现。
    #[serde(default)]
    pub format_status: Option<FormatStatus>,
    /// 会话 id。
    pub session_id: String,
    /// 最近更新时间（毫秒时间戳）。
    pub updated_at: i64,
    /// 是否正在生成。
    pub running: bool,
    /// 是否空白会话（可直接复用）。
    pub blank: bool,
    /// 父会话 id，子会话才有。
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// 来源，目前只有 `subagent`。
    #[serde(default)]
    pub origin: Option<Origin>,
    /// 会话工作目录。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 投影提示块；键缺失表示未知。
    #[serde(default)]
    pub projections: Option<SessionProjectionHints>,
}

/// 持久化格式状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormatStatus {
    /// 当前格式。
    Current,
    /// 需要迁移。
    MigrationRequired,
}

/// 会话来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// 由 subagent 派生。
    Subagent,
}

/// 投影提示块。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjectionHints {
    /// `sequenced` 来自活注册表（`asOfSeq` 可与同连接其它帧比较）；
    /// `cached` 来自持久化投影缓存（`asOfSeq` 不可与连接内值比较）。
    pub kind: ProjectionHintKind,
    /// 该块在水位空间中的位置。
    ///
    /// 持久化缓存来源的条目会用 `-1` 表示「水位未知」（实测多会话工况）：
    /// 拿 `u64` 接会让整条 `session/list` 解码失败，所以存 `i64`，
    /// 只对 `sequenced` 水位才做比较。
    pub as_of_seq: i64,
    /// 已知的摘要值，键缺失即未知。
    #[serde(default)]
    pub values: serde_json::Value,
}

/// 投影提示来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionHintKind {
    /// 活注册表。
    Sequenced,
    /// 持久化缓存。
    Cached,
}

impl SessionSummary {
    /// 投影里的会话标题；空白会话或没有该投影时为 `None`。
    pub fn title(&self) -> Option<&str> {
        self.projections
            .as_ref()?
            .values
            .pointer("/title")?
            .as_str()
    }

    /// 投影里的权限 preset（composer 左侧 mode）。
    pub fn permission_preset(&self) -> Option<&str> {
        self.projections
            .as_ref()?
            .values
            .pointer("/permissions/currentValue")?
            .as_str()
    }

    /// 投影里的模型选择：`next` 是已选、**下一轮生效**的那个，`lastUsed` 是上一轮
    /// 实际用的；两者都没有时返回 `None`。
    pub fn model_selection(&self) -> Option<(&str, &str, Option<&str>)> {
        let values = &self.projections.as_ref()?.values;
        let picked = values
            .pointer("/modelSelection/next")
            .or_else(|| values.pointer("/modelSelection/lastUsed"))?;
        let provider = picked.get("provider")?.as_str()?;
        let model = picked.get("model")?.as_str()?;
        let effort = picked
            .get("reasoningEffort")
            .and_then(|value| value.as_str());
        Some((provider, model, effort))
    }

    /// 投影记录的工作目录；缺失时退回 `cwd` 字段。
    pub fn working_directory(&self) -> Option<&str> {
        self.projections
            .as_ref()
            .and_then(|hints| hints.values.pointer("/workingDirectory")?.as_str())
            .or(self.cwd.as_deref())
    }
}
