//! Integration tests for `nodeseek-email` proxy routing, live `preferences/proxy` hot-reload,
//! and plugin-level `ProxyOverride` (`Inherit` / `Direct` / `Custom`).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;

#[cfg(unix)]
use maild_client::Endpoint;
use maild_proxy::{
    CustomProxyConfig, ProxyConfig, ProxyMode, ProxyOverride, ProxyProtocol, SystemProxySnapshot,
};
use nodeseek_email::api::ApiClient;
#[cfg(unix)]
use nodeseek_email::provider::{run_with_runtime, ProviderRuntime};
use nodeseek_email::proxy::ProxySnapshot;
#[cfg(unix)]
use nodeseek_email::proxy::{PluginProxySettings, PluginProxyStore};
use nodeseek_email::secret::SecretBuffer;
#[cfg(unix)]
use nodeseek_email::state::StateStore;
#[cfg(unix)]
use serde_json::{json, Value};
#[cfg(unix)]
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, BufReader};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

fn fake_secret() -> SecretBuffer {
    let mut key = ["sk", "live", ""].join("_").into_bytes();
    key.extend(std::iter::repeat_n(b'a', 64));
    SecretBuffer::from_bytes(key).unwrap()
}

struct MockProxy {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
}

impl MockProxy {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for MockProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn start_mock_http_proxy(expected_auth_header: Option<String>) -> MockProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let hits_clone = Arc::clone(&hits);

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            let hits = Arc::clone(&hits_clone);
            let expected_auth = expected_auth_header.clone();
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
                let text = String::from_utf8_lossy(&buf);
                let mut lines = text.split("\r\n");
                let first = lines.next().unwrap_or("");
                let mut parts = first.split_whitespace();
                let method = parts.next().unwrap_or("");
                let uri = parts.next().unwrap_or("");
                let version = parts.next().unwrap_or("HTTP/1.1");
                let headers: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();

                if let Some(expected) = expected_auth {
                    let ok = headers.iter().any(|l| {
                        l.split_once(':').is_some_and(|(k, v)| {
                            k.eq_ignore_ascii_case("proxy-authorization")
                                && v.trim().eq_ignore_ascii_case(&expected)
                        })
                    });
                    if !ok {
                        let _ = stream
                            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                }

                let Ok(parsed) = url::Url::parse(uri) else {
                    let _ = stream
                        .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    return;
                };
                let target_host = parsed.host_str().unwrap_or("127.0.0.1");
                let target_port = parsed.port_or_known_default().unwrap_or(80);
                let Ok(mut upstream) = TcpStream::connect((target_host, target_port)).await else {
                    let _ = stream
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    return;
                };

                hits.fetch_add(1, Ordering::SeqCst);

                let mut origin_path = parsed.path().to_string();
                if let Some(q) = parsed.query() {
                    origin_path.push('?');
                    origin_path.push_str(q);
                }
                let mut forwarded = format!("{method} {origin_path} {version}\r\n");
                for h in headers {
                    if let Some((k, _)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("proxy-authorization")
                            || k.eq_ignore_ascii_case("proxy-connection")
                        {
                            continue;
                        }
                    }
                    forwarded.push_str(h);
                    forwarded.push_str("\r\n");
                }
                forwarded.push_str("Connection: close\r\n\r\n");
                if upstream.write_all(forwarded.as_bytes()).await.is_ok() {
                    let _ = upstream.flush().await;
                    let _ = tokio::io::copy(&mut upstream, &mut stream).await;
                }
            });
        }
    });

    MockProxy {
        addr,
        hits,
        shutdown,
    }
}

