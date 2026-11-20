//! 侧栏的本地快照：让 host 还没起来（或还在休眠）时界面也能画出历史。
//!
//! 只缓存「侧栏要看的东西」——工作区注册表、会话摘要、归档/置顶集合。
//! 不缓存时间线内容：那是 `session/follow` 的事，宁可空也不给人看假历史。
//!
//! 位置：`$HOME/Library/Application Support/cowork/state.json`（原子替换写）。

use dsh_client_ui::{SelectedModel, SessionInfo, SidebarPrefs};
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
    /// 侧栏私有偏好：隐藏的工作区与历史归属覆盖。
    #[serde(default)]
    pub prefs: CachedPrefs,
}

/// `SidebarPrefs` 的落盘形状，字段用数组表达（JSON 对象键不稳定）。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedPrefs {
    #[serde(default)]
    pub hidden_workspace_ids: Vec<String>,
    #[serde(default)]
    pub adopted: Vec<CachedAdoption>,
}

/// 一条「历史会话归到某工作区」的覆盖。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedAdoption {
    pub workspace_id: String,
    #[serde(default)]
    pub session_ids: Vec<String>,
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
    /// host 账本里的会话 id；先前版本没有这个字段，解析时按空处理。
    #[serde(default)]
    pub session_ids: Vec<String>,
}

impl Snapshot {
    /// 从当前会话列表与注册表拍一张。
    pub fn capture(
        sessions: &[SessionInfo],
        registry: &WorkspaceRegistry,
        prefs: &SidebarPrefs,
    ) -> Self {
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
                    session_ids: workspace.session_ids.clone(),
                })
                .collect(),
            archived: registry.archived_session_ids.clone(),
            pinned: registry.pinned_session_ids.clone(),
            prefs: CachedPrefs {
                hidden_workspace_ids: prefs.hidden_workspace_ids.iter().cloned().collect(),
                adopted: prefs
                    .adopted
                    .iter()
                    .map(|(workspace_id, session_ids)| CachedAdoption {
                        workspace_id: workspace_id.clone(),
                        session_ids: session_ids.clone(),
                    })
                    .collect(),
            },
        }
    }

    /// 还原侧栏私有偏好。旧快照缺字段时解析为默认，不崩溃。
    pub fn prefs(&self) -> SidebarPrefs {
        SidebarPrefs {
            hidden_workspace_ids: self.prefs.hidden_workspace_ids.iter().cloned().collect(),
            adopted: self
                .prefs
                .adopted
                .iter()
                .map(|item| (item.workspace_id.clone(), item.session_ids.clone()))
                .collect(),
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
                    session_ids: workspace.session_ids.clone(),
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
              "workspaces": [{"workspaceId":"w1","path":"/tmp/ws","title":"ws","sessionIds":["s1"]}],
              "archived": ["s2"],
              "pinned": [],
              "prefs": {
                "hiddenWorkspaceIds": ["w2"],
                "adopted": [{"workspaceId":"w1","sessionIds":["s3"]}]
              }
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
        assert_eq!(registry.items[0].session_ids, vec!["s1".to_owned()]);
        assert_eq!(registry.archived_session_ids, vec!["s2".to_owned()]);

        let prefs = snapshot.prefs();
        assert!(prefs.hidden_workspace_ids.contains("w2"));
        assert_eq!(
            prefs.adopted.get("w1").map(Vec::as_slice),
            Some(["s3".to_owned()].as_slice())
        );
    }

    #[test]
    fn empty_file_is_a_usable_snapshot() {
        let snapshot: Snapshot = serde_json::from_str("{}").expect("空对象应可解析");
        assert!(snapshot.sessions().is_empty());
        assert!(snapshot.registry().items.is_empty());
    }
}
