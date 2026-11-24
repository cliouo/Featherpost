//! Hermetic integration tests for Phase 1 of the Featherpost global proxy module.
//!
//! Covers:
//! - IMAP receive & SMTP transport via local mock HTTP CONNECT (plain + TLS-to-proxy) and
//!   SOCKS5/SOCKS5h (no-auth + RFC 1929 auth), including auth failure `AccountDetailCode`.
//! - Probe (IMAP + SMTP) and Autodiscover (remote-DNS proxy + SSRF guard).
//! - IDLE hot-reload and reconnect backoff interruption on proxy change.
//! - JSON-RPC `getProxyConfig` (with `include_secret` origin guard), `setProxyConfig`,
//!   `testProxy` (active, draft + `use_saved_password`, default account target), and
//!   `preferences/proxy` push notification.
//! - Fail-closed corrupt `proxy_preference.toml` without overwriting disk.
//! - `maild-client` HTTP loopback RPC bypassing `HTTP_PROXY` / `ALL_PROXY`.

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use common::fake_mail::FakeMail;
use cordis::Context;
use maild::account_catalog::SecurityMode;
#[cfg(unix)]
use maild::api::{dispatch_with_origin, serve_unix_socket, RequestOrigin};
use maild::api::{dispatch, serve_http, JsonRpcRequest};
use maild::autodiscover::discover_provider_with_proxy;
use maild::events::{AccountDetailCode, AccountStateKind};
use maild::paths::assemble_with_paths;
use maild::probe::{
    probe_incoming_with_proxy, probe_outgoing_with_proxy, IncomingProbe, OutgoingProbe, ProbeStage,
};
use maild::proxy_preference::{
    CustomProxyUpdate, ProxyConfigUpdate, ProxyService, PREFERENCE_FILE,
};
use maild::receive::Receive;
use maild::transport::{Transport, TransportConfig};
#[cfg(unix)]
use maild_client::{ServerNotification, StreamConnection};
use maild_client::{Endpoint, RpcClient};
use maild_paths::AppPaths;
#[cfg(unix)]
use maild_proxy::ProxyEndpoint;
use maild_proxy::{
    CustomProxyConfig, ProxyConfig, ProxyConnector, ProxyMode, ProxyProtocol, SystemProxySnapshot,
};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// Local hermetic mock proxy servers (HTTP CONNECT plain/TLS & SOCKS5)
// ---------------------------------------------------------------------------

struct MockProxy {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
    shutdown: watch::Sender<bool>,
}

impl MockProxy {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

impl Drop for MockProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

fn self_signed_tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])
    .expect("generate self-signed cert");
    let cert_der = certified.cert.der().clone();
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der())
        .expect("private key der");
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("rustls server config");
    tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg))
}

async fn start_mock_http_proxy(
    expected_auth: Option<(String, String)>,
    use_tls: bool,
    rewrite_target: Option<SocketAddr>,
) -> MockProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let tls_acceptor = use_tls.then(self_signed_tls_acceptor);

    let hits_clone = Arc::clone(&hits);
    let targets_clone = Arc::clone(&targets);
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((tcp, _)) = accepted else {
                break;
            };
            let expected_auth = expected_auth.clone();
            let hits = Arc::clone(&hits_clone);
            let targets = Arc::clone(&targets_clone);
            let tls_acceptor = tls_acceptor.clone();
            tokio::spawn(async move {
                if let Some(acceptor) = tls_acceptor {
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        let _ = handle_http_connect(
                            tls,
                            expected_auth.as_ref(),
                            rewrite_target,
                            hits,
                            targets,
                        )
                        .await;
                    }
                } else {
                    let _ = handle_http_connect(
                        tcp,
                        expected_auth.as_ref(),
                        rewrite_target,
                        hits,
                        targets,
                    )
                    .await;
                }
            });
        }
    });

    MockProxy {
        addr,
        hits,
        targets,
        shutdown,
    }
}

