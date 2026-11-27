//! End-to-end integration tests for `maild-ui` PR 3 translation pipeline:
//! - Light + dark mode (`dark_reading_html`) bilingual & translated-only output + spoofed sender `.fp-tr-block` / `data-fp-tr`
//! - Cross-platform `ProtocolBase` URLs (Unix scheme + Windows HTTP), duplicate keys, stale/invalid `rev`, identical isolation headers
//! - Release/evict cancellation & remote-image allow/revoke token rotation
//! - Independent token-less plain-text translation path
//! - Built-in loopback OpenAI-compatible provider (with proxy snapshot guard, credential rebind, `has_api_key` redaction)
//! - External mock-daemon provider (multi-batch frozen generation & variant_revision, mid-job generation/variant mismatch, cancel propagation, hot-sync, sanitized probe)
//! - Old-daemon fallback (`-32601` & `UserOnly -32016` vs real missing permission `-32016`; never redirecting external provider to built-in)
//! - `capabilities/main.json` & `build.rs` & `lib.rs` consistency guard

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use maild_client::{
    CancelTranslationResult, ClientError, ErrorKind, InvokeTranslationRequest,
    InvokeTranslationResult, TestTranslationProviderRequest, TestTranslationProviderResult,
    TranslatedBatchItem, TranslationProviderInfo, TranslationProvidersResult,
    PROVIDER_BUSY_CODE, PROVIDER_UNAVAILABLE_CODE,
};
use maild_proxy::{ProxyConfig, SystemProxySnapshot};
use maild_translate::{
    DisplayMode, OpenAiConfigUpdate, OpenAiHttpLimits, ProbeStage, ProviderKind,
    TranslationPreferenceUpdate, BUILTIN_OPENAI_PROVIDER_ID,
};
use maild_ui::bridge::{UiCompat, UiErrorCode};
use maild_ui::documents::{
    document_response, document_response_for_query, navigation_verdict, new_token, parse_doc_query,
    DocQueryError, Document, DocumentIdentity, DocumentStore, ProtocolBase, TranslateView, Verdict,
    MAX_DOCUMENTS,
};
use maild_ui::html::{self, dark_reading_html, PrepareInput};
use maild_ui::translation::{
    DaemonTranslationClient, TranslationManager, TranslationStoragePaths,
    UiTestTranslationProviderRequest, UiTranslateHtmlRequest, UiTranslationProgress,
};
use tauri::http::{header, StatusCode};
use tauri::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type InvokeHandlerFn = Box<
    dyn Fn(&InvokeTranslationRequest) -> Result<InvokeTranslationResult, ClientError>
        + Send
        + Sync,
>;

struct MockDaemon {
    compat: RwLock<UiCompat>,
    providers_result: Mutex<Result<TranslationProvidersResult, ClientError>>,
    invoke_handler: Mutex<InvokeHandlerFn>,
    invoke_delay: Mutex<Duration>,
    probe_result: Mutex<Result<TestTranslationProviderResult, ClientError>>,
    cancelled_jobs: Mutex<Vec<String>>,
    invoke_count: AtomicUsize,
}

