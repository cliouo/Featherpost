pub mod api;
pub mod error;
pub mod provider;
pub mod secret;
pub mod state;
#[cfg(feature = "ui")]
pub mod ui;

pub const PLUGIN_ID: &str = "app.maild.provider.nodeseek-email";
pub const PRODUCTION_ORIGIN: &str = "https://seek.li/openapi/v1";
