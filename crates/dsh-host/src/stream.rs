//! Gateway 逻辑流的调用面。
//!
//! **一条连接只有一个 socket**（见 [`crate::mux`]）：这里只是给单条逻辑流包一层
//! 顺序读的接口，底层复用同一个多路复用器。多开一条流不再需要多开 socket，
//! 也不会撞上 host 的 409「一个 peer 只准一条 socket」。
//!
//! 线上帧（`docs/PROTOCOL.md`）：
//!
//! - 上行：`{type:'open', streamId, endpoint, payload}`
//! - 下行：`{type:'item', streamId, value}` / `{type:'error', streamId, error}` /
//!   `{type:'end', streamId}`

use crate::connection::{Connection, HostError};
use crate::mux::{Mux, Subscription};
use dsh_protocol::stream::StreamItem;
use serde_json::Value;

/// 一条已打开的逻辑流。
pub struct FollowStream {
    subscription: Subscription,
}

impl FollowStream {
    /// 打开一条逻辑流。`args` 的键必须是 **TypeScript 形参名**。
    ///
    /// 首次调用会顺带把连接的唯一 socket 建起来；401 时先重新握手再一次。
    pub async fn open(conn: &Connection, endpoint: &str, args: Value) -> Result<Self, HostError> {
        let mux = Mux::of(conn).await?;
        Ok(Self {
            subscription: mux.subscribe(endpoint, args)?,
        })
    }

    /// 本流的 id。
    pub fn stream_id(&self) -> &str {
        self.subscription.id()
    }

    /// 读取下一条业务帧；流结束返回 `None`。
    pub async fn next_item(&mut self) -> Result<Option<StreamItem>, HostError> {
        Ok(self.subscription.next_item().await)
    }

    /// 丢掉这条流：读端一关，多路复用器就会给 host 发 `cancel`。
    pub fn close(self) {}
}
