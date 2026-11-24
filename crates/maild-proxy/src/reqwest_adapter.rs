//! Optional `reqwest` adapter behind `feature = "reqwest"`.
//!
//! Uses `.no_proxy()` to disable reqwest's implicit env/system detection and attaches
//! `reqwest::Proxy::custom(...)` backed by [`crate::resolve_route`].

use std::sync::Arc;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::model::{
    format_host_port, ProxyConfig, ProxyEndpoint, ProxyTarget, ResolvedRoute, SystemProxySnapshot,
    TargetScheme,
};
use crate::resolve::resolve_route;

/// Build a static `reqwest::Proxy` from a config + system proxy snapshot.
pub fn build_reqwest_proxy(config: ProxyConfig, system: SystemProxySnapshot) -> reqwest::Proxy {
    dynamic_reqwest_proxy(move || (config.clone(), system.clone()))
}

/// Build a dynamic `reqwest::Proxy` that queries `provider` on every outbound HTTP request.
pub fn dynamic_reqwest_proxy<F>(provider: F) -> reqwest::Proxy
where
    F: Fn() -> (ProxyConfig, SystemProxySnapshot) + Send + Sync + 'static,
{
    let provider = Arc::new(provider);
    reqwest::Proxy::custom(move |url| {
        let host = url.host_str()?;
        let port = url.port_or_known_default()?;
        let scheme = match url.scheme() {
            "https" => TargetScheme::Https,
            "http" => TargetScheme::Http,
            _ => return None,
        };
        let (config, system) = (provider)();
        let target = ProxyTarget::new(host, port, scheme);
        match resolve_route(&config, &system, &target) {
            ResolvedRoute::Direct => None,
            ResolvedRoute::Proxy(endpoint) => endpoint_to_url(&endpoint),
        }
    })
}

/// Disable reqwest's built-in env/system proxy and apply Featherpost's resolved proxy policy.
pub fn apply_proxy(
    builder: reqwest::ClientBuilder,
    config: ProxyConfig,
    system: SystemProxySnapshot,
) -> reqwest::ClientBuilder {
    builder.no_proxy().proxy(build_reqwest_proxy(config, system))
}

fn endpoint_to_url(endpoint: &ProxyEndpoint) -> Option<url::Url> {
    let authority = format_host_port(&endpoint.host, endpoint.port);
    let userinfo = match (
        endpoint.username.as_deref().filter(|u| !u.is_empty()),
        endpoint.password.as_deref().filter(|p| !p.is_empty()),
    ) {
        (Some(u), Some(p)) => format!(
            "{}:{}@",
            utf8_percent_encode(u, NON_ALPHANUMERIC),
            utf8_percent_encode(p, NON_ALPHANUMERIC)
        ),
        (Some(u), None) => format!("{}@", utf8_percent_encode(u, NON_ALPHANUMERIC)),
        (None, Some(p)) => format!(":{}@", utf8_percent_encode(p, NON_ALPHANUMERIC)),
        (None, None) => String::new(),
    };
    let raw = format!("{}://{userinfo}{authority}", endpoint.protocol.as_str());
    url::Url::parse(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CustomProxyConfig, ProxyMode, ProxyProtocol};

    #[test]
    fn endpoint_url_encodes_credentials_and_builds_client() {
        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: 7890,
            username: Some("a@b".into()),
            password: Some("p:w/1".into()),
        };
        let url = endpoint_to_url(&ep).unwrap();
        assert_eq!(url.scheme(), "socks5h");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(7890));
        assert_eq!(url.username(), "a%40b");
        assert_eq!(url.password(), Some("p%3Aw%2F1"));

        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: 7890,
                username: None,
                password: None,
                bypass: vec![],
            }),
        };
        let client = apply_proxy(reqwest::Client::builder(), cfg, SystemProxySnapshot::default())
            .build();
        assert!(client.is_ok());
    }
}
