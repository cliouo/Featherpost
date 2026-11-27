//! Hermetic integration tests for Phase 3 WebView remote images via `Route::Remote` and
//! `RemoteImageFetcher` backed by `maild-proxy`.
//!
//! Verifies:
//! - `allow_remote = true` rewrites 100% of remote image URLs into `/remote/<token>/<idx>` with
//!   permanently locked CSP (`img-src <csp_source> data:`) and routes 100% of fetches through the
//!   configured HTTP / SOCKS5h mock proxy (with zero direct WebView hits).
//! - `allow_remote = false` blocks all remote fetches (zero proxy/origin hits).
//! - Redirect chains up to 5 hops succeed with `Referrer-Policy: no-referrer` and no `Referer`;
//!   more than 5 redirects are rejected; SSRF redirect targets (`127.0.0.1`, `localhost`,
//!   `169.254.169.254`) are rejected before dialing.
//! - Non-image `Content-Type` and >10 MiB bodies are rejected.
//! - `RemoteImageFetcher` hot-reloads atomically via `getProxyConfig { include_secret: true }`
//!   and `preferences/proxy` push notifications from a real `maild` daemon.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

use base64::Engine as _;
#[cfg(unix)]
use maild::events::MailMessage;
#[cfg(unix)]
use maild::folders::FolderRole;
#[cfg(unix)]
use maild::store::Store;
#[cfg(unix)]
use maild::{run_daemon_until, DaemonOptions};
#[cfg(unix)]
use maild_client::Endpoint;
#[cfg(unix)]
use maild_paths::AppPaths;
use maild_proxy::{
    CustomProxyConfig, ProxyConfig, ProxyMode, ProxyProtocol, SystemProxySnapshot,
};
use maild_ui::documents::{
    remote_image_response, route, Document, DocumentIdentity, DocumentStore, ProtocolBase, Route,
};
use maild_ui::remote_image::{
    RemoteImageError, RemoteImageFetcher, MAX_REMOTE_IMAGE_BYTES,
};
#[cfg(unix)]
use maild_ui::settings::{UiCustomProxyUpdate, UiProxyConfigUpdate};
#[cfg(unix)]
use maild_ui::{Bridge, FRONTEND_ID};
use tauri::http::{header, StatusCode};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// Minimal 1x1 PNG payload
// ---------------------------------------------------------------------------
const TINY_PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
    b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
    0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0x00,
    0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, b'I',
    b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
];

// ---------------------------------------------------------------------------
// Local mock HTTP proxy (supports both absolute-URI GET and CONNECT) & SOCKS5h proxy
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

