//! dsh wire protocol：事件信封、投影、表层折叠。
//!
//! 唯一参照是 `docs/MODEL.md` 与 `docs/PROTOCOL.md`。本 crate 只描述数据形状与
//! 纯函数式的 fold，**不包含任何 I/O、网络或进程代码** —— 那些属于 `dsh-host`。
//!
//! 三条开放轴（事件类型、投影键、内容块）都必须是 merge-extensible 的：
//! 未知成员不得导致整体反序列化失败，见 DECISIONS #8。
//!
//! 所有结构体忽略未知字段（serde 默认），未知的枚举成员降级为 `Unknown`。

pub mod catalog;
pub mod commands;
pub mod context;
pub mod files;
pub mod fold;
pub mod models;
pub mod session;
pub mod settings;
pub mod stream;
pub mod timeline;
pub mod transcript;
pub mod wire;
pub mod workspace;

pub use catalog::{ModelCatalog, PermissionCatalog};
pub use fold::{Apply, AssistantStreamFold, Block, FoldOutcome};
pub use session::{SessionListValue, SessionSummary};
pub use settings::{SettingsDescribeValue, SettingsNamespaceView};
pub use stream::{AssistantStreamFrame, FollowValue, StreamChunk, StreamItem, TokenUsage};
pub use timeline::{
    ChatTimeline, RenderId, TimelineRow, ToolDetail, ToolFamily, ToolResultView, ToolView,
};
pub use transcript::{Entry, MessageId, ToolCall, ToolStatus, Transcript};
pub use wire::{ClientRequest, Payload, RpcError, RpcResult, ServerResponse};
