//! 模型设置页的 host 适配层。
//!
//! 只通过 Remote 读写 dsh profile，不碰 `cordis.patch.yml` 路径：
//! - `llm/listConfigurableProviders` / `credentials/describe` 读可配置路由与凭据状态；
//! - `settings/mutate` 把 provider/model profile 写进 `llm-pi-ai.providers`；
//! - `credentials.set|unset` 单独管理 API key 真值（只写引用名到 profile）；
//! - `llm/discoverModels` 探活网关的 `/models`。
//!
//! 写操作带 `expectedRevision`（乐观锁），冲突由 host 拒绝而不是静默覆盖。

use dsh_host::Connection;
use dsh_protocol::models::{
    ConfigurableProvider, CredentialInfo, DiscoveredModel, ProviderModel, ProviderProfile,
    SettingsPathOp,
};
use dsh_protocol::settings::{SettingsDescribeValue, SettingsNamespaceView};
use serde_json::{Value, json};

/// `llm-pi-ai` namespace key（所有自定义 provider 都写进这里）。
pub const LLM_PI_AI_NS: &str = "llm-pi-ai";

/// 一次模型设置的读取快照。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSettingsSnapshot {
    /// 可配置路由目录（含已声明的自定义路由）。
    pub providers: Vec<ConfigurableProvider>,
    /// profile 里已写入的自定义路由 profile，key 为 route。
    pub configured: std::collections::HashMap<String, ProviderProfile>,
    /// 每个路由的凭据状态，key 为 provider id。
    pub credentials: std::collections::HashMap<String, CredentialInfo>,
    /// `llm-pi-ai` 的当前 revision，写入时带上。
    pub revision: u64,
    /// profile 是否可写。
    pub writable: bool,
}

/// 从 namespace user 段的 `providers` 里解析已写入的自定义路由。
/// 形状来自实测：`{"providers": {"<route>": <ProviderProfile>}}`。
pub fn parse_configured(
    user: Option<&Value>,
) -> std::collections::HashMap<String, ProviderProfile> {
    let Some(user) = user else {
        return std::collections::HashMap::new();
    };
    let Some(providers) = user.get("providers").and_then(Value::as_object) else {
        return std::collections::HashMap::new();
    };
    providers
        .iter()
        .filter_map(|(route, value)| {
            serde_json::from_value::<ProviderProfile>(value.clone())
                .ok()
                .map(|profile| (route.clone(), profile))
        })
        .collect()
}

/// 给一个 provider profile 追加模型（按 id 去重），返回新 profile。
/// 纯函数：不改入参，便于单测去重与空值处理。
pub fn append_models(profile: &ProviderProfile, model_ids: &str) -> ProviderProfile {
    let mut next = profile.clone();
    let mut existing: std::collections::HashSet<String> =
        next.models.iter().map(|model| model.id.clone()).collect();
    for raw in model_ids.split(',') {
        let id = raw.trim();
        if id.is_empty() || existing.contains(id) {
            continue;
        }
        existing.insert(id.to_owned());
        next.models.push(ProviderModel {
            id: id.to_owned(),
            ..ProviderModel::default()
        });
    }
    next
}

