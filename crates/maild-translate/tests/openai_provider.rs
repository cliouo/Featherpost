use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use maild_proxy::{
    CustomProxyConfig, ProxyConfig, ProxyMode, ProxyProtocol, ResolveOptions, SystemProxySnapshot,
};
use maild_translate::{
    ContentKind, EndpointTransport, NormalizedServiceIdentity, OpenAiCompatibleProvider,
    OpenAiHttpLimits, ProbeStage, ProxySnapshotOutcome, ResolvedOpenAiConfig, TextBlock,
    TranslateError, TranslationBatchRequest, TranslationProvider,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

type HandlerFn = Arc<dyn Fn(String, String) -> (String, Duration) + Send + Sync>;

async fn spawn_mock_http_server(handler: HandlerFn) -> (SocketAddr, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let hits_c = Arc::clone(&hits);
    let bodies_c = Arc::clone(&bodies);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let handler = Arc::clone(&handler);
            let hits = Arc::clone(&hits_c);
            let bodies = Arc::clone(&bodies_c);
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                let mut content_len = 0usize;

                loop {
                    let Ok(n) = stream.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let headers_str = String::from_utf8_lossy(&raw[..pos]);
                            for line in headers_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("content-length") {
                                        content_len = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(end) = header_end {
                        if raw.len() >= end + content_len {
                            break;
                        }
                    }
                }

                let end = header_end.unwrap_or(raw.len());
                let req_headers = String::from_utf8_lossy(&raw[..end]).to_string();
                let req_body = String::from_utf8_lossy(&raw[end..]).to_string();
                hits.fetch_add(1, Ordering::SeqCst);
                bodies.lock().unwrap().push(req_body.clone());

                let (resp, delay) = handler(req_headers, req_body);
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });

    (addr, hits, bodies)
}

fn http_json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn openai_ok_body(items_json: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": { "content": items_json }
        }]
    })
    .to_string()
}

fn loopback_config(addr: SocketAddr, api_key: Option<&str>) -> ResolvedOpenAiConfig {
    ResolvedOpenAiConfig {
        identity: NormalizedServiceIdentity::parse(&format!("http://127.0.0.1:{}/v1", addr.port()))
            .unwrap(),
        model: "qwen2.5:7b".into(),
        api_key: api_key.map(|k| Zeroizing::new(k.to_string())),
        allow_plaintext_lan: false,
        revision: 1,
    }
}

fn sample_request(items: Vec<TextBlock>, deadline: Option<Instant>) -> TranslationBatchRequest {
    TranslationBatchRequest {
        job_id: "job-1".into(),
        batch_index: 0,
        source_lang: Some("en".into()),
        target_lang: "zh-CN".into(),
        content_kind: ContentKind::Plain,
        items,
        deadline,
    }
}

#[test]
fn endpoint_validation_blocks_public_http_userinfo_query_fragment_and_flags_lan() {
    // Public HTTPS allowed.
    let pub_id = NormalizedServiceIdentity::parse("https://api.openai.com/v1").unwrap();
    assert_eq!(pub_id.transport, EndpointTransport::PublicHttps);
    assert_eq!(pub_id.canonical_key(), "https://api.openai.com:443/v1");
    assert_eq!(
        pub_id.chat_completions_url().unwrap().as_str(),
        "https://api.openai.com/v1/chat/completions"
    );

    // Public HTTP forbidden.
    assert!(matches!(
        NormalizedServiceIdentity::parse("http://api.openai.com/v1"),
        Err(TranslateError::EndpointRejected(_))
    ));

    // Userinfo, query secret, fragment, traversal forbidden.
    for bad in [
        "https://user:pass@api.openai.com/v1",
        "https://api.openai.com/v1?api_key=sk-leak",
        "https://api.openai.com/v1#secret",
        "https://api.openai.com/v1/../v2",
        "ftp://api.openai.com/v1",
        "https://0.0.0.0/v1",
    ] {
        assert!(
            matches!(
                NormalizedServiceIdentity::parse(bad),
                Err(TranslateError::EndpointRejected(_))
            ),
            "expected endpoint rejection for {bad}"
        );
    }

    // Loopback HTTP allowed (keyless Ollama).
    let local = NormalizedServiceIdentity::parse("http://127.0.0.1:11434/v1").unwrap();
    assert_eq!(local.transport, EndpointTransport::LoopbackHttp);
    assert!(local.transport.allows_keyless());
    assert!(!local.transport.is_plaintext_lan());

    // Explicit LAN HTTP classified as plaintext LAN transport.
    let lan = NormalizedServiceIdentity::parse("http://192.168.1.42:11434/v1").unwrap();
    assert_eq!(lan.transport, EndpointTransport::LanHttp);
    assert!(lan.transport.is_plaintext_lan());
    assert!(!lan.transport.allows_keyless());
}

