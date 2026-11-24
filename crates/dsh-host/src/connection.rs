//! 与 host 的一次已认证连接。
//!
//! 只有两件事：把 ready URL 换成 cookie（`/connect`），以及在该 cookie 下调用
//! 一元端点。进程的 spawn 属于生命周期层，不在这里。

use dsh_protocol::wire::{ClientRequest, RpcResultError, ServerResponse};
use reqwest::header::{self, HeaderMap, SET_COOKIE};
use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mux::Mux;

/// TCP 建连上限。host 在本机，超过这个时间基本就是卡死了。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次 HTTP 请求（含读完响应体）的上限。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// WebSocket 升级握手的上限。
pub(crate) const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 成功响应体的字节上限。超过就报错，不把整个响应攒进内存。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 错误响应体只读这么多，用于报错文案。
const MAX_ERROR_BODY_BYTES: usize = 512;

/// `interactions` 声明：决定这个 peer 是否参与转发的人类交互。
///
/// 审批请求与 user question 都走这条通道，**默认是 `Silent`，即收不到审批**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interactions {
    /// `answer`：可以接收并回应审批、用户提问。
    Answer,
    /// `none`：只发请求，不接收交互（host 默认）。
    Silent,
}

impl Interactions {
    /// 线上取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Silent => "none",
        }
    }
}

