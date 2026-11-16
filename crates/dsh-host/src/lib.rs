//! dsh host 进程的生命周期与传输。
//!
//! 职责边界见 `docs/DESIGN.md` 的「host 生命周期」：
//!
//! - 运行时来源：自带运行时优先，环境变量逃生口才用 PATH（DECISIONS #18）
//! - 进程模型：host 与 app 同生共死，谁 spawn 谁收尸（DECISIONS #39 / #41）
//! - 握手：走 `/connect` 拿 v2 cookie，每次启动独立 peer（PROTOCOL「两条交换路径」）
//! - 传输：一元调用 `POST /api/<ns>/<method>`，流走 WebSocket `/api/remote.mux`
//!
//! 本 crate 不依赖 GPUI，可以被 headless 探针（不建窗口）单独驱动。

pub mod connection;
pub mod mux;
pub mod stream;

pub use connection::{Connection, HostError, Interactions};
pub use mux::Mux;
pub use stream::FollowStream;

/// 自带运行时在 app bundle 内的相对布局，见 `docs/DESIGN.md`。
pub const RUNTIME_DIR: &str = "Resources/runtime";

/// 数据版本护栏：`$DSH_HOME/storages/*.json` 的 version 高于支持表时必须拒绝启动。
pub const STORAGE_ROOT: &str = "storages";
