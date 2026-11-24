//! Optional `reqwest` adapter behind `feature = "reqwest"`.
//!
//! Uses `.no_proxy()` to disable reqwest's implicit env/system detection and attaches
//! `reqwest::Proxy::custom(...)` backed by [`crate::resolve_route_with_options`] and optional
//! [`crate::ProxyResolver`] chains so conditional per-request routing works dynamically without
//! rebuilding the client.

use std::sync::Arc;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::bypass::is_loopback_host;
use crate::model::{
    format_host_port, ProxyConfig, ProxyEndpoint, ProxyTarget, ResolvedRoute, SystemProxySnapshot,
    TargetScheme,
};
use crate::resolve::{resolve_route_with_options, ResolveOptions};
use crate::routing::{ProxyContext, ProxyContextDefaults, ProxyDecision, ProxyResolver};

/// Build a static `reqwest::Proxy` from a config + system proxy snapshot.
pub fn build_reqwest_proxy(config: ProxyConfig, system: SystemProxySnapshot) -> reqwest::Proxy {
    dynamic_reqwest_proxy(move || (config.clone(), system.clone()))
}

/// Build a static `reqwest::Proxy` with an optional [`ProxyResolver`], default context, and
/// resolve options.
pub fn build_reqwest_proxy_with_resolver(
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    defaults: ProxyContextDefaults,
    options: ResolveOptions,
) -> reqwest::Proxy {
    dynamic_reqwest_proxy_with_resolver(
        move || (config.clone(), system.clone(), resolver.clone()),
        defaults,
        options,
    )
}

/// Build a dynamic `reqwest::Proxy` that queries `provider` on every outbound HTTP request.
pub fn dynamic_reqwest_proxy<F>(provider: F) -> reqwest::Proxy
where
    F: Fn() -> (ProxyConfig, SystemProxySnapshot) + Send + Sync + 'static,
{
    dynamic_reqwest_proxy_with_resolver(
        move || {
            let (config, system) = (provider)();
            (config, system, None)
        },
        ProxyContextDefaults::default(),
        ResolveOptions::default(),
    )
}

/// Build a dynamic `reqwest::Proxy` backed by an optional [`ProxyResolver`] + global fallback.
///
/// Evaluated per outbound HTTP request URL inside `reqwest::Proxy::custom`.
pub fn dynamic_reqwest_proxy_with_resolver<F>(
    provider: F,
    defaults: ProxyContextDefaults,
    options: ResolveOptions,
) -> reqwest::Proxy
where
    F: Fn() -> (ProxyConfig, SystemProxySnapshot, Option<Arc<dyn ProxyResolver>>)
        + Send
        + Sync
        + 'static,
{
    dynamic_reqwest_proxy_for_context(
        provider,
        move |_url, target| defaults.build_context(target),
        options,
    )
}