async fn start_mock_socks5_proxy(expected_auth: Option<(String, String)>) -> MockProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let hits_clone = Arc::clone(&hits);

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            let hits = Arc::clone(&hits_clone);
            let expected_auth = expected_auth.clone();
            tokio::spawn(async move {
                let mut hdr = [0u8; 2];
                if stream.read_exact(&mut hdr).await.is_err() || hdr[0] != 0x05 {
                    return;
                }
                let mut methods = vec![0u8; hdr[1] as usize];
                if stream.read_exact(&mut methods).await.is_err() {
                    return;
                }
                if let Some((u, p)) = expected_auth {
                    if !methods.contains(&0x02) {
                        let _ = stream.write_all(&[0x05, 0xFF]).await;
                        return;
                    }
                    let _ = stream.write_all(&[0x05, 0x02]).await;
                    let mut ah = [0u8; 2];
                    if stream.read_exact(&mut ah).await.is_err() {
                        return;
                    }
                    let mut user = vec![0u8; ah[1] as usize];
                    let _ = stream.read_exact(&mut user).await;
                    let mut plen = [0u8; 1];
                    let _ = stream.read_exact(&mut plen).await;
                    let mut pass = vec![0u8; plen[0] as usize];
                    let _ = stream.read_exact(&mut pass).await;
                    if user != u.as_bytes() || pass != p.as_bytes() {
                        let _ = stream.write_all(&[0x01, 0x01]).await;
                        return;
                    }
                    let _ = stream.write_all(&[0x01, 0x00]).await;
                } else {
                    let _ = stream.write_all(&[0x05, 0x00]).await;
                }

                let mut req = [0u8; 4];
                if stream.read_exact(&mut req).await.is_err() {
                    return;
                }
                let host = match req[3] {
                    0x01 => {
                        let mut ip = [0u8; 4];
                        let _ = stream.read_exact(&mut ip).await;
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    0x03 => {
                        let mut len = [0u8; 1];
                        let _ = stream.read_exact(&mut len).await;
                        let mut dom = vec![0u8; len[0] as usize];
                        let _ = stream.read_exact(&mut dom).await;
                        String::from_utf8_lossy(&dom).into_owned()
                    }
                    0x04 => {
                        let mut ip = [0u8; 16];
                        let _ = stream.read_exact(&mut ip).await;
                        std::net::Ipv6Addr::from(ip).to_string()
                    }
                    _ => return,
                };
                let mut port_buf = [0u8; 2];
                let _ = stream.read_exact(&mut port_buf).await;
                let port = u16::from_be_bytes(port_buf);

                let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
                    let _ = stream
                        .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    return;
                };
                hits.fetch_add(1, Ordering::SeqCst);
                let _ = stream
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                    .await;
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
            });
        }
    });

    MockProxy {
        addr,
        hits,
        shutdown,
    }
}

async fn start_mock_nodeseek_api() -> (String, Arc<AtomicUsize>, watch::Sender<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let hits_clone = Arc::clone(&hits);

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                res = listener.accept() => res,
                _ = shutdown_rx.changed() => break,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            let hits = Arc::clone(&hits_clone);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0u8; 1024];
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let req_text = String::from_utf8_lossy(&buf);
                let target = req_text
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("");
                let body: &[u8] = if target.starts_with("/openapi/v1/mailboxes/") {
                    br#"{"data":{"id":41,"address":"box@seek.li","sendDisabled":false}}"#
                } else {
                    br#"{"data":[{"id":41,"address":"box@seek.li","sendDisabled":false}]}"#
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body).await;
            });
        }
    });

    (format!("http://{addr}/openapi/v1"), hits, shutdown)
}

