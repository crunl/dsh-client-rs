//! Connection RPC 信封。
//!
//! 线上形状见 `docs/PROTOCOL.md` 的「Wire 封装（实测）」：
//!
//! ```text
//! POST /api/<namespace>/<method>
//! {"type":"client-request","rpcId":"…","method":"<ns>/<method>","payload":{"args":{…}}}
//! {"type":"server-response","rpcId":"…","result":{"ok":true,"value":…}}
//! {"type":"server-response","rpcId":"…","result":{"ok":false,"error":{"code":"…","message":"…","details":{}}}}
//! ```

use serde::{Deserialize, Serialize};

/// 调用方生成、响应原样回显的关联 id。
pub type RpcId = String;

/// 一元方法参数。**必须恰好是一个 `args` 对象**，否则 host 回
/// `gateway/internal: Remote payload must contain exactly one plain-object args field`。
///
/// `args` 的键是 TypeScript 形参名（例如 `session/list` 是 `_request`），
/// 不是位置参数。
#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    /// 形参名 → 值。
    pub args: serde_json::Value,
}

impl Payload {
    /// 用给定的形参名与值构造参数。
    pub fn new(args: serde_json::Value) -> Self {
        Self { args }
    }
}

/// `type` 判别符：客户端请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ClientRequestKind {
    /// 固定值 `client-request`。
    #[serde(rename = "client-request")]
    ClientRequest,
}

/// `type` 判别符：host 响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ServerResponseKind {
    /// 固定值 `server-response`。
    #[serde(rename = "server-response")]
    ServerResponse,
}

/// 一次一元调用的请求体。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRequest {
    /// 固定 `client-request`。
    #[serde(rename = "type")]
    pub kind: ClientRequestKind,
    /// 由调用方生成。
    pub rpc_id: RpcId,
    /// `<namespace>/<method>`，与 URL 路径末段一致。
    pub method: String,
    /// 参数封装。
    pub payload: Payload,
}

impl ClientRequest {
    /// 为指定端点构造请求。
    pub fn new(
        rpc_id: impl Into<RpcId>,
        method: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        Self {
            kind: ClientRequestKind::ClientRequest,
            rpc_id: rpc_id.into(),
            method: method.into(),
            payload: Payload::new(args),
        }
    }
}

/// host 返回的失败。
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    /// 稳定的 `<domain>/<reason>` 码，例如 `gateway/arguments-invalid`。
    /// 调用方按它判别，而不是按 message 文本。
    pub code: String,
    /// 人类可读描述，不保证稳定，不得用于分支。
    pub message: String,
    /// 按 code 定型的附加信息；形状随 code 变化。
    #[serde(default)]
    pub details: serde_json::Value,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// `result` 字段：成功带 `value`，失败带 `error`。
///
/// 不用 serde 的 tagged enum：`ok` 是布尔判别符，做成结构体更稳，
/// 也避免 `value: null` 与「字段缺失」被混为一谈。
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResult<T> {
    /// 成功为 `true`。
    pub ok: bool,
    /// 成功时的业务值。
    #[serde(default = "Option::default")]
    pub value: Option<T>,
    /// 失败时的错误。
    #[serde(default = "Option::default")]
    pub error: Option<RpcError>,
}

impl<T> RpcResult<T> {
    /// 转成 `Result`。`ok: true` 但缺 `value` 视为协议违例。
    pub fn into_result(self) -> Result<T, RpcResultError> {
        match (self.ok, self.value, self.error) {
            (true, Some(value), _) => Ok(value),
            (true, None, _) => Err(RpcResultError::MissingValue),
            (false, _, Some(error)) => Err(RpcResultError::Remote(error)),
            (false, _, None) => Err(RpcResultError::MissingError),
        }
    }
}

/// [`RpcResult::into_result`] 的失败。
#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcResultError {
    /// `ok: true` 却没有 `value`。
    #[error("host 报成功但缺少 value 字段")]
    MissingValue,
    /// `ok: false` 却没有 `error`。
    #[error("host 报失败但缺少 error 字段")]
    MissingError,
    /// host 明确返回的业务失败。
    #[error(transparent)]
    Remote(RpcError),
}

/// 一次一元调用的响应体。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerResponse<T> {
    /// 固定 `server-response`。
    #[serde(rename = "type")]
    pub kind: ServerResponseKind,
    /// 应当与请求的 `rpcId` 相同；调用方必须校验。
    pub rpc_id: RpcId,
    /// 业务结果。
    pub result: RpcResult<T>,
}