async fn handle_http_connect<S>(
    mut stream: S,
    expected_auth: Option<&(String, String)>,
    rewrite_target: Option<SocketAddr>,
    hits: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let authority = parts.next().unwrap_or("").to_string();
    if method != "CONNECT" || authority.is_empty() {
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            .await?;
        return Ok(());
    }

    if let Some((user, pass)) = expected_auth {
        let expected_b64 = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        let expected_hdr = format!("basic {expected_b64}");
        let authorized = lines.any(|line| {
            line.split_once(':').is_some_and(|(k, v)| {
                k.eq_ignore_ascii_case("proxy-authorization")
                    && v.trim().eq_ignore_ascii_case(&expected_hdr)
            })
        });
        if !authorized {
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\n\r\n")
                .await?;
            return Ok(());
        }
    }

    hits.fetch_add(1, Ordering::SeqCst);
    targets.lock().unwrap().push(authority.clone());

    let upstream = match rewrite_target {
        Some(target_addr) => TcpStream::connect(target_addr).await,
        None => TcpStream::connect(&authority).await,
    };
    let mut upstream = match upstream {
        Ok(s) => s,
        Err(_) => {
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            return Ok(());
        }
    };

    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    stream.flush().await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
    Ok(())
}

async fn start_mock_socks5_proxy(
    expected_auth: Option<(String, String)>,
    rewrite_target: Option<SocketAddr>,
) -> MockProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let (shutdown, mut shutdown_rx) = watch::channel(false);

    let hits_clone = Arc::clone(&hits);
    let targets_clone = Arc::clone(&targets);
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((tcp, _)) = accepted else {
                break;
            };
            let expected_auth = expected_auth.clone();
            let hits = Arc::clone(&hits_clone);
            let targets = Arc::clone(&targets_clone);
            tokio::spawn(async move {
                let _ = handle_socks5(tcp, expected_auth.as_ref(), rewrite_target, hits, targets).await;
            });
        }
    });

    MockProxy {
        addr,
        hits,
        targets,
        shutdown,
    }
}

async fn handle_socks5(
    mut stream: TcpStream,
    expected_auth: Option<&(String, String)>,
    rewrite_target: Option<SocketAddr>,
    hits: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).await?;
    if hdr[0] != 0x05 {
        return Ok(());
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    stream.read_exact(&mut methods).await?;

    if let Some((expected_u, expected_p)) = expected_auth {
        if !methods.contains(&0x02) {
            stream.write_all(&[0x05, 0xFF]).await?;
            return Ok(());
        }
        stream.write_all(&[0x05, 0x02]).await?;

        let mut auth_hdr = [0u8; 2];
        stream.read_exact(&mut auth_hdr).await?;
        let mut user_buf = vec![0u8; auth_hdr[1] as usize];
        stream.read_exact(&mut user_buf).await?;
        let mut plen = [0u8; 1];
        stream.read_exact(&mut plen).await?;
        let mut pass_buf = vec![0u8; plen[0] as usize];
        stream.read_exact(&mut pass_buf).await?;

        if auth_hdr[0] != 0x01
            || user_buf != expected_u.as_bytes()
            || pass_buf != expected_p.as_bytes()
        {
            stream.write_all(&[0x01, 0x01]).await?;
            return Ok(());
        }
        stream.write_all(&[0x01, 0x00]).await?;
    } else {
        stream.write_all(&[0x05, 0x00]).await?;
    }

    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    if req[0] != 0x05 || req[1] != 0x01 {
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Ok(());
    }

    let host = match req[3] {
        0x01 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets).await?;
            std::net::Ipv4Addr::from(octets).to_string()
        }
        0x04 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            std::net::Ipv6Addr::from(octets).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8_lossy(&domain).to_string()
        }
        _ => {
            stream
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Ok(());
        }
    };
    let mut port_bytes = [0u8; 2];
    stream.read_exact(&mut port_bytes).await?;
    let port = u16::from_be_bytes(port_bytes);

    hits.fetch_add(1, Ordering::SeqCst);
    targets.lock().unwrap().push(format!("{host}:{port}"));

    let upstream = match rewrite_target {
        Some(target_addr) => TcpStream::connect(target_addr).await,
        None => TcpStream::connect((host.as_str(), port)).await,
    };
    let mut upstream = match upstream {
        Ok(s) => s,
        Err(_) => {
            stream
                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Ok(());
        }
    };

    let bound = upstream.local_addr().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
    let mut reply = vec![0x05, 0x00, 0x00, 0x01];
    match bound.ip() {
        std::net::IpAddr::V4(v4) => reply.extend_from_slice(&v4.octets()),
        std::net::IpAddr::V6(_) => reply.extend_from_slice(&[127, 0, 0, 1]),
    }
    reply.extend_from_slice(&bound.port().to_be_bytes());
    stream.write_all(&reply).await?;
    stream.flush().await?;

    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn secure_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