impl MockDaemon {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            compat: RwLock::new(UiCompat::Compatible),
            providers_result: Mutex::new(Ok(TranslationProvidersResult {
                generation: 1,
                providers: vec![],
            })),
            invoke_handler: Mutex::new(Box::new(|req| {
                Ok(InvokeTranslationResult {
                    provider_id: req.provider_id.clone(),
                    generation: req.generation,
                    variant_revision: "ext-v1".into(),
                    detected_source_lang: Some("en".into()),
                    items: req
                        .items
                        .iter()
                        .map(|item| TranslatedBatchItem {
                            id: item.id,
                            text: format!("EXT:{}", item.text),
                        })
                        .collect(),
                })
            })),
            invoke_delay: Mutex::new(Duration::ZERO),
            probe_result: Mutex::new(Ok(TestTranslationProviderResult {
                ok: true,
                provider_id: "org.example.translator".into(),
                generation: 2,
                stage: "done".into(),
                reason: "ok".into(),
                latency_ms: 12,
                detail: "secret internal detail must not leak".into(),
            })),
            cancelled_jobs: Mutex::new(Vec::new()),
            invoke_count: AtomicUsize::new(0),
        })
    }

    fn sample_external_info(generation: u64, variant_revision: &str) -> TranslationProviderInfo {
        TranslationProviderInfo {
            id: "org.example.translator".into(),
            name: "Example Translator".into(),
            description: Some("Local demo translator".into()),
            l10n: Default::default(),
            version: "1.0.0".into(),
            lifecycle: "supervised".into(),
            enabled: true,
            state: "running".into(),
            has_ui: false,
            registered: true,
            generation,
            supported_languages: vec!["en".into(), "zh-CN".into()],
            max_batch_items: 2, // Force multi-batch for > 2 blocks!
            max_batch_chars: 12_000,
            max_concurrency: 2,
            supports_cancel: true,
            variant_revision: variant_revision.into(),
        }
    }
}

#[async_trait]
impl DaemonTranslationClient for MockDaemon {
    async fn list_translation_providers(&self) -> Result<TranslationProvidersResult, ClientError> {
        self.providers_result.lock().unwrap().clone()
    }

    async fn invoke_translation(
        &self,
        request: &InvokeTranslationRequest,
    ) -> Result<InvokeTranslationResult, ClientError> {
        self.invoke_count.fetch_add(1, Ordering::SeqCst);
        let delay = *self.invoke_delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        (self.invoke_handler.lock().unwrap())(request)
    }

    async fn cancel_translation(&self, job_id: &str) -> Result<CancelTranslationResult, ClientError> {
        self.cancelled_jobs.lock().unwrap().push(job_id.to_string());
        Ok(CancelTranslationResult {
            cancelled: true,
            cancelled_requests: 1,
            notified_provider: 1,
        })
    }

    async fn test_translation_provider(
        &self,
        _request: &TestTranslationProviderRequest,
    ) -> Result<TestTranslationProviderResult, ClientError> {
        self.probe_result.lock().unwrap().clone()
    }

    fn compat(&self) -> UiCompat {
        *self.compat.read().unwrap()
    }
}

fn prepare_doc(
    store: &DocumentStore,
    base: &ProtocolBase,
    subject: &str,
    raw_html: &str,
    allow_remote: bool,
) -> String {
    let token = new_token();
    let cid_parts = HashMap::from([("logo@example".to_string(), 3u32)]);
    let prepared = html::prepare(PrepareInput {
        html: raw_html,
        token: &token,
        part_url_prefix: &base.url_prefix,
        csp_source: &base.csp_source,
        cid_parts: &cid_parts,
        allow_remote,
    })
    .unwrap();
    store.insert(
        token.clone(),
        Document {
            identity: DocumentIdentity {
                account: "acc".into(),
                folder: "INBOX".into(),
                uid: "42".into(),
            },
            subject: subject.into(),
            html: prepared.html,
            csp: prepared.csp,
            links: prepared.links,
            parts: HashMap::from([(3u32, "image/png".into())]),
            remote_urls: prepared.remote_urls,
            allow_remote,
            translation: Arc::new(RwLock::new(None)),
            translation_rev: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active_job: Arc::new(Mutex::new(None)),
        },
    );
    token
}

fn make_manager(
    tmp: &tempfile::TempDir,
    daemon: Arc<MockDaemon>,
) -> TranslationManager {
    let paths = TranslationStoragePaths {
        config_dir: tmp.path().join("cfg"),
        cache_dir: tmp.path().join("cache"),
    };
    TranslationManager::with_limits(paths, daemon, OpenAiHttpLimits::default()).unwrap()
}

