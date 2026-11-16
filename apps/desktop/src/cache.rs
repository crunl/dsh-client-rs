//! 侧栏的本地快照：让 host 还没起来（或还在休眠）时界面也能画出历史。
//!
//! 只缓存「侧栏要看的东西」——工作区注册表、会话摘要、归档/置顶集合。
//! 不缓存时间线内容：那是 `session/follow` 的事，宁可空也不给人看假历史。
//!
//! 位置：`$HOME/Library/Application Support/cowork/state.json`（原子替换写）。

use dsh_client_ui::{SelectedModel, SessionInfo};
use dsh_protocol::workspace::{WorkspaceRegistry, WorkspaceView};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一份可以直接喂给界面的侧栏快照。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// 会话摘要。
    #[serde(default)]
    pub sessions: Vec<CachedSession>,
    /// 工作区。
    #[serde(default)]
    pub workspaces: Vec<CachedWorkspace>,
    /// 已归档会话 id。
    #[serde(default)]
    pub archived: Vec<String>,
    /// 已置顶会话 id。
    #[serde(default)]
    pub pinned: Vec<String>,
}

/// 会话摘要的落盘形状（字段少而稳，不跟协议字段名绑死）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSession {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub blank: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<CachedModel>,
}

/// 模型选择三件套。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedModel {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
}

/// 工作区的落盘形状。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedWorkspace {
    pub workspace_id: String,
    pub path: String,
    #[serde(default)]
    pub title: String,
}

impl Snapshot {
    /// 从当前会话列表与注册表拍一张。
    pub fn capture(sessions: &[SessionInfo], registry: &WorkspaceRegistry) -> Self {
        Self {
            sessions: sessions
                .iter()
                .map(|session| CachedSession {
                    id: session.id.clone(),
                    title: session.title.clone(),
                    cwd: session.cwd.clone(),
                    updated_at: session.updated_at,
                    blank: session.blank,
                    mode: session.mode.clone(),
                    model: session.model.as_ref().map(|selected| CachedModel {
                        provider: selected.provider.clone(),
                        model: selected.model.clone(),
                        effort: selected.effort.clone(),
                    }),
                })
                .collect(),
            workspaces: registry
                .items
                .iter()
                .map(|workspace| CachedWorkspace {
                    workspace_id: workspace.workspace_id.clone(),
                    path: workspace.path.clone(),
                    title: workspace.title.clone(),
                })
                .collect(),
            archived: registry.archived_session_ids.clone(),
            pinned: registry.pinned_session_ids.clone(),
        }
    }

    /// 还原会话列表。`running` 一律归零：快照不承载「现在正在跑吗」，
    /// 否则上次强退会把运行点留成永久绿点。
    pub fn sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|session| SessionInfo {
                id: session.id.clone(),
                title: session.title.clone(),
                cwd: session.cwd.clone(),
                updated_at: session.updated_at,
                running: false,
                blank: session.blank,
                mode: session.mode.clone(),
                model: session.model.as_ref().map(|cached| SelectedModel {
                    provider: cached.provider.clone(),
                    model: cached.model.clone(),
                    effort: cached.effort.clone(),
                }),
            })
            .collect()
    }

    /// 还原工作区注册表。
    pub fn registry(&self) -> WorkspaceRegistry {
        WorkspaceRegistry {
            items: self
                .workspaces
                .iter()
                .map(|workspace| WorkspaceView {
                    workspace_id: workspace.workspace_id.clone(),
                    path: workspace.path.clone(),
                    title: workspace.title.clone(),
                    session_ids: Vec::new(),
                    created_at: None,
                    updated_at: None,
                })
                .collect(),
            archived_session_ids: self.archived.clone(),
            pinned_session_ids: self.pinned.clone(),
        }
    }
}

/// 快照文件路径。拿不到家目录就没有快照（不影响使用）。
fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(
        home.join("Library/Application Support/cowork")
            .join("state.json"),
    )
}

/// 读快照。文件缺失、损坏、版本不符都当作「没有快照」。
pub fn load() -> Option<Snapshot> {
    let path = path()?;
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            tracing::warn!(%error, "本地快照解析失败，忽略");
            None
        }
    }
}

/// 写快照：先写临时文件再 rename，避免半截文件。
pub fn save(snapshot: &Snapshot) {
    let Some(path) = path() else {
        tracing::warn!("拿不到家目录，快照不写");
        return;
    };
    let Some(parent) = path.parent() else { return };
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(%error, "建快照目录失败");
        return;
    }
    let Ok(text) = serde_json::to_string(snapshot) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temp, text) {
        tracing::warn!(%error, "写快照失败");
        return;
    }
    if let Err(error) = std::fs::rename(&temp, &path) {
        tracing::warn!(%error, "替换快照失败");
    }
}

#[cfg(test)]
mod tests {
    use super::Snapshot;

    #[test]
    fn capture_and_restore_round_trips_the_sidebar() {
        let snapshot: Snapshot = serde_json::from_str(
            r#"{
              "sessions": [
                {"id":"s1","title":"旧会话","cwd":"/tmp/ws","updatedAt":123,"blank":false,
                 "mode":"workspace-write",
                 "model":{"provider":"deepseek-official","model":"deepseek-v4-flash","effort":"high"}}
              ],
              "workspaces": [{"workspaceId":"w1","path":"/tmp/ws","title":"ws"}],
              "archived": ["s2"],
              "pinned": []
            }"#,
        )
        .expect("快照应可解析");

        let sessions = snapshot.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("旧会话"));
        // 快照不承载运行态：一律归零，免得留成永久绿点。
        assert!(!sessions[0].running);
        assert_eq!(
            sessions[0].model.as_ref().map(|m| m.provider.as_str()),
            Some("deepseek-official")
        );

        let registry = snapshot.registry();
        assert_eq!(registry.items.len(), 1);
        assert_eq!(registry.items[0].path, "/tmp/ws");
        assert_eq!(registry.archived_session_ids, vec!["s2".to_owned()]);
    }

    #[test]
    fn empty_file_is_a_usable_snapshot() {
        let snapshot: Snapshot = serde_json::from_str("{}").expect("空对象应可解析");
        assert!(snapshot.sessions().is_empty());
        assert!(snapshot.registry().items.is_empty());
    }
}
