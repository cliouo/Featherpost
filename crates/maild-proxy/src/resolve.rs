//! Per-connection proxy route resolution.
//!
//! Resolution precedence:
//! 1. Mandatory loopback bypass (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`) -> `Direct`
//!    (opt-out available only in tests/debug builds via `ResolveOptions::allow_loopback_proxy`).
//! 2. `ProxyMode::Direct` -> always `Direct` (ignores OS settings and env vars).
//! 3. `ProxyMode::Custom` -> check `custom.bypass`, else strictly use configured endpoint.
//! 4. `ProxyMode::System` -> check OS exceptions + `NO_PROXY`, else select by target scheme:
//!    - `Http`: `system.http` -> `system.socks5`
//!    - `Https`: `system.https` -> `system.http` -> `system.socks5`
//!    - `Imap`/`Imaps`/`Smtp`/`Smtps`/`RawTcp`: `system.socks5` (as `socks5h`) -> `system.https` -> `system.http`.

use crate::bypass::{is_loopback_host, BypassList};
use crate::model::{
    ProxyConfig, ProxyMode, ProxyProtocol, ProxyTarget, ResolvedRoute, SystemProxySnapshot,
    TargetScheme,
};

/// Route resolution options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    /// Test-only opt-out for mandatory loopback bypass so local mock proxy integration tests
    /// can tunnel traffic to a local `127.0.0.1` fake mail server. Never compiled into release
    /// production code paths.
    #[cfg(any(test, debug_assertions))]
    pub allow_loopback_proxy: bool,
}

/// Resolve the proxy route for a single target connection using default production options.
pub fn resolve_route(
    config: &ProxyConfig,
    system: &SystemProxySnapshot,
    target: &ProxyTarget,
) -> ResolvedRoute {
    resolve_route_with_options(config, system, target, ResolveOptions::default())
}