/// 连接失败或调用失败。
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// ready URL 里没有 `token` 查询参数。
    #[error("ready URL 缺少 token 查询参数")]
    MissingToken,
    /// 认证失效（401）：cookie 被 host 拒绝，且重新签发也没用。
    ///
    /// 最常见的原因是 host 进程重启过（token 与密钥都换了）。
    #[error("认证失效（401）：host 可能已重启")]
    Unauthorized,
    /// `/connect` 没有返回 302。
    #[error("/connect 返回 {status}，期望 302")]
    Handshake {
        /// 实际状态码。
        status: u16,
    },
    /// 302 里没有 `Set-Cookie`。
    #[error("/connect 响应里没有可用的 Set-Cookie")]
    MissingCookie,
    /// URL 拼装失败。
    #[error("URL 无效: {0}")]
    Url(#[from] url::ParseError),
    /// 网络层失败。URL 已用 `without_url()` 剥掉：`/connect` 的 URL 带 token，
    /// 不得随 Display/Debug 进日志或 UI。
    #[error("HTTP 失败: {0}")]
    Http(#[from] reqwest::Error),
    /// host 返回了非 2xx。
    #[error("host 返回 {status}: {body}")]
    Status {
        /// 状态码。
        status: u16,
        /// 响应体（截断到 512 字节）。
        body: String,
    },
    /// 响应体超过 [`MAX_BODY_BYTES`]。
    #[error("响应体超过 {limit} 字节上限")]
    BodyTooLarge {
        /// 允许的上限（字节）。
        limit: usize,
    },
    /// 连接或握手超时（host 没有响应）。
    #[error("超时：host 没有响应")]
    Timeout,
    /// 紧随一次握手失败之后：同一批排队的调用直接失败，不重复握手。
    /// 调用方应在稍后重试（host 可能已恢复）。
    #[error("WebSocket 握手刚失败，请稍后重试")]
    RecentHandshakeFailure,
    /// 响应体不是合法的信封。
    #[error("响应无法解析: {source}；原文: {body}")]
    Decode {
        /// serde 的错误。
        #[source]
        source: serde_json::Error,
        /// 原始响应体（截断到 512 字节）。
        body: String,
    },
    /// 回显的 `rpcId` 与请求不符。
    #[error("rpcId 不匹配: 发送 {sent}，收到 {got}")]
    RpcIdMismatch {
        /// 请求里的 id。
        sent: String,
        /// 响应里的 id。
        got: String,
    },
    /// host 明确返回的业务失败。
    #[error(transparent)]
    Result(#[from] RpcResultError),
    /// WebSocket 传输失败。
    #[error("WebSocket 失败: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    /// WebSocket 握手请求构造失败。
    #[error("WebSocket 请求无效: {0}")]
    WebSocketRequest(#[from] tokio_tungstenite::tungstenite::http::Error),
    /// cookie 不能放进 header。
    #[error("cookie 不是合法的 header 值")]
    InvalidCookie,
    /// 流帧无法解析。
    #[error("流帧无法解析: {0}")]
    Frame(#[from] serde_json::Error),
}

/// 一条已认证的 host 连接。
///
/// `Debug` 故意只列非敏感字段：token 与 cookie 都是凭据。
pub struct Connection {
    http: Client,
    base: Url,
    /// 进程内的 launch token → `/connect` → v2 cookie。**不得进日志**。
    token: String,
    /// 首次握手时声明的交互能力。
    interactions: Interactions,
    /// 当前 cookie。host 重启后需要整体替换，所以放在锁里。
    cookie: std::sync::RwLock<String>,
    /// 这条连接当前的流 socket（延迟建立，见 [`crate::mux`]）。
    ///
    /// 用异步锁：建 socket 期间持有它，并发调用者排队等同一个结果；
    /// 存的 mux 若已死（socket 断开），下一次 `Mux::of` 会替换它。
    mux: tokio::sync::Mutex<Option<Arc<Mux>>>,
    /// 最近一次握手失败的时刻：窗口期内让排队的调用者直接失败，不再各自串行重试。
    /// 与 mux 槽位分开存，避免失败态被「存入成功的 mux」覆盖时被误读。
    last_handshake_failure: std::sync::Mutex<Option<Instant>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("origin", &self.base.as_str())
            .field("interactions", &self.interactions)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// 用 ready URL 换 cookie，默认声明 `interactions=answer`（要收审批就必须这样）。
    pub async fn connect(ready_url: &str) -> Result<Self, HostError> {
        Self::connect_with(ready_url, Interactions::Answer).await
    }

    /// 用 ready URL 换 cookie，并显式声明交互能力。
    pub async fn connect_with(
        ready_url: &str,
        interactions: Interactions,
    ) -> Result<Self, HostError> {
        let ready = Url::parse(ready_url)?;
        let token = ready
            .query_pairs()
            .find(|(key, _)| key == "token")
            .map(|(_, value)| value.into_owned())
            .ok_or(HostError::MissingToken)?;

        // base 取 origin；ready URL 的 path/query 都丢掉，后续按需拼接。
        let mut base = ready;
        base.set_query(None);
        base.set_path("/");

        // 不跟随重定向：`/connect` 的 Location 指向 `./api/remote.mux`，
        // 那是给浏览器 Web UI 的落点，对我们没有意义（段内带点，会 404）。
        // 超时：host 接了连接但不回话时，调用方必须能拿到错误而不是永远挂住。
        let http = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;

        let connection = Self {
            http,
            base,
            token,
            interactions,
            cookie: std::sync::RwLock::new(String::new()),
            mux: tokio::sync::Mutex::new(None),
            last_handshake_failure: std::sync::Mutex::new(None),
        };

        let cookie = connection.handshake().await?;
        connection.set_cookie(cookie);
        Ok(connection)
    }

    /// 用同一份凭据重签一次 cookie（401 自愈用）。
    pub async fn refresh(&self) -> Result<(), HostError> {
        let cookie = self.handshake().await?;
        self.set_cookie(cookie);
        Ok(())
    }

    /// 换取一枚新 cookie，不改状态。
    async fn handshake(&self) -> Result<String, HostError> {
        self.exchange_token(&self.token).await
    }

    /// `/connect?token=…&interactions=…` → 302 + `Set-Cookie`（v2 cookie）。
    async fn exchange_token(&self, token: &str) -> Result<String, HostError> {
        // 用 `url` 自己的 query 拼接：reqwest 的 `query()` 需要额外 feature。
        let mut connect_url = self.base.join("connect")?;
        connect_url
            .query_pairs_mut()
            .append_pair("token", token)
            .append_pair("interactions", self.interactions.as_str());

        let response = self
            .http
            .get(connect_url)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;

        match response.status() {
            // 实测 `/connect` 是 302；根路径 `/?token=` 才是 303，两条别混。
            StatusCode::FOUND => first_cookie(response.headers()),
            StatusCode::UNAUTHORIZED => Err(HostError::Unauthorized),
            status => Err(HostError::Handshake {
                status: status.as_u16(),
            }),
        }
    }

    fn set_cookie(&self, cookie: String) {
        // 唯一会写这个锁的地方，中毒也只可能是这里，取回内部值即可。
        *self
            .cookie
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = cookie;
    }

    fn cookie_value(&self) -> String {
        self.cookie
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// 调用一个一元端点。`args` 的键必须是 **TypeScript 形参名**。
    ///
    /// 401 时自动重新握手并重试一次：cookie 失效但 host 还活着的情况自愈；
    /// token 也失效（host 重启过）则直报 [`HostError::Unauthorized`]。
    pub async fn call<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        args: Value,
    ) -> Result<T, HostError> {
        match self.call_once(endpoint, args.clone()).await {
            Err(HostError::Unauthorized) => {
                self.refresh().await?;
                self.call_once(endpoint, args).await
            }
            other => other,
        }
    }

    async fn call_once<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        args: Value,
    ) -> Result<T, HostError> {
        let url = self.base.join(&format!("api/{endpoint}"))?;
        let request = ClientRequest::new(uuid::Uuid::new_v4().to_string(), endpoint, args);

        let response = self
            .http
            .post(url)
            .header(header::COOKIE, self.cookie_value())
            .json(&request)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(HostError::Unauthorized);
        }
        if !status.is_success() {
            // 错误响应只用于报错文案，读到 512 字节就停。
            let (bytes, _truncated) = read_body(response, MAX_ERROR_BODY_BYTES).await?;
            return Err(HostError::Status {
                status: status.as_u16(),
                body: truncate(String::from_utf8_lossy(&bytes).into_owned()),
            });
        }

        // 成功响应：边读边计数，超过上限立即失败，不把超大响应整块攒进内存。
        let (bytes, overflow) = read_body(response, MAX_BODY_BYTES).await?;
        if overflow {
            return Err(HostError::BodyTooLarge {
                limit: MAX_BODY_BYTES,
            });
        }
        // 合法 UTF-8 直接消费 Vec，不额外分配；仅非法分支才走 lossy 重建。
        let body = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        };

        let parsed: ServerResponse<T> =
            serde_json::from_str(&body).map_err(|source| HostError::Decode {
                source,
                body: truncate(body),
            })?;

        if parsed.rpc_id != request.rpc_id {
            return Err(HostError::RpcIdMismatch {
                sent: request.rpc_id,
                got: parsed.rpc_id,
            });
        }

        Ok(parsed.result.into_result()?)
    }

    /// 本次连接使用的 cookie（`name=value`）。**含凭据，不得写日志。**
    pub fn cookie(&self) -> String {
        self.cookie_value()
    }

    /// 这个错误是不是「认证失效」——流层据此决定要不要重新握手。
    pub fn is_unauthorized(error: &HostError) -> bool {
        matches!(error, HostError::Unauthorized)
    }

    /// host 的 origin，例如 `http://127.0.0.1:61664`。
    pub fn origin(&self) -> &str {
        self.base.as_str()
    }

    /// 逻辑流 WebSocket 的地址（固定 `/api/remote.mux`）。
    pub fn ws_url(&self) -> String {
        let scheme = if self.base.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        let host = self.base.host_str().unwrap_or("127.0.0.1");
        let port = self.base.port().unwrap_or(80);
        format!("{scheme}://{host}:{port}/api/remote.mux")
    }

    /// 这条连接的流多路复用器槽位（见 [`crate::mux`]）。
    pub(crate) fn mux(&self) -> &tokio::sync::Mutex<Option<Arc<Mux>>> {
        &self.mux
    }

    /// 记录一次握手失败（当前时刻）。供 mux 的并发调用者 fail-fast 用。
    pub(crate) fn note_handshake_failure(&self) {
        if let Ok(mut guard) = self.last_handshake_failure.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// 握手成功后清除失败记忆，让后续调用立即恢复，不必等窗口过期。
    pub(crate) fn clear_handshake_failure(&self) {
        if let Ok(mut guard) = self.last_handshake_failure.lock() {
            *guard = None;
        }
    }

    /// 上一次握手失败发生在 `window` 之内吗？没有失败记录时为 false。
    pub(crate) fn handshake_failed_recently(&self, window: Duration) -> bool {
        let last = self
            .last_handshake_failure
            .lock()
            .ok()
            .and_then(|guard| *guard);
        failure_within_window(last, window)
    }
}

/// 纯判断：`at` 是否落在 `window` 窗口内（None 视为无失败）。单测直接 pin 时刻，不依赖时钟。
fn failure_within_window(at: Option<Instant>, window: Duration) -> bool {
    at.is_some_and(|at| at.elapsed() < window)
}

/// 纯判断：声明的 body 长度是否已超过上限（None 表示未知，不触发 fail-fast）。
fn advertised_exceeds_limit(advertised: Option<u64>, limit: usize) -> bool {
    advertised.is_some_and(|len| len > limit as u64)
}

/// 最多读 `limit` 字节，返回（已读字节，是否还有剩余被截掉）。
///
/// 逐块读取，超出上限的部分不会进入内存。
async fn read_body(mut response: Response, limit: usize) -> Result<(Vec<u8>, bool), HostError> {
    // 声明长度已超限时立即失败，不读 body；否则按声明长度预分配，避免逐块扩容拷贝。
    // 声明长度不可信（可能缺失或谎报），所以读取循环里仍逐块校验真实字节数。
    let advertised = response.content_length();
    if advertised_exceeds_limit(advertised, limit) {
        return Ok((Vec::new(), true));
    }
    let capacity = advertised.unwrap_or(0) as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(capacity.min(limit));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        let room = limit - buf.len();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            return Ok((buf, true));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((buf, false))
}

fn first_cookie(headers: &HeaderMap) -> Result<String, HostError> {
    for value in headers.get_all(SET_COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        // 只取第一对 name=value，属性（Max-Age/Path/HttpOnly…）不需要。
        let pair = text.split(';').next().unwrap_or("").trim();
        if pair.contains('=') && !pair.starts_with('=') {
            return Ok(pair.to_owned());
        }
    }
    Err(HostError::MissingCookie)
}

fn truncate(mut body: String) -> String {
    const MAX: usize = 512;
    if body.len() > MAX {
        // 按字符边界截断，避免切坏 UTF-8。
        let mut cut = MAX;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push('…');
    }
    body
}

#[cfg(test)]
mod tests {
    use super::{Connection, HostError, advertised_exceeds_limit, failure_within_window};
    use std::time::{Duration, Instant};

    /// 网络层错误不得把 ready URL 的 token 带进 Display/Debug：
    /// reqwest::Error 默认含请求 URL，而 `/connect` 的 URL 带 token。
    #[tokio::test]
    async fn handshake_network_error_does_not_leak_the_token() {
        // port 1 必然 connection refused，走网络层错误分支。
        let error = Connection::connect("http://127.0.0.1:1/?token=leak-canary-41")
            .await
            .expect_err("port 1 必须失败");
        assert!(matches!(error, HostError::Http(_)));
        let text = format!("{error} {error:?}");
        assert!(!text.contains("leak-canary-41"), "token 泄露: {text}");
        assert!(!text.contains("token="), "URL 泄露: {text}");
    }

    #[test]
    fn advertised_length_fail_fast_boundary() {
        assert!(advertised_exceeds_limit(Some(11), 10), "超上限应 fail-fast");
        assert!(advertised_exceeds_limit(Some(100), 10));
        assert!(
            !advertised_exceeds_limit(Some(10), 10),
            "恰好等于上限应放行"
        );
        assert!(!advertised_exceeds_limit(Some(5), 10));
        assert!(
            !advertised_exceeds_limit(None, 10),
            "未知长度交给读取循环校验"
        );
    }

    #[test]
    fn handshake_failure_window_only_fail_fast_recently() {
        let window = Duration::from_secs(10);
        assert!(
            !failure_within_window(None, window),
            "无失败记录不应 fail-fast"
        );
        assert!(
            failure_within_window(Some(Instant::now()), window),
            "刚失败应 fail-fast"
        );
        assert!(
            !failure_within_window(Some(Instant::now() - Duration::from_secs(11)), window),
            "超出窗口应允许重试"
        );
    }
}
