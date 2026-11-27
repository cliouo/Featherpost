//! maild 守护进程库。
//!
//! - R3：`loader`（TOML 声明式配置装配）+ `config`（`Merge` / `intercept_config`）。
//! - R4：领域服务插件 `store` / `notify` / `otp` / `receive`（imap-idle），
//!   事件协议在 `events`（对齐 ARCHITECTURE.md §5）。
//! - R4.5：`accounts`——多账户装配（scoped + isolate）+ 统一入口 `assemble()`。
//! - M2 headless：`api`——JSON-RPC over stdio / 本地 HTTP（status / listAccounts /
//!   listMessageSummaries / listMessages / getMessage / send / getSettings / setSettings /
//!   claimNextOpenIntent；stdio 支持 message/received / account/state 推送），以及控制 CLI（`maild_brand::CLI_CTL`）
//!   独立 CLI。
//!
//! 装配入口：`accounts::assemble(ctx, toml)`（`[plugins]` 全局插件 + `[[accounts]]`
//! 每账户 receive），守护进程二进制（`src/bin/maild.rs`）与 e2e 测试共用同一条路径。

pub mod accounts;
pub mod account_catalog;
pub mod addresses;
pub mod agent_lifecycle;
mod agent_control;
pub mod api;
pub mod autodiscover;
pub mod compose;
pub mod config;
pub mod contacts;
pub mod containment;
pub mod credential;
pub mod daemon;
pub mod drafts;
pub mod events;
pub mod external;
pub mod filters;
pub mod folders;
pub mod frontend;
pub mod loader;
pub mod locale_preference;
pub mod localtime;
pub(crate) mod manifest_l10n;
pub mod menu;
pub mod mime;
pub mod notify;
pub mod otp;
pub mod paths;
pub mod pending;
pub mod plugin_identity;
pub mod plugin_state;
pub mod plugin_transactions;
pub mod probe;
pub mod provider;
pub mod proxy_preference;
pub mod reading_policy;
pub mod recovery;
pub mod receive;
pub mod rules;
pub mod scheduler;
pub mod search;
pub mod store;
pub mod translation_provider;
pub mod transport;
pub mod uploads;

pub use accounts::{assemble, global_registry, Accounts};
pub use daemon::{run_daemon, run_daemon_until, DaemonError, DaemonOptions};
pub use paths::assemble_with_paths;