#[tokio::test]
async fn api_client_routes_via_http_and_socks5_and_honors_plugin_overrides() {
    let (origin, api_hits, _api_stop) = start_mock_nodeseek_api().await;
    // Basic base64("nsuser:nspass") = "bnN1c2VyOm5zcGFzcw=="
    let http_proxy = start_mock_http_proxy(Some("Basic bnN1c2VyOm5zcGFzcw==".into())).await;
    let socks_proxy = start_mock_socks5_proxy(Some(("su".into(), "sp".into()))).await;

    // 1. Inherit global HTTP proxy with auth
    let global_http = ProxyConfig {
        mode: ProxyMode::Custom,
        custom: Some(CustomProxyConfig {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: http_proxy.addr.port(),
            username: Some("nsuser".into()),
            password: Some("nspass".into()),
            bypass: vec![],
        }),
    };
    let snap = ProxySnapshot::from_global(global_http.clone(), SystemProxySnapshot::default())
        .with_allow_loopback_proxy(true);
    let client = ApiClient::for_test_with_proxy(fake_secret(), &origin, snap).unwrap();

    let boxes = client.list_mailboxes().await.unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(http_proxy.hits(), 1);
    assert_eq!(socks_proxy.hits(), 0);
    assert_eq!(api_hits.load(Ordering::SeqCst), 1);

    // 2. Plugin-level override: Direct beats global HTTP proxy
    client.update_plugin_override(ProxyOverride::Direct).unwrap();
    let boxes = client.list_mailboxes().await.unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(http_proxy.hits(), 1, "Direct override must bypass global proxy");
    assert_eq!(api_hits.load(Ordering::SeqCst), 2);

    // 3. Plugin-level override: Custom SOCKS5h beats global HTTP proxy
    client
        .update_plugin_override(ProxyOverride::Custom(CustomProxyConfig {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: socks_proxy.addr.port(),
            username: Some("su".into()),
            password: Some("sp".into()),
            bypass: vec![],
        }))
        .unwrap();
    let boxes = client.list_mailboxes().await.unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(http_proxy.hits(), 1);
    assert_eq!(socks_proxy.hits(), 1);
    assert_eq!(api_hits.load(Ordering::SeqCst), 3);

    // 4. Resetting plugin override back to Inherit resumes following global proxy
    client.update_plugin_override(ProxyOverride::Inherit).unwrap();
    let boxes = client.list_mailboxes().await.unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(http_proxy.hits(), 2);
    assert_eq!(socks_proxy.hits(), 1);
}

#[cfg(unix)]
async fn read_json_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).await.unwrap() > 0);
    serde_json::from_str(&line).unwrap()
}