#[tokio::test]
async fn first_proxy_snapshot_guard_and_stale_generation_rejection() {
    let (addr, hits, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let body = openai_ok_body(r#"{"items":[{"id":0,"text":"你好"}]}"#);
        (http_json_response("200 OK", &body), Duration::ZERO)
    }))
    .await;

    let provider = OpenAiCompatibleProvider::default();
    provider.set_config(Some(loopback_config(addr, None)));
    assert!(!provider.is_proxy_ready());

    // Before first valid proxy snapshot, BOTH translate_batch and probe must refuse outbound traffic.
    let err = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Hello".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(err, TranslateError::ProxyNotReady);

    let probe = provider.probe("zh-CN").await;
    assert!(!probe.ok);
    assert_eq!(probe.stage, ProbeStage::Proxy);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // Recording a proxy fetch failure must NOT silently fall back to Direct.
    provider.record_proxy_fetch_failure();
    assert!(!provider.is_proxy_ready());

    // Apply initial snapshot (gen = 5).
    assert_eq!(
        provider
            .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 5)
            .unwrap(),
        ProxySnapshotOutcome::Applied { generation: 5 }
    );
    assert!(provider.is_proxy_ready());
    assert_eq!(provider.proxy_generation(), Some(5));

    // Stale generations (<= 5) are rejected.
    assert_eq!(
        provider
            .apply_proxy_snapshot(
                ProxyConfig {
                    mode: ProxyMode::Direct,
                    custom: None,
                },
                SystemProxySnapshot::default(),
                4,
            )
            .unwrap(),
        ProxySnapshotOutcome::IgnoredStale {
            incoming: 4,
            current: 5
        }
    );
    assert_eq!(
        provider
            .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 5)
            .unwrap(),
        ProxySnapshotOutcome::IgnoredStale {
            incoming: 5,
            current: 5
        }
    );
    assert_eq!(provider.proxy_generation(), Some(5));

    // Now loopback Ollama works cleanly!
    let resp = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Hello".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.items.len(), 1);
    assert_eq!(resp.items[0].text, "你好");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn global_proxy_routes_public_via_proxy_while_loopback_stays_direct() {
    // Spawn a mock local Ollama server and a mock HTTP CONNECT proxy.
    let (ollama_addr, ollama_hits, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let body = openai_ok_body(r#"{"items":[{"id":0,"text":"本地译文"}]}"#);
        (http_json_response("200 OK", &body), Duration::ZERO)
    }))
    .await;

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let proxy_hits_c = Arc::clone(&proxy_hits);

    // Build a self-signed TLS backend for `api.openai.test` tunneled through the HTTP CONNECT proxy.
    let cert = rcgen::generate_simple_self_signed(vec!["api.openai.test".to_string()]).unwrap();
    let cert_der = cert.cert.der().clone();
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let tls_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap(),
    );
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_cfg);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = proxy_listener.accept().await {
            let hits = Arc::clone(&proxy_hits_c);
            let acceptor = tls_acceptor.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                let head = String::from_utf8_lossy(&buf[..n]);
                hits.fetch_add(1, Ordering::SeqCst);
                if head.starts_with("CONNECT ") {
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await;
                    if let Ok(mut tls_stream) = acceptor.accept(stream).await {
                        let mut req_buf = [0u8; 4096];
                        let _ = tls_stream.read(&mut req_buf).await;
                        let body = openai_ok_body(r#"{"items":[{"id":0,"text":"代理译文"}]}"#);
                        let resp = http_json_response("200 OK", &body);
                        let _ = tls_stream.write_all(resp.as_bytes()).await;
                        let _ = tls_stream.shutdown().await;
                    }
                }
            });
        }
    });

    let provider = OpenAiCompatibleProvider::default();
    provider.set_test_options(ResolveOptions::default(), true);
    provider
        .apply_proxy_snapshot(
            ProxyConfig {
                mode: ProxyMode::Custom,
                custom: Some(CustomProxyConfig {
                    protocol: ProxyProtocol::Http,
                    host: "127.0.0.1".into(),
                    port: proxy_addr.port(),
                    username: None,
                    password: None,
                    bypass: vec![],
                }),
            },
            SystemProxySnapshot::default(),
            1,
        )
        .unwrap();

    // 1. Loopback Ollama MUST bypass global proxy and connect Direct!
    provider.set_config(Some(loopback_config(ollama_addr, None)));
    let local_probe = provider.probe("zh-CN").await;
    assert!(local_probe.ok);
    assert_eq!(local_probe.route, "direct (loopback)");
    assert_eq!(ollama_hits.load(Ordering::SeqCst), 1);
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);

    // 2. Public HTTPS endpoint MUST route through the configured HTTP CONNECT proxy!
    let public_cfg = ResolvedOpenAiConfig {
        identity: NormalizedServiceIdentity::parse("https://api.openai.test/v1").unwrap(),
        model: "gpt-4o-mini".into(),
        api_key: Some(Zeroizing::new("sk-proxy-test".into())),
        allow_plaintext_lan: false,
        revision: 2,
    };
    let pub_probe = provider.probe_config(&public_cfg, "zh-CN").await;
    assert!(pub_probe.ok, "probe failed: {pub_probe:?}");
    assert!(pub_probe.route.contains("proxy http://127.0.0.1:"));
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn redirect_response_is_blocked_without_following() {
    let (target_addr, target_hits, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let body = openai_ok_body(r#"{"items":[{"id":0,"text":"should-not-reach"}]}"#);
        (http_json_response("200 OK", &body), Duration::ZERO)
    }))
    .await;

    let redirect_location = format!("http://127.0.0.1:{}/v1/chat/completions", target_addr.port());
    let (redir_addr, redir_hits, _) = spawn_mock_http_server(Arc::new(move |_, _| {
        (
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {redirect_location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            Duration::ZERO,
        )
    }))
    .await;

    let provider = OpenAiCompatibleProvider::default();
    provider
        .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 1)
        .unwrap();
    provider.set_config(Some(loopback_config(redir_addr, Some("sk-secret"))));

    let err = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Hello".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(err, TranslateError::RedirectBlocked);
    assert_eq!(redir_hits.load(Ordering::SeqCst), 1);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn response_byte_limit_and_total_deadline_enforced() {
    // 1. Oversize response rejected.
    let big_content = "x".repeat(4096);
    let (big_addr, _, _) = spawn_mock_http_server(Arc::new(move |_, _| {
        let body = openai_ok_body(&format!(r#"{{"items":[{{"id":0,"text":"{big_content}"}}]}}"#));
        (http_json_response("200 OK", &body), Duration::ZERO)
    }))
    .await;

    let provider = OpenAiCompatibleProvider::new(OpenAiHttpLimits {
        max_response_bytes: 512,
        total_deadline: Duration::from_millis(250),
        ..OpenAiHttpLimits::default()
    });
    provider
        .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 1)
        .unwrap();
    provider.set_config(Some(loopback_config(big_addr, None)));

    let err = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Hello".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(err, TranslateError::ResponseTooLarge);

    // 2. Slow server exceeds total deadline -> Timeout.
    let (slow_addr, _, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let body = openai_ok_body(r#"{"items":[{"id":0,"text":"晚了"}]}"#);
        (http_json_response("200 OK", &body), Duration::from_millis(700))
    }))
    .await;
    provider.set_config(Some(loopback_config(slow_addr, None)));

    let err = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Hello".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(err, TranslateError::Timeout);
}

