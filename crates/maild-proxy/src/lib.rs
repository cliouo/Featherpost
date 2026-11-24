//! Featherpost shared proxy core.
//!
//! Default features intentionally do NOT depend on `reqwest` so the background daemon
//! (`featherpost-agent`) retains its low memory footprint (ADR-0041). Enable the optional
//! `reqwest` feature for HTTP client consumers (`maild-ui`, external plugins).

pub mod bypass;
pub mod model;
pub mod resolve;
#[cfg(feature = "reqwest")]
pub mod reqwest_adapter;
pub mod system;
pub mod tunnel;

pub use bypass::{is_loopback_host, BypassList, BypassRule};
pub use model::{
    format_host_port, parse_proxy_url, parse_proxy_url_with_default_scheme,
    validate_custom_config, validate_proxy_config, CustomProxyConfig, ProxyConfig, ProxyEndpoint,
    ProxyError, ProxyErrorKind, ProxyMode, ProxyProtocol, ProxyTarget, ResolvedRoute,
    SystemProxySnapshot, TargetScheme, MAX_BYPASS_ENTRIES, MAX_HOST_BYTES, MAX_PASSWORD_BYTES,
    MAX_USERNAME_BYTES,
};
pub use resolve::{resolve_route, resolve_route_with_options, ResolveOptions};
pub use system::detect_system_proxy;
#[cfg(target_os = "macos")]
pub use system::SystemProxyWatcher;
pub use tunnel::{ConnectOptions, ProxyConnector, ProxyStream};