/// Resolve the proxy route with explicit [`ResolveOptions`].
pub fn resolve_route_with_options(
    config: &ProxyConfig,
    system: &SystemProxySnapshot,
    target: &ProxyTarget,
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

    // 1. Mandatory loopback bypass.
    if !allow_loopback && is_loopback_host(&target.host) {
        return ResolvedRoute::Direct;
    }

    match config.mode {
        // 2. Direct mode ignores OS and env proxies completely.
        ProxyMode::Direct => ResolvedRoute::Direct,

        // 3. Custom mode checks its own bypass list, then strictly uses custom proxy.
        ProxyMode::Custom => {
            let Some(custom) = &config.custom else {
                return ResolvedRoute::Direct;
            };
            let bypass = BypassList::parse_entries(&custom.bypass);
            if bypass.matches(&target.host, target.port) {
                return ResolvedRoute::Direct;
            }
            ResolvedRoute::Proxy(custom.to_endpoint())
        }

        // 4. System mode checks OS exceptions + NO_PROXY, then selects by target scheme.
        ProxyMode::System => {
            let bypass = BypassList::parse_entries(&system.bypass)
                .with_exclude_simple_hostnames(system.exclude_simple_hostnames);
            if bypass.matches(&target.host, target.port) {
                return ResolvedRoute::Direct;
            }
            let selected = match target.scheme {
                TargetScheme::Http => system.http.clone().or_else(|| system.socks5.clone()),
                TargetScheme::Https => system
                    .https
                    .clone()
                    .or_else(|| system.http.clone())
                    .or_else(|| system.socks5.clone()),
                TargetScheme::Imap
                | TargetScheme::Imaps
                | TargetScheme::Smtp
                | TargetScheme::Smtps
                | TargetScheme::RawTcp => system
                    .socks5
                    .clone()
                    .map(|ep| ep.with_protocol(ProxyProtocol::Socks5h))
                    .or_else(|| system.https.clone())
                    .or_else(|| system.http.clone()),
            };
            match selected {
                Some(ep) => ResolvedRoute::Proxy(ep),
                None => ResolvedRoute::Direct,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CustomProxyConfig, ProxyEndpoint};

    fn ep(protocol: ProxyProtocol, host: &str, port: u16) -> ProxyEndpoint {
        ProxyEndpoint {
            protocol,
            host: host.into(),
            port,
            username: None,
            password: None,
        }
    }

    fn full_system() -> SystemProxySnapshot {
        SystemProxySnapshot {
            http: Some(ep(ProxyProtocol::Http, "10.0.0.1", 8080)),
            https: Some(ep(ProxyProtocol::Http, "10.0.0.1", 8443)),
            socks5: Some(ep(ProxyProtocol::Socks5, "10.0.0.1", 1080)),
            bypass: vec!["*.local".into(), "169.254/16".into()],
            exclude_simple_hostnames: true,
            pac_url: None,
            source: "test".into(),
        }
    }

    #[test]
    fn loopback_is_always_direct_by_default_even_in_custom_mode() {
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Socks5h,
                host: "10.0.0.2".into(),
                port: 7890,
                username: None,
                password: None,
                bypass: vec![],
            }),
        };
        let sys = full_system();
        for host in ["localhost", "sub.localhost", "127.0.0.1", "127.1.2.3", "::1"] {
            let route = resolve_route(&cfg, &sys, &ProxyTarget::new(host, 993, TargetScheme::Imaps));
            assert_eq!(route, ResolvedRoute::Direct, "{host} must bypass proxy");
        }

        // Test opt-out enables proxying loopback for hermetic mock tests.
        let route = resolve_route_with_options(
            &cfg,
            &sys,
            &ProxyTarget::new("127.0.0.1", 993, TargetScheme::Imaps),
            ResolveOptions {
                allow_loopback_proxy: true,
            },
        );
        assert_eq!(
            route,
            ResolvedRoute::Proxy(ep(ProxyProtocol::Socks5h, "10.0.0.2", 7890))
        );
    }

    #[test]
    fn direct_mode_ignores_system_and_custom() {
        let cfg = ProxyConfig {
            mode: ProxyMode::Direct,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Http,
                host: "10.0.0.2".into(),
                port: 8080,
                username: None,
                password: None,
                bypass: vec![],
            }),
        };
        let sys = full_system();
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("imap.gmail.com", 993, TargetScheme::Imaps)),
            ResolvedRoute::Direct
        );
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("example.com", 443, TargetScheme::Https)),
            ResolvedRoute::Direct
        );
    }

    #[test]
    fn custom_mode_respects_bypass_and_uses_configured_protocol_strictly() {
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Http,
                host: "proxy.corp.test".into(),
                port: 3128,
                username: None,
                password: None,
                bypass: vec![".internal.example".into()],
            }),
        };
        let sys = full_system();
        assert_eq!(
            resolve_route(
                &cfg,
                &sys,
                &ProxyTarget::new("imap.internal.example", 993, TargetScheme::Imaps)
            ),
            ResolvedRoute::Direct
        );
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("imap.gmail.com", 993, TargetScheme::Imaps)),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "proxy.corp.test", 3128))
        );
    }

    #[test]
    fn system_mode_follows_scheme_precedence_and_upgrades_mail_socks_to_socks5h() {
        let cfg = ProxyConfig::default();
        let sys = full_system();

        // Mail/RawTcp: socks5 (upgraded to socks5h) -> https -> http
        for scheme in [
            TargetScheme::Imap,
            TargetScheme::Imaps,
            TargetScheme::Smtp,
            TargetScheme::Smtps,
            TargetScheme::RawTcp,
        ] {
            let route = resolve_route(&cfg, &sys, &ProxyTarget::new("imap.gmail.com", 993, scheme));
            assert_eq!(
                route,
                ResolvedRoute::Proxy(ep(ProxyProtocol::Socks5h, "10.0.0.1", 1080)),
                "{scheme:?} should pick socks5h first"
            );
        }

        // Http: http -> socks5
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("example.com", 80, TargetScheme::Http)),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.1", 8080))
        );

        // Https: https -> http -> socks5
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("example.com", 443, TargetScheme::Https)),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.1", 8443))
        );

        // Fallback when socks5 absent: mail uses https -> http
        let mut no_socks = full_system();
        no_socks.socks5 = None;
        assert_eq!(
            resolve_route(&cfg, &no_socks, &ProxyTarget::new("imap.gmail.com", 993, TargetScheme::Imaps)),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.1", 8443))
        );
        no_socks.https = None;
        assert_eq!(
            resolve_route(&cfg, &no_socks, &ProxyTarget::new("imap.gmail.com", 993, TargetScheme::Imaps)),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.1", 8080))
        );

        // System bypass and ExcludeSimpleHostnames
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("nas.local", 993, TargetScheme::Imaps)),
            ResolvedRoute::Direct
        );
        assert_eq!(
            resolve_route(&cfg, &sys, &ProxyTarget::new("mailserver", 993, TargetScheme::Imaps)),
            ResolvedRoute::Direct
        );
    }
}
