//! 模型设置页的 wire 形状：`llm/listConfigurableProviders` / `credentials/describe`
//! / `llm/discoverModels` 的返回，以及 `settings/mutate` 写入 `llm-pi-ai` 的 path op。
//!
//! 形状全部来自实测（见 docs/DECISIONS.md #49 与本轮探针），不是文档字面：
//! 凭据真值永不回显，只有 `configured` / `masked` / `source`；provider 的加入
//! 走 `settings/mutate` 的路径操作，API key 另走 `credentials.set`。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `llm/listConfigurableProviders` 的一项：一个可配置的路由。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurableProvider {
    /// provider 路由 id（也作为 `providers.<id>` 的 settings key）。
    pub provider: String,
    /// 界面显示名。
    pub display_name: String,
    /// 该 provider 的配置写在哪个 settings namespace。
    pub settings_ns: String,
    /// 从 namespace 根到 provider profile 的路径；空表示整段就是 profile。
    #[serde(default)]
    pub settings_path: Vec<String>,
    /// 官方语义：adapter 仅因配置声明才认识该路由（自定义网关/自托管）。
    /// 注意它不是「用户已配置」——内置 provider 配过 key 仍是 false，
    /// UI 的「已配置」由 `declared || profile 里有声明` 共同判断。
    #[serde(default)]
    pub declared: bool,
    /// 该 provider 的默认 endpoint；自定网关时作为占位提示。
    #[serde(rename = "defaultBaseURL", default)]
    pub default_base_url: Option<String>,
    /// base URL 从哪些环境变量取。
    #[serde(rename = "baseURLEnv", default)]
    pub base_url_env: Vec<String>,
}

impl ConfigurableProvider {
    /// 显示名缺失时的回退。
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.provider
        } else {
            &self.display_name
        }
    }
}

/// `credentials/describe` 的一项。真值不进这个结构。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    /// 是否已配置。
    #[serde(default)]
    pub configured: bool,
    /// 是否可写。
    #[serde(default)]
    pub writable: bool,
    /// 打码后的展示串（如 `sk-•••`），不是真值。
    #[serde(default)]
    pub masked: Option<String>,
    /// 真值来源：`env` / `file` 等。
    #[serde(default)]
    pub source: Option<String>,
}

/// `llm/discoverModels` 返回的一个候选模型。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    /// 端点接受的模型 id。
    pub id: String,
    /// 端点给出的显示名，可能缺失。
    #[serde(default)]
    pub name: Option<String>,
    /// 上下文窗口，可能缺失。
    #[serde(default)]
    pub context_window: Option<u64>,
    /// 最大输出，可能缺失。
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

/// `settings/mutate` 的一条路径操作（wire 形状）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPathOp {
    /// `set` 或 `unset`。
    pub op: String,
    /// 从 namespace user 段根出发的路径。
    pub path: Vec<String>,
    /// `set` 时携带的值；`unset` 时不带。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl SettingsPathOp {
    /// 构造一条 `set`。
    pub fn set(path: &[&str], value: Value) -> Self {
        Self {
            op: "set".to_owned(),
            path: path.iter().map(|s| (*s).to_owned()).collect(),
            value: Some(value),
        }
    }

    /// 构造一条 `unset`。
    pub fn unset(path: &[&str]) -> Self {
        Self {
            op: "unset".to_owned(),
            path: path.iter().map(|s| (*s).to_owned()).collect(),
            value: None,
        }
    }
}

/// 写一个自定义 provider 时 `providers.<route>` 的值。
///
/// 字段命名对齐 pi-ai 的 settings schema（实测）。`apiKeyEnv` 只是引用名，
/// 真值另由 `credentials.set` 存到 `.credentials.yaml`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    /// 线协议家族：`openai-completions` / `openai-responses` /
    /// `anthropic-messages` / `google-generative-ai`。
    pub api: String,
    /// 网关地址。dsh 用的是 `baseURL`（大写 URL），camelCase 规则会给成 `baseUrl`，
    /// 必须显式改名，否则读回已写入的 profile 时这个字段永远是 None。
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 凭据引用名（不是密钥真值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// 显示名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 该路由下的模型。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ProviderModel>,
}