#[cfg(unix)]
async fn write_json_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) {
    writer
        .write_all(value.to_string().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn provider_hot_switches_on_preferences_proxy_and_respects_local_override_file() {
    let (origin, _api_hits, _api_stop) = start_mock_nodeseek_api().await;
    let proxy_a = start_mock_http_proxy(None).await;
    let proxy_b = start_mock_socks5_proxy(Some(("u2".into(), "p2".into()))).await;
    let proxy_plugin = start_mock_socks5_proxy(None).await;

    let root = tempfile::tempdir().unwrap();
    let plugin_dir = root.path().join("state");
    let state = StateStore::at_root(plugin_dir.clone()).unwrap();
    let proxy_store = PluginProxyStore::at_root(plugin_dir).unwrap();

    let api = ApiClient::for_test_with_proxy(
        fake_secret(),
        &origin,
        ProxySnapshot::default_system().with_allow_loopback_proxy(true),
    )
    .unwrap();
    let runtime = ProviderRuntime::for_test(api, state, Some(proxy_store.clone()));

    let socket = root.path().join("mail.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();

    let proxy_a_port = proxy_a.addr.port();
    let proxy_b_port = proxy_b.addr.port();
    let proxy_plugin_port = proxy_plugin.addr.port();
    let store_for_ipc = proxy_store.clone();
    let observed_probes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let probes_clone = Arc::clone(&observed_probes);

    let ipc_server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut read = BufReader::new(read);

        // 1. authenticate
        let auth = read_json_line(&mut read).await;
        assert_eq!(auth["method"], "authenticate");
        write_json_line(&mut write, &json!({"jsonrpc":"2.0","id":auth["id"],"result":{}})).await;

        // 2. providerRegister
        let reg = read_json_line(&mut read).await;
        assert_eq!(reg["method"], "providerRegister");
        write_json_line(&mut write, &json!({"jsonrpc":"2.0","id":reg["id"],"result":{}})).await;

        // 3. Startup getProxyConfig { include_secret: true } -> proxy A (HTTP, no password)
        let get1 = read_json_line(&mut read).await;
        assert_eq!(get1["method"], "getProxyConfig");
        assert_eq!(get1["params"]["include_secret"], true);
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0",
                "id":get1["id"],
                "result":{
                    "config":{
                        "mode":"custom",
                        "custom":{"protocol":"http","host":"127.0.0.1","port":proxy_a_port,"bypass":[]}
                    },
                    "has_password":false,
                    "effective_system":{"bypass":[],"exclude_simple_hostnames":false,"source":"none"},
                    "persisted":true
                }
            }),
        )
        .await;

        // Probe 1 -> goes through Proxy A
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0","id":"p1","method":"transport/probe",
                "params":{"account":"","address":"box@seek.li","direction":"incoming"}
            }),
        )
        .await;
        let resp1 = read_json_line(&mut read).await;
        probes_clone.lock().unwrap().push(resp1);

        // 4. Push `preferences/proxy` switching to authenticated SOCKS5h Proxy B (has_password = true)
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0",
                "method":"preferences/proxy",
                "params":{
                    "config":{
                        "mode":"custom",
                        "custom":{"protocol":"socks5h","host":"127.0.0.1","port":proxy_b_port,"username":"u2","bypass":[]}
                    },
                    "has_password":true,
                    "effective_system":{"bypass":[],"exclude_simple_hostnames":false,"source":"none"},
                    "persisted":true,
                    "generation":2
                }
            }),
        )
        .await;

        // Provider must follow up with `getProxyConfig { include_secret: true }` to get password!
        let get2 = read_json_line(&mut read).await;
        assert_eq!(get2["method"], "getProxyConfig");
        assert_eq!(get2["params"]["include_secret"], true);
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0",
                "id":get2["id"],
                "result":{
                    "config":{
                        "mode":"custom",
                        "custom":{
                            "protocol":"socks5h","host":"127.0.0.1","port":proxy_b_port,
                            "username":"u2","password":"p2","bypass":[]
                        }
                    },
                    "has_password":true,
                    "effective_system":{"bypass":[],"exclude_simple_hostnames":false,"source":"none"},
                    "persisted":true
                }
            }),
        )
        .await;

        // Probe 2 -> goes through authenticated SOCKS5h Proxy B!
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0","id":"p2","method":"transport/probe",
                "params":{"account":"","address":"box@seek.li","direction":"incoming"}
            }),
        )
        .await;
        let resp2 = read_json_line(&mut read).await;
        probes_clone.lock().unwrap().push(resp2);

        // 5. Simulate user saving plugin-level Custom override in UI/CLI while provider is running!
        store_for_ipc
            .save(&PluginProxySettings::from_override(ProxyOverride::Custom(
                CustomProxyConfig {
                    protocol: ProxyProtocol::Socks5h,
                    host: "127.0.0.1".into(),
                    port: proxy_plugin_port,
                    username: None,
                    password: None,
                    bypass: vec![],
                },
            )))
            .unwrap();

        // Probe 3 -> must automatically pick up plugin override proxy_plugin over global Proxy B!
        write_json_line(
            &mut write,
            &json!({
                "jsonrpc":"2.0","id":"p3","method":"transport/probe",
                "params":{"account":"","address":"box@seek.li","direction":"incoming"}
            }),
        )
        .await;
        let resp3 = read_json_line(&mut read).await;
        probes_clone.lock().unwrap().push(resp3);
    });

    let provider = tokio::spawn(run_with_runtime(
        runtime,
        Endpoint::Unix { path: socket },
        "mdp_test_token",
    ));

    ipc_server.await.unwrap();
    let _ = provider.await.unwrap();

    let probes = observed_probes.lock().unwrap();
    assert_eq!(probes.len(), 3);
    assert_eq!(probes[0]["result"]["ok"], true);
    assert_eq!(probes[1]["result"]["ok"], true);
    assert_eq!(probes[2]["result"]["ok"], true);

    assert_eq!(proxy_a.hits(), 1, "Probe 1 must use startup Proxy A");
    assert_eq!(
        proxy_b.hits(),
        1,
        "Probe 2 must hot-reload to authenticated Proxy B via preferences/proxy + getProxyConfig"
    );
    assert_eq!(
        proxy_plugin.hits(),
        1,
        "Probe 3 must honor plugin-level Custom override over global Proxy B"
    );
}