#[tokio::test]
async fn limited_400_response_format_fallback_and_error_sanitization() {
    // 1. Upstream rejects `response_format` with 400 -> retry ONCE without `response_format`.
    let attempt = Arc::new(AtomicUsize::new(0));
    let attempt_c = Arc::clone(&attempt);
    let (fallback_addr, hits, bodies) = spawn_mock_http_server(Arc::new(move |_, _| {
        let n = attempt_c.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            let err_body = r#"{"error":{"message":"Unsupported parameter: response_format json_object is not supported"}}"#;
            (http_json_response("400 Bad Request", err_body), Duration::ZERO)
        } else {
            let ok = openai_ok_body("```json\n{\"items\":[{\"id\":0,\"text\":\"降级成功\"}]}\n```");
            (http_json_response("200 OK", &ok), Duration::ZERO)
        }
    }))
    .await;

    let provider = OpenAiCompatibleProvider::default();
    provider
        .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 1)
        .unwrap();
    provider.set_config(Some(loopback_config(fallback_addr, None)));

    let resp = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "Fallback test".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.items[0].text, "降级成功");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let recorded = bodies.lock().unwrap().clone();
    assert!(recorded[0].contains("\"response_format\""));
    assert!(recorded[0].contains("\"temperature\":0.1"));
    assert!(!recorded[1].contains("\"response_format\""));

    // 2. Unrelated 400 error MUST NOT retry and MUST NOT leak raw upstream error body or email text.
    let (other400_addr, other_hits, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let leaky = r#"{"error":"context_length_exceeded: secret-email-body-xyz sk-leaked"}"#;
        (http_json_response("400 Bad Request", leaky), Duration::ZERO)
    }))
    .await;
    provider.set_config(Some(loopback_config(other400_addr, Some("sk-leaked"))));

    let err = provider
        .translate_batch(sample_request(
            vec![TextBlock {
                id: 0,
                text: "secret-email-body-xyz".into(),
                anchors: vec![],
            }],
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(other_hits.load(Ordering::SeqCst), 1);
    assert!(matches!(err, TranslateError::UpstreamStatus { status: 400, .. }));
    let rendered = format!("{err} | {err:?}");
    assert!(!rendered.contains("secret-email-body-xyz"));
    assert!(!rendered.contains("sk-leaked"));
    assert!(!rendered.contains("context_length_exceeded"));
}

#[tokio::test]
async fn malformed_missing_duplicate_id_responses_are_rejected() {
    let provider = OpenAiCompatibleProvider::default();
    provider
        .apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 1)
        .unwrap();

    let two_items = vec![
        TextBlock {
            id: 0,
            text: "Subject line".into(),
            anchors: vec![],
        },
        TextBlock {
            id: 1,
            text: "Body paragraph".into(),
            anchors: vec![],
        },
    ];

    for bad_json in [
        r#"{"items":[{"id":0,"text":"only one"}]}"#,
        r#"{"items":[{"id":0,"text":"dup"},{"id":0,"text":"dup2"}]}"#,
        r#"{"items":[{"id":0,"text":"ok"},{"id":99,"text":"unknown id"}]}"#,
        r#"not json at all"#,
    ] {
        let payload = bad_json.to_string();
        let (addr, _, _) = spawn_mock_http_server(Arc::new(move |_, _| {
            let body = openai_ok_body(&payload);
            (http_json_response("200 OK", &body), Duration::ZERO)
        }))
        .await;
        provider.set_config(Some(loopback_config(addr, None)));
        let err = provider
            .translate_batch(sample_request(two_items.clone(), None))
            .await
            .unwrap_err();
        assert!(
            matches!(err, TranslateError::InvalidResponse(_)),
            "expected InvalidResponse for {bad_json}, got {err:?}"
        );
    }

    // Valid out-of-order response is accepted and reordered deterministically.
    let (addr, _, _) = spawn_mock_http_server(Arc::new(|_, _| {
        let body = openai_ok_body(r#"{"items":[{"id":1,"text":"正文"},{"id":0,"text":"主题"}]}"#);
        (http_json_response("200 OK", &body), Duration::ZERO)
    }))
    .await;
    provider.set_config(Some(loopback_config(addr, None)));
    let ok = provider
        .translate_batch(sample_request(two_items, None))
        .await
        .unwrap();
    assert_eq!(ok.items[0].id, 0);
    assert_eq!(ok.items[0].text, "主题");
    assert_eq!(ok.items[1].id, 1);
    assert_eq!(ok.items[1].text, "正文");
}