/// provider profile 里的一个模型条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    /// 模型 id（请求里发送的值）。
    pub id: String,
    /// 显示名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 上下文窗口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 最大输出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// 推理档位：官方 schema 是 `false | object`，非推理模型写 `false`。
    /// 用 `Value` 透传，UI 层保证形状，客户端不替 host 决策。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Value>,
}

impl Default for ProviderModel {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            context_window: None,
            max_tokens: None,
            reasoning_efforts: Some(Value::Bool(false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_op_set_and_unset_wire_shape() {
        assert_eq!(
            serde_json::to_value(SettingsPathOp::set(
                &["providers", "acme"],
                json!({"api": "x"})
            ))
            .unwrap(),
            json!({"op": "set", "path": ["providers", "acme"], "value": {"api": "x"}})
        );
        assert_eq!(
            serde_json::to_value(SettingsPathOp::unset(&["providers", "acme"])).unwrap(),
            json!({"op": "unset", "path": ["providers", "acme"]})
        );
    }

    #[test]
    fn provider_wire_uses_base_url_capitals() {
        // dsh 用 baseURL / defaultBaseURL / baseURLEnv（大写 URL），
        // serde 默认 camelCase 会给成 baseUrl，读写都会静默错位。
        let provider: ConfigurableProvider = serde_json::from_value(json!({
            "provider": "acme", "displayName": "Acme", "settingsNs": "llm-pi-ai",
            "defaultBaseURL": "https://x/v1", "baseURLEnv": ["X_BASE"]
        }))
        .unwrap();
        assert_eq!(provider.default_base_url.as_deref(), Some("https://x/v1"));
        assert_eq!(provider.base_url_env, ["X_BASE"]);
    }

    #[test]
    fn configurable_provider_missing_optional_fields_default() {
        let provider: ConfigurableProvider = serde_json::from_value(json!({
            "provider": "acme",
            "displayName": "Acme",
            "settingsNs": "llm-pi-ai"
        }))
        .unwrap();
        assert_eq!(provider.provider, "acme");
        assert_eq!(provider.label(), "Acme");
        assert!(provider.settings_path.is_empty());
        assert!(!provider.declared);
        assert!(provider.default_base_url.is_none());
    }

    #[test]
    fn configurable_provider_blank_name_falls_back_to_id() {
        let provider: ConfigurableProvider = serde_json::from_value(json!({
            "provider": "acme-gw", "displayName": "", "settingsNs": "llm-pi-ai"
        }))
        .unwrap();
        assert_eq!(provider.label(), "acme-gw");
    }

    #[test]
    fn credential_info_never_carries_secret() {
        let info: CredentialInfo = serde_json::from_value(json!({
            "configured": true, "masked": "sk-•••", "source": "file", "writable": true
        }))
        .unwrap();
        assert!(info.configured);
        assert!(info.writable);
        assert_eq!(info.masked.as_deref(), Some("sk-•••"));
        assert_eq!(info.source.as_deref(), Some("file"));
    }

    #[test]
    fn discovered_model_optional_fields() {
        let model: DiscoveredModel =
            serde_json::from_value(json!({"id": "m1", "name": "Model 1"})).unwrap();
        assert_eq!(model.id, "m1");
        assert_eq!(model.name.as_deref(), Some("Model 1"));
        assert!(model.context_window.is_none());
        assert!(model.max_tokens.is_none());
    }

    #[test]
    fn provider_profile_skips_none_and_serializes_camel_case() {
        let profile = ProviderProfile {
            api: "openai-completions".to_owned(),
            base_url: Some("https://x/v1".to_owned()),
            api_key_env: Some("ACME_KEY".to_owned()),
            display_name: None,
            models: vec![ProviderModel::default()],
        };
        let value = serde_json::to_value(&profile).unwrap();
        assert_eq!(value["api"], "openai-completions");
        assert_eq!(value["baseURL"], "https://x/v1");
        assert_eq!(value["apiKeyEnv"], "ACME_KEY");
        assert!(value.get("displayName").is_none());
        assert_eq!(value["models"][0]["reasoningEfforts"], json!(false));
    }
}
