//! 单 socket 多路复用：一条 WebSocket 上开多条逻辑流。
//!
//! Gateway 的规则是**一个 Peer 只拥有一个 socket**（`admitConnection` 里
//! `record.ownsSocket` 为真时第二次 upgrade 直接 409 Conflict）。所以「每条流开一个
//! socket」的写法只能活一条流：`session/follow` 占了 socket，`workspace/follow`
//! 就连不上。
//!
//! 这里把 socket 收归一个后台任务：上行发 `open` / `cancel`，下行按 `streamId`
//! 把帧分给各自的 channel。副作用是更省内存——整窗口只有一条 WS。

use crate::connection::{Connection, HostError};
use dsh_protocol::Payload;
use dsh_protocol::stream::{StreamItem, StreamItemKind};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::COOKIE;
use tokio_tungstenite::{MaybeTlsStream, connect_async};

/// 一条逻辑流的读端。
pub struct Subscription {
    id: String,
    rx: UnboundedReceiver<StreamItem>,
}

impl Subscription {
    /// 流 id。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 下一条帧；流结束（或 socket 断开）返回 `None`。
    pub async fn next_item(&mut self) -> Option<StreamItem> {
        self.rx.recv().await
    }
}

/// 提供给各调用方的多路复用器：只持有发往后台任务的命令通道。
pub struct Mux {
    commands: UnboundedSender<Command>,
}

impl std::fmt::Debug for Mux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mux")
    }
}

enum Command {
    Open {
        stream_id: String,
        endpoint: String,
        args: Value,
        out: UnboundedSender<StreamItem>,
    },
}

impl Mux {
    /// 打开（或复用）这条连接上的唯一 socket。
    pub async fn of(conn: &Connection) -> Result<Arc<Self>, HostError> {
        if let Some(mux) = conn.mux().get() {
            return Ok(Arc::clone(mux));
        }
        let socket = match handshake(conn).await {
            Err(error) if Connection::is_unauthorized(&error) => {
                conn.refresh().await?;
                handshake(conn).await?
            }
            other => other?,
        };
        // 竞态下可能已经被别人塞进去了；用先到的那个，后建的任务自己退出。
        let built = conn
            .mux()
            .get_or_init(|| async { Arc::new(Self::start(socket)) })
            .await;
        Ok(Arc::clone(built))
    }

    fn start(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        let (commands, mut inbox) = unbounded_channel();
        tokio::spawn(async move {
            let mut socket = socket;
            let mut streams: HashMap<String, UnboundedSender<StreamItem>> = HashMap::new();
            loop {
                tokio::select! {
                    command = inbox.recv() => {
                        let Some(command) = command else { break };
                        match command {
                            Command::Open { stream_id, endpoint, args, out } => {
                                let open = serde_json::json!({
                                    "type": "open",
                                    "streamId": stream_id,
                                    "endpoint": endpoint,
                                    "payload": Payload::new(args),
                                });
                                if socket
                                    .send(Message::Text(open.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                streams.insert(stream_id, out);
                            }
                        }
                    }
                    message = socket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                let Ok(item) = serde_json::from_str::<StreamItem>(&text) else {
                                    continue;
                                };
                                let finished = item.kind == StreamItemKind::End;
                                let stream_id = item.stream_id.clone();
                                let delivered = streams
                                    .get(&stream_id)
                                    .is_some_and(|out| out.send(item).is_ok());
                                if !delivered {
                                    // 读端被丢掉（切会话时 abort 掉 reader）：告诉 host 收摊。
                                    streams.remove(&stream_id);
                                    let cancel = serde_json::json!({
                                        "type": "cancel",
                                        "streamId": stream_id,
                                    });
                                    let _ = socket
                                        .send(Message::Text(cancel.to_string().into()))
                                        .await;
                                } else if finished {
                                    streams.remove(&stream_id);
                                }
                            }
                            Some(Ok(_)) => {}
                            Some(Err(_)) | None => break,
                        }
                    }
                }
            }
            // socket 没了：让所有读端看到 None（channel 随 drop 关闭）。
            streams.clear();
        });
        Self { commands }
    }

    /// 开一条逻辑流。`args` 的键必须是 TypeScript 形参名。
    pub fn subscribe(&self, endpoint: &str, args: Value) -> Result<Subscription, HostError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (out, rx) = unbounded_channel();
        self.commands
            .send(Command::Open {
                stream_id: id.clone(),
                endpoint: endpoint.to_owned(),
                args,
                out,
            })
            .map_err(|_| {
                HostError::WebSocket(tokio_tungstenite::tungstenite::Error::ConnectionClosed)
            })?;
        Ok(Subscription { id, rx })
    }
}

async fn handshake(
    conn: &Connection,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, HostError> {
    let mut request = conn.ws_url().into_client_request()?;
    let cookie = HeaderValue::from_str(&conn.cookie()).map_err(|_| HostError::InvalidCookie)?;
    request.headers_mut().insert(COOKIE, cookie);

    match connect_async(request).await {
        Ok((socket, _response)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 401 => {
            Err(HostError::Unauthorized)
        }
        Err(error) => Err(HostError::WebSocket(error)),
    }
}