fn test_proxy_service(dir: &Path) -> Arc<ProxyService> {
    let service = Arc::new(ProxyService::open_with_system(
        dir.join(PREFERENCE_FILE),
        SystemProxySnapshot::default,
    ));
    service.set_allow_loopback_proxy(true);
    service.set_accept_invalid_proxy_certs(true);
    service
}

fn req(method: &str, params: Value, id: u64) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!(id),
        method: method.into(),
        params,
    }
}

fn enable_test_invalid_certs() {
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
}

// ---------------------------------------------------------------------------
// 1. IMAP & SMTP through HTTP CONNECT (plain) and HTTPS proxy (TLS-over-TLS)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn imap_and_smtp_tunnel_through_http_connect_and_https_proxy() {
    enable_test_invalid_certs();
    let mail = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();
    let http_proxy = start_mock_http_proxy(
        Some(("proxyuser".into(), "proxypass".into())),
        false,
        None,
    )
    .await;
    let https_proxy = start_mock_http_proxy(
        Some(("tlsuser".into(), "tlspass".into())),
        true,
        None,
    )
    .await;

    let dir = secure_dir();
    let proxy_service = test_proxy_service(dir.path());
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Http,
                host: "127.0.0.1".into(),
                port: http_proxy.addr.port(),
                username: Some("proxyuser".into()),
                password: Some("proxypass".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();

    // IMAP implicit TLS over plain HTTP CONNECT with Basic auth
    let ctx = Context::new();
    ctx.provide::<ProxyService>(Arc::clone(&proxy_service));
    let toml = format!(
        r#"
[plugins.store.config]
db_path = ""

[[accounts]]
account = "demo"
server = "127.0.0.1"
port = {}
username = "alice"
password = "secret"
tls = "implicit"
accept_invalid_certs = true
idle = true
"#,
        mail.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).unwrap();
    let accounts = ctx.get::<maild::Accounts>().unwrap();
    let receive: Receive = accounts.snapshot()[0].2.clone();
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("IMAP must come online through HTTP CONNECT proxy");
    assert!(http_proxy.hits() >= 1, "IMAP must traverse HTTP CONNECT proxy");

    // Switch proxy to HTTPS (TLS-to-proxy) and verify SMTP STARTTLS (TLS-over-TLS!)
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Https,
                host: "127.0.0.1".into(),
                port: https_proxy.addr.port(),
                username: Some("tlsuser".into()),
                password: Some("tlspass".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();

    let transport = Transport::from_config_with_proxy(
        TransportConfig {
            server: "127.0.0.1".into(),
            port: mail.smtp_addr.port(),
            username: "alice".into(),
            password: "secret".into(),
            tls: "starttls".into(),
            accept_invalid_certs: true,
        },
        Some(Arc::clone(&proxy_service)),
    );
    transport
        .send("alice@example.com", "bob@example.com", "via https proxy", "hello tunnel")
        .await
        .expect("SMTP STARTTLS over HTTPS proxy must succeed");
    assert!(
        https_proxy.hits() >= 1,
        "SMTP must traverse HTTPS CONNECT proxy (TLS-over-TLS)"
    );

    asm.dispose().await;
}

// ---------------------------------------------------------------------------
// 2. SOCKS5h (no-auth + RFC 1929 auth + auth failure AccountDetailCode)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn imap_and_smtp_through_socks5_no_auth_and_auth_with_error_classification() {
    enable_test_invalid_certs();
    let mail = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();
    let socks_no_auth = start_mock_socks5_proxy(None, None).await;
    let socks_auth =
        start_mock_socks5_proxy(Some(("socksuser".into(), "supersecret99".into())), None).await;

    let dir = secure_dir();
    let proxy_service = test_proxy_service(dir.path());

    // First test wrong password -> ProxyAuthFailed without leaking password
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: socks_auth.addr.port(),
                username: Some("socksuser".into()),
                password: Some("wrong-pass-do-not-leak".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();

    let ctx = Context::new();
    ctx.provide::<ProxyService>(Arc::clone(&proxy_service));
    let toml = format!(
        r#"
[plugins.store.config]
db_path = ""

[[accounts]]
account = "socks-acc"
server = "127.0.0.1"
port = {}
username = "alice"
password = "secret"
tls = "implicit"
accept_invalid_certs = true
reconnect_secs = 3600
idle = true
"#,
        mail.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).unwrap();
    let receive = ctx.get::<maild::Accounts>().unwrap().snapshot()[0].2.clone();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let st = receive.state();
        if st.state == AccountStateKind::Offline {
            assert_eq!(st.detail_code, Some(AccountDetailCode::ProxyAuthFailed));
            assert!(
                !st.detail.contains("wrong-pass-do-not-leak"),
                "proxy password must never leak into account detail"
            );
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "expected offline with ProxyAuthFailed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Now fix password -> backoff sleep is interrupted immediately and account comes Online!
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: socks_auth.addr.port(),
                username: Some("socksuser".into()),
                password: Some("supersecret99".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), receive.wait_online())
        .await
        .expect("fixing proxy password must interrupt 3600s backoff and come online");
    assert!(socks_auth.hits() >= 1);

    // SMTP through no-auth SOCKS5
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Socks5,
                host: "127.0.0.1".into(),
                port: socks_no_auth.addr.port(),
                username: None,
                password: None,
                clear_password: true,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();
    let transport = Transport::from_config_with_proxy(
        TransportConfig {
            server: "127.0.0.1".into(),
            port: mail.smtp_addr.port(),
            username: "alice".into(),
            password: "secret".into(),
            tls: "starttls".into(),
            accept_invalid_certs: true,
        },
        Some(Arc::clone(&proxy_service)),
    );
    transport
        .send("alice@example.com", "bob@example.com", "via socks5", "hello socks")
        .await
        .expect("SMTP via no-auth SOCKS5 must succeed");
    assert!(socks_no_auth.hits() >= 1);

    asm.dispose().await;
}

// ---------------------------------------------------------------------------
// 3. Probe (IMAP & SMTP) + Autodiscover (remote DNS + SSRF guard)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn probe_and_autodiscover_use_proxy_without_ssrf_regression() {
    enable_test_invalid_certs();
    let mail = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();
    let socks_proxy = start_mock_socks5_proxy(None, None).await;
    let http_proxy = start_mock_http_proxy(None, true, None).await;

    let socks_connector = ProxyConnector::new(
        ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: socks_proxy.addr.port(),
                username: None,
                password: None,
                bypass: vec![],
            }),
        },
        SystemProxySnapshot::default(),
    )
    .with_allow_loopback_proxy(true);

    let incoming = probe_incoming_with_proxy(
        IncomingProbe {
            server: "127.0.0.1",
            port: mail.imap_addr.port(),
            security: SecurityMode::Implicit,
            username: "alice",
            secret: b"secret",
            mailbox: "INBOX",
            idle: true,
        },
        Duration::from_secs(10),
        Some(&socks_connector),
    )
    .await;
    assert!(incoming.ok, "IMAP probe via SOCKS5h failed: {incoming:?}");
    assert_eq!(incoming.stage, ProbeStage::Ready);
    assert!(socks_proxy.hits() >= 1);

    let https_connector = ProxyConnector::new(
        ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Https,
                host: "127.0.0.1".into(),
                port: http_proxy.addr.port(),
                username: None,
                password: None,
                bypass: vec![],
            }),
        },
        SystemProxySnapshot::default(),
    )
    .with_allow_loopback_proxy(true)
    .with_accept_invalid_proxy_certs(true);

    let outgoing = probe_outgoing_with_proxy(
        OutgoingProbe {
            server: "127.0.0.1",
            port: mail.smtp_addr.port(),
            security: SecurityMode::StartTls,
            username: "alice",
            secret: Some(b"secret"),
        },
        Duration::from_secs(10),
        Some(&https_connector),
    )
    .await;
    assert!(outgoing.ok, "SMTP probe via HTTPS proxy failed: {outgoing:?}");
    assert_eq!(outgoing.stage, ProbeStage::Ready);
    assert!(http_proxy.hits() >= 1);

    // Autodiscover through remote-DNS SOCKS5h proxy (domain does not exist in local DNS!)
    let dummy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dummy_addr = dummy_listener.local_addr().unwrap();
    let accept_task = tokio::spawn(async move {
        loop {
            if dummy_listener.accept().await.is_err() {
                break;
            }
        }
    });
    let autodiscover_proxy = start_mock_socks5_proxy(None, Some(dummy_addr)).await;
    let autodiscover_connector = ProxyConnector::new(
        ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: autodiscover_proxy.addr.port(),
                username: None,
                password: None,
                bypass: vec![],
            }),
        },
        SystemProxySnapshot::default(),
    );

    // SSRF checks must STILL reject private/loopback/reserved domains before touching proxy!
    for forbidden in ["alice@localhost", "alice@127.0.0.1", "alice@corp.internal", "alice@nas.local"] {
        assert!(
            discover_provider_with_proxy(forbidden, true, Some(&autodiscover_connector))
                .await
                .is_err(),
            "{forbidden} must be rejected by SSRF validation"
        );
    }
    assert_eq!(autodiscover_proxy.hits(), 0, "blocked domains must never hit proxy");

    // Public domain succeeds via remote-DNS proxy even though it has no local DNS record!
    let discovered = discover_provider_with_proxy(
        "alice@nonexistent-corp-mail-domain.org",
        true,
        Some(&autodiscover_connector),
    )
    .await
    .expect("public domain autodiscover via socks5h must succeed");
    assert_eq!(discovered.source, "tcp_probe");
    assert_eq!(discovered.confidence, "high");
    assert!(autodiscover_proxy.hits() >= 1);
    assert!(
        autodiscover_proxy
            .targets()
            .iter()
            .any(|t| t == "imap.nonexistent-corp-mail-domain.org:993"),
        "expected remote-DNS target in {:?}",
        autodiscover_proxy.targets()
    );
    accept_task.abort();
}

