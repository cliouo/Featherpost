//! cordis（Rust 版）—— 时空可组合性元框架内核。
//!
//! 这是 [cordis](https://github.com/cordiverse/cordis)（TS）方法论的 Rust 移植，
//! 已同步至上游 `v4.0.0-rc.13`（`bc9fae7`）语义。
//! 设计与形式化映射见仓库根目录 `docs/rust-port-design.md`。

mod context;
mod disposer;
mod events;
mod fiber;
mod plugin;
mod realm;
mod registry;
mod service;

pub use context::{Context, FiberHandle, ProvideHandle};
pub use disposer::{BoxFuture, Disposer};
pub use events::{
    BailEvent, EmitEvent, EventOptions, ParallelEvent, SerialEvent, WaterfallEvent, WaterfallNext,
};
pub use fiber::FiberState;
pub use plugin::{
    intercept_fn, Component, ComponentError, ConfigurableComponent, CordisError, CordisErrorCode,
    InterceptFn, Plugin, PluginError,
};
pub use realm::RealmId;
pub use registry::Registry;
pub use service::Service;