/// 凭据引用名：大写 id、非字母数字段折叠为 `_`（与官方 `deriveKeyRef` 一致）。
/// POSIX shell 变量名不能以数字开头，所以数字开头的 id 补一个前导下划线。
pub fn credential_ref(provider: &str) -> String {
    let mut out: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    // 折叠连续下划线，去掉首尾。
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        return "_".to_owned();
    }
    if trimmed.as_bytes()[0].is_ascii_digit() {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// 读模型设置快照：目录 + 凭据状态 + revision。
pub async fn describe(conn: &Connection) -> Result<ModelSettingsSnapshot, String> {
    let described: SettingsDescribeValue = conn
        .call("settings/describe", json!({}))
        .await
        .map_err(|e| format!("读 host 设置失败: {e}"))?;
    let providers: Vec<ConfigurableProvider> = conn
        .call("llm/listConfigurableProviders", json!({}))
        .await
        .map_err(|e| format!("读可配置 provider 失败: {e}"))?;

    let namespace = described
        .namespaces
        .into_iter()
        .find(|item| item.ns == LLM_PI_AI_NS)
        .ok_or_else(|| format!("host 没有 {LLM_PI_AI_NS} 设置项"))?;

    let configured = parse_configured(namespace.user.as_ref());

    // 凭据状态查「目录里的路由」+「profile 里已声明但目录里没有的自定义路由」的并集，
    // 否则通过本页加 key 的网关刷新后会永远显示「无凭据」。
    let mut route_refs: Vec<(String, String)> = providers
        .iter()
        .map(|p| (p.provider.clone(), credential_ref(&p.provider)))
        .collect();
    for route in configured.keys() {
        if !route_refs.iter().any(|(existing, _)| existing == route) {
            route_refs.push((route.clone(), credential_ref(route)));
        }
    }
    let refs: Vec<String> = route_refs
        .iter()
        .map(|(_, reference)| reference.clone())
        .collect();
    let credential_values: Value = conn
        .call("credentials/describe", json!({ "refs": refs }))
        .await
        .map_err(|e| format!("读凭据状态失败: {e}"))?;
    let mut credentials = std::collections::HashMap::new();
    for (route, reference) in &route_refs {
        if let Ok(info) =
            serde_json::from_value::<CredentialInfo>(credential_values[reference].clone())
        {
            credentials.insert(route.clone(), info);
        }
    }

    Ok(ModelSettingsSnapshot {
        configured,
        providers,
        credentials,
        revision: namespace.revision,
        writable: described.writable,
    })
}

/// 把一个 provider profile 写进 `llm-pi-ai.providers.<route>`。
pub async fn write_provider(
    conn: &Connection,
    route: &str,
    profile: &ProviderProfile,
    revision: u64,
) -> Result<(), String> {
    let value = serde_json::to_value(profile).map_err(|e| format!("序列化 provider 失败: {e}"))?;
    let view: SettingsNamespaceView = conn
        .call(
            "settings/mutate",
            json!({
                "ns": LLM_PI_AI_NS,
                "ops": [SettingsPathOp::set(&["providers", route], value)],
                "expectedRevision": revision,
            }),
        )
        .await
        .map_err(|e| format!("写 provider 失败: {e}"))?;
    if view.ns != LLM_PI_AI_NS {
        return Err(format!("host 返回了错误的 namespace: {}", view.ns));
    }
    Ok(())
}

/// 给一个已声明的 provider 追加模型：读当前 profile → 追加 → 带 revision 写回。
pub async fn add_model(conn: &Connection, route: &str, model_ids: &str) -> Result<(), String> {
    let snapshot = describe(conn).await?;
    let profile = snapshot
        .configured
        .get(route)
        .ok_or_else(|| format!("provider {route} 尚未声明"))?;
    let next = append_models(profile, model_ids);
    write_provider(conn, route, &next, snapshot.revision).await
}

/// 删除一个 provider（只删 profile 声明，凭据另由调用方决定是否删）。
pub async fn remove_provider(conn: &Connection, route: &str, revision: u64) -> Result<(), String> {
    let view: SettingsNamespaceView = conn
        .call(
            "settings/mutate",
            json!({
                "ns": LLM_PI_AI_NS,
                "ops": [SettingsPathOp::unset(&["providers", route])],
                "expectedRevision": revision,
            }),
        )
        .await
        .map_err(|e| format!("删除 provider 失败: {e}"))?;
    if view.ns != LLM_PI_AI_NS {
        return Err(format!("host 返回了错误的 namespace: {}", view.ns));
    }
    Ok(())
}

/// 存 API key 真值（写 `.credentials.yaml`，只留引用名在 profile 里）。
pub async fn store_credential(
    conn: &Connection,
    provider: &str,
    value: &str,
) -> Result<(), String> {
    // set/unset 返回 void，用 () 接收，不能反序列化成 Value。
    conn.call::<()>(
        "credentials/set",
        json!({ "ref": credential_ref(provider), "value": value }),
    )
    .await
    .map_err(|e| format!("存凭据失败: {e}"))
}

/// 删除 API key 真值。
pub async fn remove_credential(conn: &Connection, provider: &str) -> Result<(), String> {
    conn.call::<()>(
        "credentials/unset",
        json!({ "ref": credential_ref(provider) }),
    )
    .await
    .map_err(|e| format!("删凭据失败: {e}"))
}

/// 组装一次发现的 request：dsh 字段是 `baseURL` / `api` / `apiKey`（一次性真值，
/// 不落盘）；不存在 `apiKeyEnv` 字段。纯函数，便于单测形状。
fn discovery_request(base_url: &str, api: &str, api_key: &str) -> Value {
    let mut request = json!({ "baseURL": base_url });
    if !api.is_empty() {
        request["api"] = json!(api);
    }
    if !api_key.is_empty() {
        request["apiKey"] = json!(api_key);
    }
    request
}

/// 探活网关，拿它声明的模型候选。
pub async fn discover_models(
    conn: &Connection,
    settings_ns: &str,
    base_url: &str,
    api: &str,
    api_key: &str,
) -> Result<Vec<DiscoveredModel>, String> {
    // `call` 的键必须是 TS 形参名；实参名是 `settingsNs`（不是 `ns`）。
    conn.call(
        "llm/discoverModels",
        json!({
            "settingsNs": settings_ns,
            "request": discovery_request(base_url, api, api_key),
        }),
    )
    .await
    .map_err(|e| format!("探测模型失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_configured_reads_written_profiles() {
        let user = json!({
            "providers": {
                "acme": {"api": "openai-completions", "baseURL": "https://x/v1",
                         "apiKeyEnv": "ACME_KEY",
                         "models": [{"id": "m1", "name": "M1", "reasoningEfforts": false}]},
                "broken": {"api": 123}
            }
        });
        let configured = parse_configured(Some(&user));
        assert_eq!(configured.len(), 1, "形状不符的条目被跳过");
        let acme = &configured["acme"];
        assert_eq!(acme.api, "openai-completions");
        assert_eq!(acme.base_url.as_deref(), Some("https://x/v1"));
        assert_eq!(acme.models[0].id, "m1");
    }

    #[test]
    fn parse_configured_handles_missing_user() {
        assert!(parse_configured(None).is_empty());
        assert!(parse_configured(Some(&json!({}))).is_empty());
        assert!(parse_configured(Some(&json!({"providers": {}}))).is_empty());
    }

    #[test]
    fn append_models_dedups_and_trims() {
        let profile = ProviderProfile {
            api: "openai-completions".to_owned(),
            base_url: None,
            api_key_env: None,
            display_name: None,
            models: vec![ProviderModel {
                id: "m1".to_owned(),
                ..ProviderModel::default()
            }],
        };
        let next = append_models(&profile, " m2 , m1 , , m3 , m2 ");
        let ids: Vec<&str> = next.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["m1", "m2", "m3"]);
        // 入参未被改。
        assert_eq!(profile.models.len(), 1);
    }

    #[test]
    fn append_models_all_blank_is_noop() {
        let profile = ProviderProfile {
            api: "x".to_owned(),
            base_url: None,
            api_key_env: None,
            display_name: None,
            models: vec![],
        };
        assert!(append_models(&profile, " , , ").models.is_empty());
    }

    #[test]
    fn credential_ref_matches_official_derivation() {
        assert_eq!(credential_ref("openai"), "OPENAI");
        assert_eq!(credential_ref("my-gateway"), "MY_GATEWAY");
        assert_eq!(credential_ref("acme.gw/v1"), "ACME_GW_V1");
        assert_eq!(credential_ref("123route"), "_123ROUTE");
    }

    #[test]
    fn credential_ref_folds_runs_and_edges() {
        assert_eq!(credential_ref("a--b___c"), "A_B_C");
        assert_eq!(credential_ref("---"), "_");
    }

    #[test]
    fn discovery_request_uses_wire_field_names() {
        // dsh 的 LlmModelDiscoveryRequest 只有 baseURL / api / apiKey / provider，
        // 没有 apiKeyEnv；一次性 key 真值走 apiKey，harness 不落盘。
        let request = discovery_request("https://x/v1", "openai-completions", "sk-x");
        assert_eq!(request["baseURL"], "https://x/v1");
        assert_eq!(request["api"], "openai-completions");
        assert_eq!(request["apiKey"], "sk-x");
        assert!(request.get("apiKeyEnv").is_none());

        let minimal = discovery_request("https://x/v1", "", "");
        assert_eq!(minimal, json!({ "baseURL": "https://x/v1" }));
    }
}