/// Minimal loopback mock OpenAI `/v1/chat/completions` server returning deterministic JSON.
async fn spawn_mock_openai_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let hits_req = Arc::clone(&hits_clone);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                let mut content_len = 0usize;

                while let Ok(n) = stream.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let header_str = String::from_utf8_lossy(&buf[..pos]);
                            for line in header_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("content-length") {
                                        content_len = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(end) = header_end {
                        if buf.len() >= end + content_len {
                            break;
                        }
                    }
                }

                hits_req.fetch_add(1, Ordering::SeqCst);
                let body_bytes = match header_end {
                    Some(end) => &buf[end..buf.len().min(end + content_len)],
                    None => &[][..],
                };
                let parsed: serde_json::Value =
                    serde_json::from_slice(body_bytes).unwrap_or_default();
                let user_content = parsed["messages"][1]["content"]
                    .as_str()
                    .unwrap_or("{}");
                let payload: serde_json::Value =
                    serde_json::from_str(user_content).unwrap_or_default();
                let items = payload["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let out_items: Vec<serde_json::Value> = items
                    .into_iter()
                    .map(|item| {
                        let id = item["id"].as_u64().unwrap_or(0);
                        let text = item["text"].as_str().unwrap_or("");
                        serde_json::json!({ "id": id, "text": format!("ZH:{text}") })
                    })
                    .collect();
                let content_json = serde_json::json!({ "items": out_items }).to_string();
                let resp_body = serde_json::json!({
                    "choices": [{
                        "message": { "content": content_json }
                    }]
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://127.0.0.1:{}/v1", addr.port()), hits, handle)
}

#[tokio::test]
async fn light_and_dark_mode_bilingual_and_translated_with_spoofed_sender_protection() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = MockDaemon::new();
    let (base_url, hits, server) = spawn_mock_openai_server().await;
    let mgr = make_manager(&tmp, daemon);

    // First proxy snapshot guard: before proxy snapshot arrives, built-in translation fails closed!
    mgr.save_preference(TranslationPreferenceUpdate {
        expected_revision: None,
        active_provider_id: BUILTIN_OPENAI_PROVIDER_ID.into(),
        display_mode: DisplayMode::Bilingual,
        target_lang: "zh-CN".into(),
        detect_banner_enabled: true,
        auto_translate: false,
        auto_translate_provider_id: None,
        ignored_languages: vec![],
        openai_compatible: Some(OpenAiConfigUpdate {
            preset: Some("ollama".into()),
            base_url: base_url.clone(),
            model: "qwen2.5".into(),
            api_key: None,
            clear_api_key: false,
            allow_plaintext_lan: false,
        }),
    })
    .unwrap();

    // Verify get_preference never exposes plaintext api_key and reflects has_api_key=false.
    let pref_view = mgr.get_preference();
    assert!(!pref_view.openai_compatible.as_ref().unwrap().has_api_key);

    let store = DocumentStore::new();
    let base = ProtocolBase::unix();
    let raw_html = r##"<body>
        <p class="fp-tr-block" data-fp-tr="1" style="color:black">Spoofed sender paragraph</p>
        <p>Please <a href="https://example.com/verify">verify account</a> today.</p>
        <img src="cid:logo@example" width="64">
    </body>"##;
    let token = prepare_doc(&store, &base, "Welcome Email", raw_html, false);

    // Before proxy snapshot: must fail with translation_proxy_not_ready!
    let err = mgr
        .translate_html_document(
            &store,
            &base,
            UiTranslateHtmlRequest {
                token: token.clone(),
                target_lang: None,
                provider_id: None,
                force_refresh: false,
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(err.reason, "translation_proxy_not_ready");
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // Apply first valid proxy snapshot -> now translation succeeds!
    mgr.apply_proxy_snapshot(ProxyConfig::default(), SystemProxySnapshot::default(), 1);

    let mut progress_events: Vec<UiTranslationProgress> = Vec::new();
    let result = mgr
        .translate_html_document(
            &store,
            &base,
            UiTranslateHtmlRequest {
                token: token.clone(),
                target_lang: Some("zh-CN".into()),
                provider_id: None,
                force_refresh: false,
            },
            |ev| progress_events.push(ev),
        )
        .await
        .unwrap();

    assert_eq!(result.rev, 1);
    assert_eq!(result.translated_subject.as_deref(), Some("ZH:Welcome Email"));
    assert_eq!(result.provider_id, BUILTIN_OPENAI_PROVIDER_ID);
    assert!(!result.from_cache);
    assert_eq!(
        result.bilingual_url,
        format!("{}/doc/{token}?translate=bilingual&rev=1", base.url_prefix)
    );
    assert_eq!(
        result.translated_dark_url,
        format!(
            "{}/doc/{token}?translate=translated&rev=1&appearance=dark",
            base.url_prefix
        )
    );

    // Progress events must contain ZERO plaintext email content!
    assert!(!progress_events.is_empty());
    let progress_json = serde_json::to_string(&progress_events).unwrap();
    assert!(!progress_json.contains("Spoofed sender"));
    assert!(!progress_json.contains("verify account"));
    assert!(!progress_json.contains("Welcome Email"));

    // Serve light & dark bilingual + translated views via protocol handler
    let doc = store.get(&token).unwrap();
    let orig_resp = document_response(&doc, false);
    let bilingual_light = document_response_for_query(&doc, Some("translate=bilingual&rev=1"));
    let bilingual_dark =
        document_response_for_query(&doc, Some("translate=bilingual&rev=1&appearance=dark"));
    let translated_dark =
        document_response_for_query(&doc, Some("translate=translated&rev=1&appearance=dark"));

    for resp in [&bilingual_light, &bilingual_dark, &translated_dark] {
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()[header::CONTENT_SECURITY_POLICY],
            orig_resp.headers()[header::CONTENT_SECURITY_POLICY]
        );
        assert_eq!(resp.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    }

    let bilingual_dark_body = String::from_utf8(bilingual_dark.into_body()).unwrap();
    // Trusted generated translation spans get dark-mode readable styling (#cfd6e6 + #60a5fa left border).
    assert!(bilingual_dark_body.contains("data-fp-tr=\"1\""));
    assert!(bilingual_dark_body.contains("color:#cfd6e6!important"));
    assert!(bilingual_dark_body.contains("border-left:2px solid #60a5fa!important"));

    // Sender's spoofed `<p class="fp-tr-block" data-fp-tr="1">` had its `data-fp-tr` stripped by
    // `html::prepare`, so dark_reading_html styles that `<p>` as regular email text (#e4e4ed), NOT #cfd6e6!
    let p_tag_start = bilingual_dark_body
        .find("<p class=\"fp-tr-block\"")
        .expect("sender p tag present");
    let p_tag_end = bilingual_dark_body[p_tag_start..].find('>').unwrap() + p_tag_start;
    let sender_p_open = &bilingual_dark_body[p_tag_start..=p_tag_end];
    assert!(
        !sender_p_open.contains("data-fp-tr"),
        "prepare must strip spoofed sender data-fp-tr: {sender_p_open}"
    );
    assert!(
        sender_p_open.contains("color:#e4e4ed!important"),
        "spoofed .fp-tr-block must not receive trusted translation dark style: {sender_p_open}"
    );
    assert!(
        !sender_p_open.contains("#60a5fa"),
        "spoofed .fp-tr-block must not receive blue translation border: {sender_p_open}"
    );

    // Direct dark_reading_html check on bare spoofed class without data-fp-tr:
    let spoofed_only = dark_reading_html("<p class=\"fp-tr-block\">Fake</p>");
    assert!(spoofed_only.contains("color:#e4e4ed!important"));
    assert!(!spoofed_only.contains("#cfd6e6"));

    // Translated-only dark mode keeps cid image and safe `/link/<token>/0` link!
    let translated_dark_body = String::from_utf8(translated_dark.into_body()).unwrap();
    assert!(translated_dark_body.contains(&format!("/part/{token}/3")));
    assert!(translated_dark_body.contains(&format!("/link/{token}/0")));
    assert!(translated_dark_body.contains("ZH:Spoofed sender paragraph"));

    // Second call with same settings hits SQLite cache (0 extra upstream HTTP requests)!
    let hits_before = hits.load(Ordering::SeqCst);
    let cached = mgr
        .translate_html_document(
            &store,
            &base,
            UiTranslateHtmlRequest {
                token: token.clone(),
                target_lang: Some("zh-CN".into()),
                provider_id: None,
                force_refresh: false,
            },
            |_| {},
        )
        .await
        .unwrap();
    assert!(cached.from_cache);
    assert_eq!(cached.rev, 2);
    assert_eq!(hits.load(Ordering::SeqCst), hits_before);

    // Stale rev=1 is now 404, current rev=2 is 200!
    assert_eq!(
        document_response_for_query(&doc, Some("translate=bilingual&rev=1")).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        document_response_for_query(&doc, Some("translate=bilingual&rev=2")).status(),
        StatusCode::OK
    );

    // Cache stats & clear work cleanly
    let stats = mgr.cache_stats().unwrap();
    assert_eq!(stats.entry_count, 1);
    let clear = mgr.clear_cache().await.unwrap();
    assert_eq!(clear.cleared_entries, 1);
    assert_eq!(clear.stats_after.entry_count, 0);

    server.abort();
}

#[test]
fn cross_platform_protocol_base_query_validation_and_stale_rev_semantics() {
    let token = new_token();
    let unix_base = ProtocolBase::unix();
    let win_base = ProtocolBase::windows();

    let unix_url = unix_base.translated_doc_url(&token, TranslateView::Bilingual, 3, true);
    assert!(unix_url.starts_with(&format!("{}://localhost/doc/{token}?", maild_brand::URI_SCHEME)));
    assert!(unix_url.ends_with("translate=bilingual&rev=3&appearance=dark"));

    let win_url = win_base.translated_doc_url(&token, TranslateView::Translated, 4, false);
    assert_eq!(
        win_url,
        format!(
            "http://{}.localhost/doc/{token}?translate=translated&rev=4",
            maild_brand::URI_SCHEME
        )
    );

    // Navigation verdict allows both Unix and Windows translated document URLs!
    let app_allows = |_: &Url| false;
    assert_eq!(
        navigation_verdict(&Url::parse(&unix_url).unwrap(), &unix_base, &app_allows),
        Verdict::Allow
    );
    assert_eq!(
        navigation_verdict(&Url::parse(&win_url).unwrap(), &win_base, &app_allows),
        Verdict::Allow
    );

    // Query parser edge cases: duplicate keys, missing rev, rev=0, rev without translate, bad values
    assert_eq!(
        parse_doc_query(Some("translate=bilingual&translate=translated&rev=1")),
        Err(DocQueryError::DuplicateKey)
    );
    assert_eq!(
        parse_doc_query(Some("translate=bilingual&rev=1&rev=2")),
        Err(DocQueryError::DuplicateKey)
    );
    assert_eq!(
        parse_doc_query(Some("appearance=dark&appearance=light")),
        Err(DocQueryError::DuplicateKey)
    );
    assert_eq!(
        parse_doc_query(Some("translate=bilingual")),
        Err(DocQueryError::MissingRev)
    );
    assert_eq!(
        parse_doc_query(Some("translate=bilingual&rev=0")),
        Err(DocQueryError::InvalidRev)
    );
    assert_eq!(
        parse_doc_query(Some("translate=bilingual&rev=abc")),
        Err(DocQueryError::InvalidRev)
    );
    assert_eq!(
        parse_doc_query(Some("rev=1")),
        Err(DocQueryError::UnexpectedRev)
    );
    assert_eq!(
        parse_doc_query(Some("translate=sidebyside&rev=1")),
        Err(DocQueryError::InvalidTranslateValue)
    );
    assert_eq!(
        parse_doc_query(Some("appearance=sepia")),
        Err(DocQueryError::InvalidAppearanceValue)
    );

    // GET on untranslated document for ?translate=bilingual&rev=1 returns 404 (never triggers translation)
    let store = DocumentStore::new();
    let tok = prepare_doc(&store, &unix_base, "S", "<p>Hello</p>", false);
    let doc = store.get(&tok).unwrap();
    assert_eq!(
        document_response_for_query(&doc, Some("translate=bilingual&rev=1")).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        document_response_for_query(&doc, Some("translate=bilingual&translate=bilingual&rev=1"))
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn external_plugin_multi_batch_frozen_generation_cancellation_eviction_and_old_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = MockDaemon::new();
    *daemon.providers_result.lock().unwrap() = Ok(TranslationProvidersResult {
        generation: 5,
        providers: vec![MockDaemon::sample_external_info(2, "ext-v1")],
    });

    let mgr = Arc::new(make_manager(&tmp, Arc::clone(&daemon)));
    let list = mgr.sync_external_providers_from_daemon().await.unwrap();
    assert!(list.external_supported);
    assert_eq!(list.catalog_generation, 5);
    assert_eq!(list.providers.len(), 2);
    assert!(list.providers.iter().any(|p| p.id == "org.example.translator" && p.registered && p.generation == 2));

    // Stale catalog push (generation 4 < 5) must be ignored!
    assert!(mgr
        .apply_external_providers_snapshot(TranslationProvidersResult {
            generation: 4,
            providers: vec![],
        })
        .is_none());

    // Select external provider
    mgr.save_preference(TranslationPreferenceUpdate {
        expected_revision: None,
        active_provider_id: "org.example.translator".into(),
        display_mode: DisplayMode::Bilingual,
        target_lang: "zh-CN".into(),
        detect_banner_enabled: true,
        auto_translate: false,
        auto_translate_provider_id: None,
        ignored_languages: vec![],
        openai_compatible: None,
    })
    .unwrap();

    // 1. Multi-batch plain-text token-less path (max_batch_items = 2, 4 blocks = 2 batches!)
    let plain_res = mgr
        .translate_plain_text(
            "Plain Subject",
            "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.",
            None,
            None,
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(plain_res.provider_id, "org.example.translator");
    assert_eq!(plain_res.provider_generation, 2);
    assert_eq!(
        plain_res.translated_subject.as_deref(),
        Some("EXT:Plain Subject")
    );
    assert_eq!(plain_res.blocks.len(), 3);
    assert_eq!(plain_res.blocks[2].translated, "EXT:Third paragraph.");
    assert_eq!(daemon.invoke_count.load(Ordering::SeqCst), 2);

    // 2. Sanitized external probe: raw daemon `detail` must NEVER leak into ProbeResult!
    let probe = mgr
        .test_provider(UiTestTranslationProviderRequest {
            provider_id: "org.example.translator".into(),
            target_lang: "zh-CN".into(),
            draft_openai: None,
        })
        .await
        .unwrap();
    assert!(probe.ok);
    assert_eq!(probe.stage, ProbeStage::Done);
    assert_eq!(probe.reason, "ok");
    assert_eq!(probe.route, "ipc://daemon");
    assert!(!serde_json::to_string(&probe)
        .unwrap()
        .contains("secret internal detail"));

    // 3. Mid-job generation/variant change must fail closed without stitching or falling back to built-in!
    *daemon.invoke_handler.lock().unwrap() = Box::new(|req| {
        Ok(InvokeTranslationResult {
            provider_id: req.provider_id.clone(),
            generation: req.generation,
            variant_revision: "ext-v2-upgraded".into(), // changed mid-job!
            detected_source_lang: Some("en".into()),
            items: req
                .items
                .iter()
                .map(|i| TranslatedBatchItem {
                    id: i.id,
                    text: format!("NEW:{}", i.text),
                })
                .collect(),
        })
    });
    let err = mgr
        .translate_plain_text("S", "Another email body.", None, None, true, |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.reason, "translation_provider_unavailable");

    // Restore valid handler with delay to test release/evict cancellation & remote-image token rotation!
    *daemon.invoke_handler.lock().unwrap() = Box::new(|req| {
        Ok(InvokeTranslationResult {
            provider_id: req.provider_id.clone(),
            generation: req.generation,
            variant_revision: "ext-v1".into(),
            detected_source_lang: Some("en".into()),
            items: req
                .items
                .iter()
                .map(|i| TranslatedBatchItem {
                    id: i.id,
                    text: format!("EXT:{}", i.text),
                })
                .collect(),
        })
    });
    *daemon.invoke_delay.lock().unwrap() = Duration::from_millis(250);

    let store = Arc::new(DocumentStore::new());
    let base = ProtocolBase::unix();
    let old_token = prepare_doc(
        &store,
        &base,
        "Slow Email",
        "<p>Paragraph 1</p><p>Paragraph 2</p><p>Paragraph 3</p>",
        false,
    );

    let mgr_task = Arc::clone(&mgr);
    let store_task = Arc::clone(&store);
    let base_task = base.clone();
    let tok_task = old_token.clone();
    let inflight = tokio::spawn(async move {
        mgr_task
            .translate_html_document(
                &store_task,
                &base_task,
                UiTranslateHtmlRequest {
                    token: tok_task,
                    target_lang: Some("zh-CN".into()),
                    provider_id: None,
                    force_refresh: true,
                },
                |_| {},
            )
            .await
    });

    // Give the task a moment to enter batch 1, then simulate remote-image toggle (release old token + insert new token)
    tokio::time::sleep(Duration::from_millis(40)).await;
    let (released, cancelled_id) = store.release_and_cancel(&old_token);
    assert!(released);
    assert!(cancelled_id.is_some());
    let new_token_after_remote = prepare_doc(
        &store,
        &base,
        "Slow Email",
        "<p>Paragraph 1</p><p>Paragraph 2</p><p>Paragraph 3</p>",
        true,
    );
    assert_ne!(old_token, new_token_after_remote);

    let res = inflight.await.unwrap();
    assert_eq!(res.unwrap_err().reason, "translation_cancelled");
    assert!(
        store.get(&new_token_after_remote).unwrap().current_translation().is_none(),
        "cancelled old-token job must never commit to rotated token"
    );
    assert!(
        !daemon.cancelled_jobs.lock().unwrap().is_empty(),
        "daemon cancelTranslation must be notified on cancellation"
    );

    // Test LRU eviction cancels in-flight translation on evicted document!
    let victim_token = prepare_doc(&store, &base, "Victim", "<p>Evict me</p>", false);
    let mgr_evict = Arc::clone(&mgr);
    let store_evict = Arc::clone(&store);
    let base_evict = base.clone();
    let victim_clone = victim_token.clone();
    let evict_task = tokio::spawn(async move {
        mgr_evict
            .translate_html_document(
                &store_evict,
                &base_evict,
                UiTranslateHtmlRequest {
                    token: victim_clone,
                    target_lang: Some("zh-CN".into()),
                    provider_id: None,
                    force_refresh: true,
                },
                |_| {},
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    for i in 0..=MAX_DOCUMENTS {
        prepare_doc(&store, &base, &format!("Filler {i}"), "<p>Fill</p>", false);
    }
    assert!(store.get(&victim_token).is_none());
    assert_eq!(evict_task.await.unwrap().unwrap_err().reason, "translation_cancelled");

    // 4. Busy (-32022) and unavailable (-32018) mapping
    *daemon.invoke_delay.lock().unwrap() = Duration::ZERO;
    *daemon.invoke_handler.lock().unwrap() = Box::new(|_| {
        Err(ClientError {
            kind: ErrorKind::Rpc,
            message: "queue full".into(),
            rpc_code: Some(PROVIDER_BUSY_CODE),
            rpc_data: None,
        })
    });
    let busy_err = mgr
        .translate_plain_text("S", "Busy test", None, None, true, |_| {})
        .await
        .unwrap_err();
    assert_eq!(busy_err.code, UiErrorCode::Limit);
    assert_eq!(busy_err.reason, "translation_provider_busy");

    *daemon.invoke_handler.lock().unwrap() = Box::new(|_| {
        Err(ClientError {
            kind: ErrorKind::Rpc,
            message: "offline".into(),
            rpc_code: Some(PROVIDER_UNAVAILABLE_CODE),
            rpc_data: None,
        })
    });
    let unavail_err = mgr
        .translate_plain_text("S", "Offline test", None, None, true, |_| {})
        .await
        .unwrap_err();
    assert_eq!(unavail_err.reason, "translation_provider_unavailable");

    // 5. Old-daemon fallback: -32601 AND legacy UserOnly -32016 mark external_supported=false,
    //    keep built-in listed, and NEVER redirect selected external provider traffic to built-in!
    for legacy_err in [
        ClientError {
            kind: ErrorKind::Rpc,
            message: "method not found".into(),
            rpc_code: Some(-32601),
            rpc_data: None,
        },
        ClientError {
            kind: ErrorKind::Rpc,
            message: "前端主体无权调用 listTranslationProviders：该方法仅限用户主体".into(),
            rpc_code: Some(-32016),
            rpc_data: None,
        },
    ] {
        *daemon.providers_result.lock().unwrap() = Err(legacy_err);
        let synced = mgr.sync_external_providers_from_daemon().await.unwrap();
        assert!(!synced.external_supported);
        assert_eq!(synced.providers.len(), 1);
        assert_eq!(synced.providers[0].kind, ProviderKind::Builtin);

        // Selected external provider MUST fail with translation_external_unsupported, NEVER fall back to built-in!
        let ext_err = mgr
            .translate_plain_text("S", "Hello old daemon", None, None, true, |_| {})
            .await
            .unwrap_err();
        assert_eq!(ext_err.reason, "translation_external_unsupported");
    }

    // 6. Real missing permission (-32016 without 仅限用户主体 on compatible daemon) surfaces Denied!
    *daemon.providers_result.lock().unwrap() = Err(ClientError {
        kind: ErrorKind::Rpc,
        message: "前端主体无权调用 listTranslationProviders：缺少 capability plugins_manage".into(),
        rpc_code: Some(-32016),
        rpc_data: None,
    });
    let denied = mgr.sync_external_providers_from_daemon().await.unwrap_err();
    assert_eq!(denied.code, UiErrorCode::Denied);
    assert_eq!(denied.reason, "denied");
}

#[test]
fn capabilities_main_json_build_rs_and_generate_handler_are_consistent() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let build_rs = std::fs::read_to_string(manifest_dir.join("build.rs")).unwrap();
    let main_json =
        std::fs::read_to_string(manifest_dir.join("capabilities/main.json")).unwrap();
    let lib_rs = std::fs::read_to_string(manifest_dir.join("src/lib.rs")).unwrap();

    let cap: serde_json::Value = serde_json::from_str(&main_json).unwrap();
    let permissions: HashSet<String> = cap["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();

    let expected_translation_cmds = [
        "ui_translation_get_preference",
        "ui_translation_save_preference",
        "ui_translation_list_providers",
        "ui_translation_test_provider",
        "ui_translation_detect_language",
        "ui_translation_translate_html",
        "ui_translation_translate_plain",
        "ui_translation_cancel",
        "ui_translation_cache_stats",
        "ui_translation_clear_cache",
    ];

    for cmd in expected_translation_cmds {
        assert!(
            build_rs.contains(&format!("\"{cmd}\"")),
            "build.rs missing command {cmd}"
        );
        assert!(
            lib_rs.contains(&format!("commands::{cmd}")),
            "lib.rs generate_handler! missing commands::{cmd}"
        );
        let perm = format!("allow-{}", cmd.replace('_', "-"));
        assert!(
            permissions.contains(&perm),
            "capabilities/main.json missing permission {perm}"
        );
    }
}