/// Build a dynamic `reqwest::Proxy` with a custom per-URL [`ProxyContext`] builder (e.g. to attach
/// tags or metadata based on scheme/host/port or runtime state for conditional per-request routing).
///
/// Note: `reqwest::Proxy::custom` operates at the transport connector layer and typically receives
/// only the target origin (`scheme://host[:port]`) rather than the HTTP request path or query.
pub fn dynamic_reqwest_proxy_for_context<F, C>(
    provider: F,
    context_builder: C,
    options: ResolveOptions,
) -> reqwest::Proxy
where
    F: Fn() -> (ProxyConfig, SystemProxySnapshot, Option<Arc<dyn ProxyResolver>>)
        + Send
        + Sync
        + 'static,
    C: Fn(&url::Url, ProxyTarget) -> ProxyContext + Send + Sync + 'static,
{
    let provider = Arc::new(provider);
    let context_builder = Arc::new(context_builder);
    reqwest::Proxy::custom(move |url| {
        let target = target_from_url(url)?;
        let ctx = (context_builder)(url, target);
        let (config, system, resolver) = (provider)();
        match resolve_url_route(&config, &system, resolver.as_deref(), &ctx, options) {
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
    builder
        .no_proxy()
        .proxy(build_reqwest_proxy(config, system))
}

/// Disable reqwest's built-in env/system proxy and apply a resolver-backed proxy policy.
pub fn apply_proxy_with_resolver(
    builder: reqwest::ClientBuilder,
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    defaults: ProxyContextDefaults,
    options: ResolveOptions,
) -> reqwest::ClientBuilder {
    builder
        .no_proxy()
        .proxy(build_reqwest_proxy_with_resolver(
            config, system, resolver, defaults, options,
        ))
}

/// Disable reqwest's built-in env/system proxy and attach a pre-built dynamic `reqwest::Proxy`.
pub fn apply_dynamic_proxy(
    builder: reqwest::ClientBuilder,
    proxy: reqwest::Proxy,
) -> reqwest::ClientBuilder {
    builder.no_proxy().proxy(proxy)
}

/// Resolve the effective [`ResolvedRoute`] for an HTTP(S) [`ProxyContext`], enforcing mandatory
/// loopback bypass before consulting `resolver` and falling back to global `config` + `system`.
pub fn resolve_url_route(
    config: &ProxyConfig,
    system: &SystemProxySnapshot,
    resolver: Option<&dyn ProxyResolver>,
    ctx: &ProxyContext,
    options: ResolveOptions,
) -> ResolvedRoute {
    let allow_loopback = {
        #[cfg(any(test, debug_assertions))]
        {
            options.allow_loopback_proxy
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = options;
            false
        }
    };
    if !allow_loopback && is_loopback_host(&ctx.target.host) {
        return ResolvedRoute::Direct;
    }
    if let Some(resolver) = resolver {
        match resolver.resolve(ctx) {
            ProxyDecision::Direct => return ResolvedRoute::Direct,
            ProxyDecision::Use(ep) => return ResolvedRoute::Proxy(ep),
            ProxyDecision::Inherit => {}
        }
    }
    resolve_route_with_options(config, system, &ctx.target, options)
}

fn target_from_url(url: &url::Url) -> Option<ProxyTarget> {
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    let scheme = match url.scheme() {
        "https" => TargetScheme::Https,
        "http" => TargetScheme::Http,
        _ => return None,
    };
    Some(ProxyTarget::new(host, port, scheme))
}

pub(crate) fn endpoint_to_url(endpoint: &ProxyEndpoint) -> Option<url::Url> {
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
    use crate::routing::{
        ConditionalProxyConfig, ProxyMatcher, ProxyOverride, ProxyPurpose, ProxyRule,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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

    async fn spawn_http_forward_proxy(tag: &'static str) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let hits = Arc::clone(&hits_clone);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let _ = stream.read(&mut buf).await;
                    hits.fetch_add(1, Ordering::SeqCst);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{tag}",
                        tag.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });
        (addr, hits)
    }

    #[tokio::test]
    async fn single_reqwest_client_routes_per_request_conditionally_and_falls_back_to_global() {
        let (proxy_a, hits_a) = spawn_http_forward_proxy("via-proxy-a").await;
        let (proxy_b, hits_b) = spawn_http_forward_proxy("via-proxy-b").await;
        let (proxy_global, hits_global) = spawn_http_forward_proxy("via-global").await;

        let cond = ConditionalProxyConfig::new(
            vec![
                ProxyRule::new(
                    ProxyMatcher::default().with_host("*.alpha.example"),
                    ProxyDecision::Use(ProxyEndpoint {
                        protocol: ProxyProtocol::Http,
                        host: "127.0.0.1".into(),
                        port: proxy_a.port(),
                        username: None,
                        password: None,
                    }),
                ),
                ProxyRule::new(
                    ProxyMatcher::default().with_tag("special-route"),
                    ProxyDecision::Use(ProxyEndpoint {
                        protocol: ProxyProtocol::Http,
                        host: "127.0.0.1".into(),
                        port: proxy_b.port(),
                        username: None,
                        password: None,
                    }),
                ),
            ],
            ProxyOverride::Inherit,
        )
        .compile()
        .unwrap();

        let global = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Http,
                host: "127.0.0.1".into(),
                port: proxy_global.port(),
                username: None,
                password: None,
                bypass: vec![],
            }),
        };
        let resolver: Arc<dyn ProxyResolver> = Arc::new(cond);
        let enable_special = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let enable_special_ctx = Arc::clone(&enable_special);
        let proxy = dynamic_reqwest_proxy_for_context(
            move || {
                (
                    global.clone(),
                    SystemProxySnapshot::default(),
                    Some(Arc::clone(&resolver)),
                )
            },
            move |url, target| {
                let mut ctx = ProxyContext::new(target).with_purpose(ProxyPurpose::Plugin);
                if enable_special_ctx.load(Ordering::SeqCst)
                    && url.host_str() == Some("beta.example")
                {
                    ctx = ctx.with_tag("special-route");
                }
                ctx
            },
            ResolveOptions::default(),
        );
        let client = apply_dynamic_proxy(reqwest::Client::builder(), proxy)
            .build()
            .unwrap();

        let body_a = client
            .get("http://img.alpha.example/pic.png")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body_a, "via-proxy-a");
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);

        let body_b = client
            .get("http://beta.example/item")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body_b, "via-proxy-b");
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);

        // Unmatched host falls back to global proxy.
        let body_g = client
            .get("http://unmatched.example/normal")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body_g, "via-global");
        assert_eq!(hits_global.load(Ordering::SeqCst), 1);

        // Flipping runtime condition dynamically re-routes beta.example to global without
        // rebuilding the reqwest::Client.
        enable_special.store(false, Ordering::SeqCst);
        let body_g2 = client
            .get("http://beta.example/item")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body_g2, "via-global");
        assert_eq!(hits_global.load(Ordering::SeqCst), 2);
    }
}