async fn start_mock_http_proxy(
    expected_auth: Option<(String, String)>,
    rewrite_target: SocketAddr,
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
                let _ = handle_http_proxy(tcp, expected_auth.as_ref(), rewrite_target, hits, targets)
                    .await;
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

async fn handle_http_proxy(
    mut stream: TcpStream,
    expected_auth: Option<&(String, String)>,
    rewrite_target: SocketAddr,
    hits: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.len() > 16384 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let uri = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("HTTP/1.1");

    let header_lines: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    if let Some((user, pass)) = expected_auth {
        let expected_b64 =
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        let expected_hdr = format!("basic {expected_b64}");
        let authorized = header_lines.iter().any(|line| {
            line.split_once(':').is_some_and(|(k, v)| {
                k.eq_ignore_ascii_case("proxy-authorization")
                    && v.trim().eq_ignore_ascii_case(&expected_hdr)
            })
        });
        if !authorized {
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
            return Ok(());
        }
    }

    hits.fetch_add(1, Ordering::SeqCst);
    targets.lock().unwrap().push(uri.to_string());

    let mut upstream = TcpStream::connect(rewrite_target).await?;
    if method == "CONNECT" {
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        stream.flush().await?;
        let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        return Ok(());
    }

    // Plain HTTP proxy forwarding: rewrite absolute URI (`http://host/path`) to origin-form `/path`
    // and strip `Proxy-Authorization` before forwarding upstream.
    let origin_path = url::Url::parse(uri)
        .map(|u| {
            let mut p = u.path().to_string();
            if let Some(q) = u.query() {
                p.push('?');
                p.push_str(q);
            }
            p
        })
        .unwrap_or_else(|_| uri.to_string());
    let mut forwarded = format!("{method} {origin_path} {version}\r\n");
    for line in header_lines {
        if let Some((k, _)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("proxy-authorization")
                || k.eq_ignore_ascii_case("proxy-connection")
            {
                continue;
            }
        }
        forwarded.push_str(line);
        forwarded.push_str("\r\n");
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    upstream.write_all(forwarded.as_bytes()).await?;
    upstream.flush().await?;
    let _ = tokio::io::copy(&mut upstream, &mut stream).await;
    Ok(())
}

async fn start_mock_socks5_proxy(
    expected_auth: Option<(String, String)>,
    rewrite_target: SocketAddr,
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
                let _ =
                    handle_socks5(tcp, expected_auth.as_ref(), rewrite_target, hits, targets).await;
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
    rewrite_target: SocketAddr,
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
        let mut user = vec![0u8; auth_hdr[1] as usize];
        stream.read_exact(&mut user).await?;
        let mut plen = [0u8; 1];
        stream.read_exact(&mut plen).await?;
        let mut pass = vec![0u8; plen[0] as usize];
        stream.read_exact(&mut pass).await?;
        if user != expected_u.as_bytes() || pass != expected_p.as_bytes() {
            stream.write_all(&[0x01, 0x01]).await?;
            return Ok(());
        }
        stream.write_all(&[0x01, 0x00]).await?;
    } else {
        stream.write_all(&[0x05, 0x00]).await?;
    }

    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    let host = match req[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8_lossy(&domain).into_owned()
        }
        0x04 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => return Ok(()),
    };
    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await?;
    let port = u16::from_be_bytes(port_buf);

    hits.fetch_add(1, Ordering::SeqCst);
    targets.lock().unwrap().push(format!("{host}:{port}"));

    let mut upstream = TcpStream::connect(rewrite_target).await?;
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Local mock upstream image server
// ---------------------------------------------------------------------------

struct MockOrigin {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: watch::Sender<bool>,
}

impl MockOrigin {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockOrigin {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn start_mock_image_origin(ssrf_trap_port: u16) -> MockOrigin {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (shutdown, mut shutdown_rx) = watch::channel(false);

    let reqs_clone = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            let reqs = Arc::clone(&reqs_clone);
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(512);
                let mut byte = [0u8; 1];
                loop {
                    let Ok(n) = stream.read(&mut byte).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.push(byte[0]);
                    if buf.ends_with(b"\r\n\r\n") || buf.len() > 16384 {
                        break;
                    }
                }
                let raw = String::from_utf8_lossy(&buf).into_owned();
                reqs.lock().unwrap().push(raw.clone());
                let path = raw
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");

                match path {
                    "/ok.png" | "/final.png" => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            TINY_PNG.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(TINY_PNG).await;
                    }
                    "/html" => {
                        let body = b"<html><body>not an image</body></html>";
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(body).await;
                    }
                    "/huge-cl" => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            MAX_REMOTE_IMAGE_BYTES + 1
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                    }
                    "/huge-stream" => {
                        // No Content-Length header: stream >10 MiB to test chunked enforcement.
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nConnection: close\r\n\r\n";
                        let _ = stream.write_all(head.as_bytes()).await;
                        let chunk = vec![0xAAu8; 256 * 1024];
                        for _ in 0..42 {
                            if stream.write_all(&chunk).await.is_err() {
                                break;
                            }
                        }
                    }
                    "/redir-ok/1" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://cdn2.example.test/redir-ok/2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    }
                    "/redir-ok/2" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    }
                    "/redir-loop" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 302 Found\r\nLocation: /redir-loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    }
                    "/redir-ssrf-loopback" => {
                        let resp = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{ssrf_trap_port}/secret.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                    }
                    "/redir-ssrf-metadata" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    }
                    _ => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    }
                }
            });
        }
    });

    MockOrigin {
        addr,
        requests,
        shutdown,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_remote_fetches_through_proxy_enforces_guards_and_respects_allow_remote() {
    // Trap listener on loopback: SSRF redirects MUST NEVER reach it.
    let trap_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let trap_port = trap_listener.local_addr().unwrap().port();
    let trap_hits = Arc::new(AtomicUsize::new(0));
    let trap_hits_clone = Arc::clone(&trap_hits);
    let trap_task = tokio::spawn(async move {
        while let Ok((mut s, _)) = trap_listener.accept().await {
            trap_hits_clone.fetch_add(1, Ordering::SeqCst);
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 1\r\n\r\nx")
                .await;
        }
    });

    let origin = start_mock_image_origin(trap_port).await;
    let proxy = start_mock_http_proxy(
        Some(("imguser".into(), "imgpass".into())),
        origin.addr,
    )
    .await;

    let fetcher = RemoteImageFetcher::with_proxy_config(
        ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(CustomProxyConfig {
                protocol: ProxyProtocol::Http,
                host: "127.0.0.1".into(),
                port: proxy.addr.port(),
                username: Some("imguser".into()),
                password: Some("imgpass".into()),
                bypass: vec![],
            }),
        },
        SystemProxySnapshot::default(),
    );

    let store = DocumentStore::new();
    let base = ProtocolBase::for_platform();
    let token_blocked = maild_ui::documents::new_token();
    let token_allowed = maild_ui::documents::new_token();

    let raw_html = concat!(
        r#"<html><body>"#,
        r#"<img src="http://cdn.example.test/ok.png">"#,
        r#"<img src="http://cdn.example.test/redir-ok/1">"#,
        r#"<img src="http://cdn.example.test/html">"#,
        r#"<img src="http://cdn.example.test/huge-cl">"#,
        r#"<img src="http://cdn.example.test/huge-stream">"#,
        r#"<img src="http://cdn.example.test/redir-loop">"#,
        r#"<img src="http://cdn.example.test/redir-ssrf-loopback">"#,
        r#"<img src="http://cdn.example.test/redir-ssrf-metadata">"#,
        r#"</body></html>"#,
    );

    // 1. allow_remote = false -> zero remote_urls, zero network requests even if manually probed.
    let prepared_blocked = maild_ui::html::prepare(maild_ui::html::PrepareInput {
        html: raw_html,
        token: &token_blocked,
        part_url_prefix: &base.url_prefix,
        csp_source: &base.csp_source,
        cid_parts: &Default::default(),
        allow_remote: false,
    })
    .unwrap();
    assert!(prepared_blocked.remote_urls.is_empty());
    assert!(!prepared_blocked.csp.contains("https:"));
    assert!(!prepared_blocked.csp.contains("http:"));
    store.insert(
        token_blocked.clone(),
        Document {
            identity: DocumentIdentity {
                account: "a".into(),
                folder: "INBOX".into(),
                uid: "1".into(),
            },
            subject: String::new(),
            html: prepared_blocked.html,
            csp: prepared_blocked.csp,
            links: prepared_blocked.links,
            parts: Default::default(),
            remote_urls: vec!["http://cdn.example.test/ok.png".into()], // even if injected!
            allow_remote: false,
            translation: std::sync::Arc::new(std::sync::RwLock::new(None)),
            translation_rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active_job: std::sync::Arc::new(std::sync::Mutex::new(None)),
        },
    );
    assert_eq!(
        fetcher.serve_remote(&store, &token_blocked, 0).await,
        Err(RemoteImageError::Blocked)
    );
    assert_eq!(proxy.hits(), 0, "allow_remote=false must make zero requests");

    // 2. allow_remote = true -> rewritten to /remote/<token>/<idx>, locked CSP, fetched via proxy.
    let prepared_allowed = maild_ui::html::prepare(maild_ui::html::PrepareInput {
        html: raw_html,
        token: &token_allowed,
        part_url_prefix: &base.url_prefix,
        csp_source: &base.csp_source,
        cid_parts: &Default::default(),
        allow_remote: true,
    })
    .unwrap();
    assert_eq!(prepared_allowed.remote_urls.len(), 8);
    assert!(!prepared_allowed.html.contains("cdn.example.test"));
    assert!(!prepared_allowed.csp.contains("https:"));
    assert!(!prepared_allowed.csp.contains("http:"));
    assert!(prepared_allowed.html.contains(&format!(
        "{}/remote/{token_allowed}/0",
        base.url_prefix
    )));
    assert_eq!(
        route(&format!("/remote/{token_allowed}/0")),
        Route::Remote(token_allowed.clone(), 0)
    );

    store.insert(
        token_allowed.clone(),
        Document {
            identity: DocumentIdentity {
                account: "a".into(),
                folder: "INBOX".into(),
                uid: "2".into(),
            },
            subject: String::new(),
            html: prepared_allowed.html,
            csp: prepared_allowed.csp,
            links: prepared_allowed.links,
            parts: Default::default(),
            remote_urls: prepared_allowed.remote_urls,
            allow_remote: true,
            translation: std::sync::Arc::new(std::sync::RwLock::new(None)),
            translation_rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active_job: std::sync::Arc::new(std::sync::Mutex::new(None)),
        },
    );

    // Fetch index 0: valid PNG via authenticated mock HTTP proxy
    let img = fetcher
        .serve_remote(&store, &token_allowed, 0)
        .await
        .expect("valid image must load through proxy");
    assert_eq!(img.content_type, "image/png");
    assert_eq!(img.bytes, TINY_PNG);
    assert_eq!(proxy.hits(), 1);

    // Verify response security headers
    let http_resp = remote_image_response(&img.content_type, img.bytes.clone());
    assert_eq!(http_resp.status(), StatusCode::OK);
    assert_eq!(http_resp.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        http_resp.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(http_resp.headers()[header::CACHE_CONTROL], "no-store");

    // Verify Referrer-Policy: no-referrer was sent and NO Referer header was sent!
    let first_req = origin.requests()[0].to_ascii_lowercase();
    assert!(
        first_req.contains("referrer-policy: no-referrer"),
        "expected Referrer-Policy: no-referrer, got:\n{first_req}"
    );
    assert!(
        !first_req.contains("\r\nreferer:"),
        "must not send Referer header:\n{first_req}"
    );

    // Fetch index 1: valid 2-hop redirect chain -> final PNG
    let redirected = fetcher
        .serve_remote(&store, &token_allowed, 1)
        .await
        .expect("2-hop redirect to public host must succeed");
    assert_eq!(redirected.bytes, TINY_PNG);
    assert_eq!(proxy.hits(), 4); // 1 initial + 3 hops (redir-ok/1 -> redir-ok/2 -> final.png)

    // Fetch index 2: non-image Content-Type (text/html) -> rejected
    assert_eq!(
        fetcher.serve_remote(&store, &token_allowed, 2).await,
        Err(RemoteImageError::NonImageContentType)
    );

    // Fetch index 3: >10 MiB Content-Length -> rejected
    assert_eq!(
        fetcher.serve_remote(&store, &token_allowed, 3).await,
        Err(RemoteImageError::TooLarge)
    );

    // Fetch index 4: >10 MiB streamed without Content-Length -> rejected
    assert_eq!(
        fetcher.serve_remote(&store, &token_allowed, 4).await,
        Err(RemoteImageError::TooLarge)
    );

    // Fetch index 5: >5 redirects -> rejected
    assert_eq!(
        fetcher.serve_remote(&store, &token_allowed, 5).await,
        Err(RemoteImageError::TooManyRedirects)
    );

    // Fetch index 6 & 7: SSRF redirects to loopback and link-local metadata -> blocked BEFORE dialing!
    assert!(matches!(
        fetcher.serve_remote(&store, &token_allowed, 6).await,
        Err(RemoteImageError::SsrfBlocked(_))
    ));
    assert!(matches!(
        fetcher.serve_remote(&store, &token_allowed, 7).await,
        Err(RemoteImageError::SsrfBlocked(_))
    ));
    assert_eq!(
        trap_hits.load(Ordering::SeqCst),
        0,
        "SSRF redirect target must never be dialed"
    );

    trap_task.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn remote_image_fetcher_hot_reloads_from_daemon_ipc_and_proxy_push() {
    let origin = start_mock_image_origin(9).await;
    let http_proxy = start_mock_http_proxy(
        Some(("httpu".into(), "http-secret-1".into())),
        origin.addr,
    )
    .await;
    let socks_proxy = start_mock_socks5_proxy(
        Some(("socksu".into(), "socks-secret-2".into())),
        origin.addr,
    )
    .await;

    let directory = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_roots(
        &directory.path().join("config"),
        &directory.path().join("data"),
        Some(&directory.path().join("runtime")),
        Some(&directory.path().join("state")),
        directory.path(),
    );
    for dir in [
        &paths.config_dir,
        &paths.data_dir,
        &paths.runtime_dir,
        &paths.plugin_dir,
        &paths.frontend_dir,
    ] {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(
        &paths.config_file,
        r#"
        [plugins.store.config]
        db_path = "mail.sqlite3"
        [plugins.api.config]
        transport = "unix"
        listen = "mail.sock"
        [plugins.frontend.config]
        default = "builtin-egui"
        [plugins.external.config]
        "#,
    )
    .unwrap();

    // Install frontend manifest so Bridge authenticates cleanly.
    let package = paths.frontend_dir.join(FRONTEND_ID);
    std::fs::create_dir_all(&package).unwrap();
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("package/frontend.toml"),
    )
    .unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        manifest.replace(maild_brand::BUNDLED_FRONTEND_ID, FRONTEND_ID),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            package.join("frontend.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    // Seed one HTML message with a remote image.
    {
        let store = Store::open(paths.data_dir.join("mail.sqlite3").to_str().unwrap()).unwrap();
        store
            .upsert_folder("demo", "INBOX", "收件箱", Some(FolderRole::Inbox))
            .unwrap();
        let msg = MailMessage {
            account: "demo".into(),
            folder: "INBOX".into(),
            uid: "1".into(),
            sender: "a@example.test".into(),
            recipient: "b@example.test".into(),
            subject: "img".into(),
            body: "hi".into(),
            body_html: r#"<p><img src="http://cdn.example.test/ok.png"></p>"#.into(),
            received_at: 1_757_000_000,
            ..Default::default()
        };
        store.insert_message(&msg).unwrap();
    }

    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let options = DaemonOptions::new(paths.config_file.clone(), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let daemon_task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));

    let socket = paths.runtime_dir.join("mail.sock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(tokio::time::Instant::now() < deadline, "daemon socket timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let bridge = Bridge::spawn(Endpoint::Unix { path: socket }).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while bridge.snapshot().phase != "online" {
        assert!(tokio::time::Instant::now() < deadline, "bridge online timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut events = bridge.subscribe_events();
    let fetcher = RemoteImageFetcher::new();
    let documents = DocumentStore::new();
    let base = ProtocolBase::for_platform();
    let doc = bridge
        .prepare_document(&documents, &base, "demo", "INBOX", "1", true)
        .await
        .unwrap();
    let token = doc.token.unwrap();

    // Step 1: Set global proxy to authenticated HTTP proxy
    bridge
        .set_proxy_config(UiProxyConfigUpdate {
            mode: "custom".into(),
            custom: Some(UiCustomProxyUpdate {
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: http_proxy.addr.port(),
                username: Some("httpu".into()),
                password: Some("http-secret-1".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .await
        .unwrap();

    let ev1 = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(maild_app::AppEvent::Notification(
                maild_client::ServerNotification::ProxyPreference(ev),
            )) = events.recv().await
            {
                return ev;
            }
        }
    })
    .await
    .expect("must receive first preferences/proxy push");
    fetcher.refresh_from_bridge(&bridge, ev1.generation).await;

    let img1 = fetcher.serve_remote(&documents, &token, 0).await.unwrap();
    assert_eq!(img1.bytes, TINY_PNG);
    assert_eq!(http_proxy.hits(), 1);
    assert_eq!(socks_proxy.hits(), 0);

    // Step 2: Live-switch global proxy to authenticated SOCKS5h proxy
    bridge
        .set_proxy_config(UiProxyConfigUpdate {
            mode: "custom".into(),
            custom: Some(UiCustomProxyUpdate {
                protocol: "socks5h".into(),
                host: "127.0.0.1".into(),
                port: socks_proxy.addr.port(),
                username: Some("socksu".into()),
                password: Some("socks-secret-2".into()),
                clear_password: false,
                bypass: vec![],
            }),
            clear_password: false,
        })
        .await
        .unwrap();

    let ev2 = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(maild_app::AppEvent::Notification(
                maild_client::ServerNotification::ProxyPreference(ev),
            )) = events.recv().await
            {
                if ev.generation > ev1.generation {
                    return ev;
                }
            }
        }
    })
    .await
    .expect("must receive second preferences/proxy push");
    fetcher.refresh_from_bridge(&bridge, ev2.generation).await;

    let img2 = fetcher.serve_remote(&documents, &token, 0).await.unwrap();
    assert_eq!(img2.bytes, TINY_PNG);
    assert_eq!(http_proxy.hits(), 1, "old HTTP proxy must not receive new hits");
    assert_eq!(socks_proxy.hits(), 1, "new SOCKS5h proxy must serve image");
    assert_eq!(socks_proxy.targets(), vec!["cdn.example.test:80"]);

    bridge.shutdown().await;
    let _ = shutdown_tx.send(());
    daemon_task.await.unwrap().unwrap();
}
