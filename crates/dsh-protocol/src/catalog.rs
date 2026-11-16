//! 目录形状：模型目录与权限 preset 目录。
//!
//! 两处都在 `docs/MODEL.md` 层 6 与 `docs/PROTOCOL.md` 里有实测样本，
//! fixture 见 `crates/dsh-protocol/fixtures/`。

use serde::Deserialize;

/// `session/modelCatalog` 的返回值。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    /// 未配置会话时的部署默认。
    pub default: ModelSelection,
    /// 可路由的 provider id。
    #[serde(default)]
    pub routable_providers: Vec<String>,
    /// 按 provider 分组的模型。
    #[serde(default)]
    pub groups: Vec<ModelProviderGroup>,
    /// provider 级故障。**必须渲染**，否则「有凭据但列举失败」会表现成「什么都没配」。
    /// 形状随 provider 变化，暂不细化。
    #[serde(default)]
    pub failures: Vec<serde_json::Value>,
}

impl ModelCatalog {
    /// 目录是否为空（没有任何可选模型）。
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.models.is_empty())
    }
}

/// 一个模型选择。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    /// provider id。
    pub provider: String,
    /// 该 provider 下的模型 id。
    pub model: String,
    /// 推理强度 id；目录默认值里没有这个键，会话投影的 `next` 有。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// 按 provider 分组。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelProviderGroup {
    /// provider id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 该组下的模型。
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// 目录里的一个模型。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    /// 模型 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 说明，可能缺失。
    #[serde(default)]
    pub description: Option<String>,
    /// 推理档位，不支持推理的模型没有这个键。
    #[serde(default)]
    pub reasoning: Option<ModelReasoning>,
    /// 徽标，可能缺失。
    #[serde(default)]
    pub badge: Option<String>,
}

/// 一个模型的推理档位。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoning {
    /// 可选档位。
    #[serde(default)]
    pub efforts: Vec<EffortInfo>,
    /// 默认档位。
    #[serde(default)]
    pub default_effort: Option<String>,
}

/// 一个推理档位。
#[derive(Debug, Clone, Deserialize)]
pub struct EffortInfo {
    /// 档位 id，**不透明**，必须精确匹配，不得自行降级或合成 `off`。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 说明。
    #[serde(default)]
    pub description: Option<String>,
}

/// `permissionPresets/catalog` 的返回值。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionCatalog {
    /// 全部可选 preset。
    #[serde(default)]
    pub options: Vec<PresetOption>,
    /// 可作为新会话默认的 preset。
    #[serde(default)]
    pub default_options: Vec<PresetOption>,
    /// 出厂默认 preset 的 key。
    #[serde(default)]
    pub default_preset: Option<String>,
}

/// 目录里的一个权限 preset。
#[derive(Debug, Clone, Deserialize)]
pub struct PresetOption {
    /// preset key，写回时用它。
    pub value: String,
    /// 显示名。
    pub name: String,
    /// 说明；**实测这个构建里没有该字段**，不能假定存在。
    #[serde(default)]
    pub description: Option<String>,
}