// ---------------------------------------------------------------------------
// 4. IDLE hot-reload on proxy change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn idle_session_reconnects_immediately_when_proxy_changes() {
    let mail = FakeMail::start().await.unwrap();
    let proxy1 = start_mock_socks5_proxy(None, None).await;
    let proxy2 = start_mock_socks5_proxy(None, None).await;

    let dir = secure_dir();
    let proxy_service = test_proxy_service(dir.path());
    proxy_service
        .set(&ProxyConfigUpdate {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyUpdate {
                protocol: ProxyProtocol::Socks5h,
                host: "127.0.0.1".into(),
                port: proxy1.addr.port(),
                username: None,
                password: None,
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .unwrap();

    let ctx = Context::new();
    ctx.provide::<ProxyService>(Arc::clone(&proxy_service));
    let toml = format!(
        r#"
[plugins.store.config]
db_path = ""

[[accounts]]
account = "idle-hot"
server = "127.0.0.1"
port = {}
username = "alice"
password = "secret"
tls = "none"
idle = true
"#,
        mail.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).unwrap();
    let receive = ctx.get::<maild::Accounts>().unwrap().snapshot()[0].2.clone();
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("account must enter IDLE online via proxy1");
    assert_eq!(proxy1.hits(), 1);
    assert_eq!(proxy2.hits(), 0);

    // Switch active proxy while session is parked in IDLE
    let resp = dispatch(
        &ctx,
        &req(
            "setProxyConfig",
            json!({
                "mode": "custom",
                "custom": {
                    "protocol": "socks5h",
                    "host": "127.0.0.1",
                    "port": proxy2.addr.port(),
                    "bypass": []
                }
            }),
            1,
        ),
    )
    .await;
    assert!(resp.error.is_none(), "setProxyConfig failed: {:?}", resp.error);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while proxy2.hits() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "IDLE session must gracefully reconnect through proxy2 without waiting for 25m IDLE renew"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::timeout(Duration::from_secs(5), receive.wait_online())
        .await
        .expect("reconnected session must return online");

    asm.dispose().await;
}

// ---------------------------------------------------------------------------
// 5. JSON-RPC API, include_secret origin guard, testProxy, & preferences/proxy stream
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn rpc_secret_guard_test_proxy_and_stream_notifications() {
    let mail = FakeMail::start().await.unwrap();
    let socks_auth =
        start_mock_socks5_proxy(Some(("u1".into(), "s3cr3t!".into())), None).await;

    let dir = secure_dir();
    let sys_snap = Arc::new(Mutex::new(SystemProxySnapshot {
        http: Some(ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "10.0.0.1".into(),
            port: 8080,
            username: Some("sysuser".into()),
            password: Some("syspass".into()),
        }),
        source: "test_os".into(),
        ..Default::default()
    }));
    let sys_reader = Arc::clone(&sys_snap);
    let proxy_service = Arc::new(ProxyService::open_with_system(
        dir.path().join(PREFERENCE_FILE),
        move || sys_reader.lock().unwrap().clone(),
    ));
    proxy_service.set_allow_loopback_proxy(true);

    let ctx = Context::new();
    ctx.provide::<ProxyService>(Arc::clone(&proxy_service));

    // Start Unix socket server so we can verify real stream IPC notifications + include_secret.
    let sock_path = dir.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let sock_ctx = ctx.clone();
    let server_task = tokio::spawn(async move {
        let _ = serve_unix_socket(listener, sock_ctx, async move {
            let _ = stop_rx.changed().await;
        })
        .await;
    });

    let endpoint = Endpoint::Unix {
        path: sock_path.clone(),
    };
    let unix_stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    let mut stream = StreamConnection::from_stream(unix_stream);
    let client = RpcClient::new(endpoint).unwrap();

    // Set custom proxy with password via RPC
    let saved = client
        .set_proxy_config(&maild_client::ProxyConfigUpdate::custom(
            maild_client::CustomProxyUpdate {
                protocol: "socks5h".into(),
                host: "127.0.0.1".into(),
                port: socks_auth.addr.port(),
                username: Some("u1".into()),
                password: Some("s3cr3t!".into()),
                clear_password: false,
                bypass: vec![],
            },
        ))
        .await
        .unwrap();
    assert!(saved.persisted);
    assert!(saved.has_password);
    assert_eq!(saved.config.custom.as_ref().unwrap().password, None);
    assert_eq!(saved.effective_system.http.as_ref().unwrap().password, None);

    // Stream receives `preferences/proxy` notification with generation=1 and no password!
    let incoming = tokio::time::timeout(Duration::from_secs(3), stream.next_incoming())
        .await
        .expect("expected preferences/proxy push")
        .unwrap();
    let maild_client::Incoming::Notification(notif) = incoming else {
        panic!("expected notification frame");
    };
    let ServerNotification::ProxyPreference(event) = notif.into_server_notification().unwrap() else {
        panic!("expected ProxyPreference notification");
    };
    assert_eq!(event.generation, 1);
    assert!(event.has_password);
    assert_eq!(event.config.custom.as_ref().unwrap().password, None);

    // Local Unix IPC allows `include_secret: true`
    let with_secret = client.proxy_config_with_secret().await.unwrap();
    assert_eq!(
        with_secret.config.custom.as_ref().unwrap().password.as_deref(),
        Some("s3cr3t!")
    );

    // HTTP origin MUST reject `include_secret: true`!
    let http_err = dispatch_with_origin(
        &ctx,
        RequestOrigin::Http,
        &req("getProxyConfig", json!({ "include_secret": true }), 2),
    )
    .await;
    assert_eq!(http_err.error.as_ref().map(|e| e.code), Some(-32600));

    // HTTP origin without include_secret succeeds without exposing passwords
    let http_ok = dispatch_with_origin(
        &ctx,
        RequestOrigin::Http,
        &req("getProxyConfig", json!({}), 3),
    )
    .await;
    let http_val = http_ok.result.unwrap();
    assert_eq!(http_val["has_password"], json!(true));
    assert!(http_val["config"]["custom"].get("password").is_none());

    // testProxy: active saved config succeeds
    let target = format!("127.0.0.1:{}", mail.imap_addr.port());
    let ok_res = client
        .test_proxy(&maild_client::TestProxyRequest {
            target: Some(target.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(ok_res.ok, "testProxy should succeed: {ok_res:?}");
    assert_eq!(ok_res.stage, "ready");
    assert!(ok_res.route.starts_with("socks5h://u1@127.0.0.1:"));
    assert!(!ok_res.route.contains("s3cr3t!"));

    // testProxy: unsaved draft WITHOUT use_saved_password fails auth
    let fail_res = client
        .test_proxy(&maild_client::TestProxyRequest {
            config: Some(maild_client::ProxyConfigUpdate::custom(
                maild_client::CustomProxyUpdate {
                    protocol: "socks5h".into(),
                    host: "127.0.0.1".into(),
                    port: socks_auth.addr.port(),
                    username: Some("u1".into()),
                    password: None,
                    clear_password: false,
                    bypass: vec![],
                },
            )),
            use_saved_password: false,
            target: Some(target.clone()),
            scheme: None,
        })
        .await
        .unwrap();
    assert!(!fail_res.ok);
    assert_eq!(fail_res.stage, "proxy_auth");
    assert_eq!(fail_res.detail_code.as_deref(), Some("proxy_auth_failed"));

    // testProxy: same unsaved draft WITH use_saved_password: true succeeds!
    let draft_ok = client
        .test_proxy(&maild_client::TestProxyRequest {
            config: Some(maild_client::ProxyConfigUpdate::custom(
                maild_client::CustomProxyUpdate {
                    protocol: "socks5h".into(),
                    host: "127.0.0.1".into(),
                    port: socks_auth.addr.port(),
                    username: Some("u1".into()),
                    password: None,
                    clear_password: false,
                    bypass: vec![],
                },
            )),
            use_saved_password: true,
            target: Some(target),
            scheme: None,
        })
        .await
        .unwrap();
    assert!(draft_ok.ok, "draft with use_saved_password must succeed: {draft_ok:?}");

    // Simulate OS system proxy change -> bumps generation & emits `preferences/proxy`
    sys_snap.lock().unwrap().http.as_mut().unwrap().port = 9090;
    assert!(proxy_service.refresh_system_proxy(Some(&ctx)));
    let incoming2 = tokio::time::timeout(Duration::from_secs(3), stream.next_incoming())
        .await
        .expect("expected second preferences/proxy push")
        .unwrap();
    let maild_client::Incoming::Notification(notif2) = incoming2 else {
        panic!("expected notification frame");
    };
    let ServerNotification::ProxyPreference(event2) = notif2.into_server_notification().unwrap() else {
        panic!("expected ProxyPreference notification");
    };
    assert_eq!(event2.generation, 2);
    assert_eq!(event2.effective_system.http.as_ref().unwrap().port, 9090);
    assert_eq!(event2.effective_system.http.as_ref().unwrap().password, None);

    let _ = stop_tx.send(true);
    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// 6. Fail-closed corrupt config & maild-client HTTP loopback bypass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn corrupt_proxy_preference_fails_closed_without_overwriting_disk() {
    let dir = secure_dir();
    let app_paths = AppPaths::from_roots(
        &dir.path().join("config"),
        &dir.path().join("data"),
        Some(&dir.path().join("runtime")),
        Some(&dir.path().join("state")),
        dir.path(),
    );
    std::fs::create_dir_all(&app_paths.config_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&app_paths.config_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let pref_path = app_paths.config_dir.join(PREFERENCE_FILE);
    let corrupt_body = "format_version = 99\nmode = \"custom\"\nfuture_flag = true\n";
    std::fs::write(&pref_path, corrupt_body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&pref_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let ctx = Context::new();
    let mut asm = assemble_with_paths(&ctx, "", &app_paths, None).unwrap();

    let get_resp = dispatch(&ctx, &req("getProxyConfig", json!({}), 1)).await;
    let val = get_resp.result.unwrap();
    assert_eq!(val["persisted"], json!(false));
    assert_eq!(val["config"]["mode"], json!("system"));

    let set_resp = dispatch(&ctx, &req("setProxyConfig", json!({ "mode": "direct" }), 2)).await;
    assert_eq!(set_resp.error.as_ref().map(|e| e.code), Some(-32001));
    assert_eq!(
        std::fs::read_to_string(&pref_path).unwrap(),
        corrupt_body,
        "corrupt proxy_preference.toml must never be overwritten"
    );

    asm.dispose().await;
}

#[tokio::test]
async fn maild_client_http_loopback_rpc_bypasses_env_proxy() {
    // Start a mock HTTP proxy that counts hits and refuses everything via 407.
    let trap_proxy = start_mock_http_proxy(Some(("never".into(), "match".into())), false, None).await;
    let proxy_url = format!("http://127.0.0.1:{}", trap_proxy.addr.port());

    let prev_http = std::env::var_os("HTTP_PROXY");
    let prev_all = std::env::var_os("ALL_PROXY");
    std::env::set_var("HTTP_PROXY", &proxy_url);
    std::env::set_var("ALL_PROXY", &proxy_url);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = listener.local_addr().unwrap();

    let token = "a".repeat(32);
    let ctx = Context::new();
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let token_clone = token.clone();
    let server_task = tokio::spawn(async move {
        let _ = serve_http(listener, ctx, token_clone, async move {
            let _ = stop_rx.changed().await;
        })
        .await;
    });

    let client = RpcClient::new(Endpoint::Http {
        url: format!("http://{api_addr}/rpc"),
        token,
    })
    .unwrap();
    let info = client
        .agent_info()
        .await
        .expect("loopback HTTP RPC must bypass HTTP_PROXY / ALL_PROXY");
    assert!(!info.version.is_empty());
    assert_eq!(
        trap_proxy.hits(),
        0,
        "maild-client HTTP loopback RPC must never route through HTTP_PROXY/ALL_PROXY"
    );

    // Restore env vars
    match prev_http {
        Some(v) => std::env::set_var("HTTP_PROXY", v),
        None => std::env::remove_var("HTTP_PROXY"),
    }
    match prev_all {
        Some(v) => std::env::set_var("ALL_PROXY", v),
        None => std::env::remove_var("ALL_PROXY"),
    }

    let _ = stop_tx.send(true);
    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// 7. Poll-mode hot-reload & testProxy default target from first enabled account
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_session_reconnects_on_proxy_change_and_test_proxy_defaults_to_first_account() {
    let mail = FakeMail::start().await.unwrap();
    let proxy1 = start_mock_socks5_proxy(None, None).await;
    let proxy2 = start_mock_socks5_proxy(None, None).await;

    let dir = secure_dir();
    let app_paths = AppPaths::from_roots(
        &dir.path().join("config"),
        &dir.path().join("data"),
        Some(&dir.path().join("runtime")),
        Some(&dir.path().join("state")),
        dir.path(),
    );
    std::fs::create_dir_all(&app_paths.config_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&app_paths.config_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store.config]
db_path = ""
[plugins.scheduler.config]

[[accounts]]
account = "poll-acc"
server = "127.0.0.1"
port = {}
username = "alice"
password = "secret"
tls = "none"
idle = false
poll_secs = 3600
"#,
        mail.imap_addr.port()
    );
    let mut asm = assemble_with_paths(&ctx, &toml, &app_paths, None).unwrap();
    let proxy_service = ctx.get::<ProxyService>().unwrap();
    proxy_service.set_allow_loopback_proxy(true);

    // Switch to proxy1 -> poll session reconnects via proxy1
    let set1 = dispatch(
        &ctx,
        &req(
            "setProxyConfig",
            json!({
                "mode": "custom",
                "custom": {
                    "protocol": "socks5h",
                    "host": "127.0.0.1",
                    "port": proxy1.addr.port(),
                    "bypass": []
                }
            }),
            1,
        ),
    )
    .await;
    assert!(set1.error.is_none(), "{:?}", set1.error);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while proxy1.hits() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "poll session must reconnect via proxy1");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Switch to proxy2 -> poll session (sleeping on 3600s poll interval) reconnects immediately!
    let set2 = dispatch(
        &ctx,
        &req(
            "setProxyConfig",
            json!({
                "mode": "custom",
                "custom": {
                    "protocol": "socks5h",
                    "host": "127.0.0.1",
                    "port": proxy2.addr.port(),
                    "bypass": []
                }
            }),
            2,
        ),
    )
    .await;
    assert!(set2.error.is_none(), "{:?}", set2.error);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while proxy2.hits() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "poll session must reconnect via proxy2");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // testProxy with omitted `target` automatically uses the first enabled account's IMAP host:port!
    let hits_before = proxy2.hits();
    let test_resp = dispatch(&ctx, &req("testProxy", json!({}), 3)).await;
    let val = test_resp.result.expect("testProxy should succeed with default target");
    assert_eq!(val["ok"], json!(true), "{val}");
    assert_eq!(val["stage"], json!("ready"));
    assert!(proxy2.hits() > hits_before, "testProxy should hit active proxy2");

    asm.dispose().await;
}
