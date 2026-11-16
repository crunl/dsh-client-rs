//! 工作区注册（`workspace/*` 命名空间）。
//!
//! 侧栏的「添加目录」走这里：`workspace/create` 只是把一个**已存在的目录**
//! 注册成工作区（幂等）, 不会建会话，也没有目录选择器——路径得由调用方给。

use serde::Deserialize;

/// 一个已注册的工作区。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    /// 工作区 id。
    pub workspace_id: String,
    /// 规范化后的 host 目录路径。
    pub path: String,
    /// 用户可见标题。
    pub title: String,
    /// 记在这个工作区下的会话，按手工顺序。
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// 创建时刻（ISO-8601）。
    #[serde(default)]
    pub created_at: Option<String>,
    /// 最近一次变更时刻（ISO-8601）。
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// `workspace/create` 的结果。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCreated {
    /// 新建或此前已注册的工作区。
    pub workspace: WorkspaceView,
    /// 这次调用是否真的新建了注册（已存在时为 `false`）。
    pub created: bool,
}

/// `workspace/delete` 的结果。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct WorkspaceDeleted {
    /// 删掉一条注册就是 `true`。
    pub deleted: bool,
}

/// `workspace/follow` 的一代基线（每代连接的开场帧）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBaseline {
    /// 当前注册的全部工作区，按注册表顺序。
    #[serde(default)]
    pub items: Vec<WorkspaceView>,
    /// 已归档的会话。
    #[serde(default)]
    pub archived_session_ids: Vec<String>,
    /// 已置顶的会话。
    #[serde(default)]
    pub pinned_session_ids: Vec<String>,
}

/// `workspace/follow` 的一帧。
///
/// 线上是 `{type, …}` 打平的形状，不是 `{type, value}` 的包裹：只有 `baseline`
/// 与 `replace` 带 `value`，其余变体各自带字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WorkspaceFollowFrame {
    /// 新一代连接的开场基线。
    Baseline {
        /// 整份注册表。
        value: WorkspaceBaseline,
    },
    /// 全量替换（与基线同形），supersede 之前收到的全部状态。
    Replace {
        /// 整份注册表。
        value: WorkspaceBaseline,
    },
    /// 新增或更新一个工作区。
    Upsert {
        /// 该工作区。
        workspace: WorkspaceView,
    },
    /// 删掉一个工作区注册（目录与会话都还在）。
    Remove {
        /// 被删的 id。
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    /// 注册表排序变更。
    Order {
        /// 新的完整顺序。
        #[serde(rename = "workspaceIds")]
        workspace_ids: Vec<String>,
    },
    /// 归档集合变更。
    Archived {
        /// 已归档的会话。
        #[serde(rename = "archivedSessionIds")]
        archived_session_ids: Vec<String>,
    },
    /// 置顶集合变更。
    Pinned {
        /// 已置顶的会话。
        #[serde(rename = "pinnedSessionIds")]
        pinned_session_ids: Vec<String>,
    },
}

/// `workspace/archiveSession` 的入参。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSessionRequest<'a> {
    /// 目标会话。
    pub session_id: &'a str,
    /// 会话还在跑时要先停掉它（turn / 子 agent / 后台任务 / 定时）。
    /// `false` 时 host 会以 `workspace/session-active` 拒绝归档。
    pub stop_activity: bool,
}

/// `workspace/archiveSession` 与 `workspace/unarchiveSession` 的结果。
/// 回的是**整份**归档集合，不是增量。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveValue {
    /// 归档后完整的已归档会话 id。
    #[serde(default)]
    pub archived_session_ids: Vec<String>,
}

/// 侧栏真正需要的那份状态：注册表 + 归档集合 + 置顶集合。
///
/// 三者都来自同一条 `workspace/follow`：分开存会让「哪一帧重画侧栏」散落在
/// 调用方，所以叠成一个结构，`apply` 自己判断有没有变。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRegistry {
    /// 已注册的工作区，按注册表顺序。
    pub items: Vec<WorkspaceView>,
    /// 已归档的会话 id（`archived` 帧给的是整份集合）。
    pub archived_session_ids: Vec<String>,
    /// 已置顶的会话 id。
    pub pinned_session_ids: Vec<String>,
}

impl WorkspaceRegistry {
    /// 把一帧合进来；返回 `true` 表示侧栏需要重画。
    pub fn apply(&mut self, frame: WorkspaceFollowFrame) -> bool {
        match frame {
            WorkspaceFollowFrame::Baseline { value } | WorkspaceFollowFrame::Replace { value } => {
                let changed = self.items != value.items
                    || self.archived_session_ids != value.archived_session_ids
                    || self.pinned_session_ids != value.pinned_session_ids;
                self.items = value.items;
                self.archived_session_ids = value.archived_session_ids;
                self.pinned_session_ids = value.pinned_session_ids;
                changed
            }
            WorkspaceFollowFrame::Upsert { workspace } => {
                match self
                    .items
                    .iter_mut()
                    .find(|item| item.workspace_id == workspace.workspace_id)
                {
                    Some(slot) => *slot = workspace,
                    None => self.items.push(workspace),
                }
                true
            }
            WorkspaceFollowFrame::Remove { workspace_id } => {
                let before = self.items.len();
                self.items.retain(|item| item.workspace_id != workspace_id);
                self.items.len() != before
            }
            WorkspaceFollowFrame::Order { workspace_ids } => {
                // 只按已知 id 重排；注册表里没有的 id 忽略。
                let mut ordered: Vec<WorkspaceView> = Vec::with_capacity(self.items.len());
                for id in workspace_ids {
                    if let Some(position) =
                        self.items.iter().position(|item| item.workspace_id == id)
                    {
                        ordered.push(self.items.remove(position));
                    }
                }
                ordered.append(&mut self.items);
                let changed = self.items != ordered;
                self.items = ordered;
                changed
            }
            WorkspaceFollowFrame::Archived {
                archived_session_ids,
            } => {
                let changed = self.archived_session_ids != archived_session_ids;
                self.archived_session_ids = archived_session_ids;
                changed
            }
            WorkspaceFollowFrame::Pinned { pinned_session_ids } => {
                let changed = self.pinned_session_ids != pinned_session_ids;
                self.pinned_session_ids = pinned_session_ids;
                changed
            }
        }
    }

    /// 这个会话是不是已归档。
    pub fn is_archived(&self, session_id: &str) -> bool {
        self.archived_session_ids.iter().any(|id| id == session_id)
    }
}
