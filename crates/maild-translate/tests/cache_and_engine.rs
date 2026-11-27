use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use maild_translate::{
    plan_batches, AnchorSlot, BatchLimits, CacheConfig, CacheKeyInput, CacheWriteInput,
    CacheWriteOutcome, ContentKind, ProbeResult, ProbeStage, ProviderCapabilities, ProviderKind,
    ProviderMetadata, TextBlock, TranslateDocumentRequest, TranslateError, TranslatedBlock,
    TranslationBatchRequest, TranslationBatchResponse, TranslationCache, TranslationEngine,
    TranslationProvider, TranslationRegistry, CACHE_SCHEMA_VERSION, CHUNKER_AND_PLACEHOLDER_VERSION,
    PROMPT_VERSION,
};

struct MockProvider {
    meta: ProviderMetadata,
    calls: Arc<AtomicUsize>,
    fail_on_batch: Option<u32>,
    fail_all: Arc<AtomicBool>,
    prefix: String,
    on_batch_hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
}

impl MockProvider {
    fn new(id: &str, generation: u64, variant_revision: &str, limits: BatchLimits) -> Self {
        Self {
            meta: ProviderMetadata {
                id: id.to_string(),
                kind: if id.starts_with("builtin.") {
                    ProviderKind::Builtin
                } else {
                    ProviderKind::External
                },
                display_name: "Mock Provider".into(),
                version: "1.0.0".into(),
                variant_revision: variant_revision.to_string(),
                generation,
                capabilities: ProviderCapabilities {
                    supported_languages: vec!["en".into(), "zh-CN".into()],
                    limits,
                    supports_cancel: true,
                },
            },
            calls: Arc::new(AtomicUsize::new(0)),
            fail_on_batch: None,
            fail_all: Arc::new(AtomicBool::new(false)),
            prefix: "TR:".into(),
            on_batch_hook: None,
        }
    }
}

#[async_trait]
impl TranslationProvider for MockProvider {
    fn metadata(&self) -> ProviderMetadata {
        self.meta.clone()
    }

    async fn translate_batch(
        &self,
        request: TranslationBatchRequest,
    ) -> Result<TranslationBatchResponse, TranslateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(ref hook) = self.on_batch_hook {
            hook(request.batch_index);
        }
        if self.fail_all.load(Ordering::SeqCst)
            || self.fail_on_batch == Some(request.batch_index)
        {
            return Err(TranslateError::UpstreamStatus {
                status: 503,
                reason: "mock failure".into(),
            });
        }
        let items = request
            .items
            .into_iter()
            .map(|b| TranslatedBlock {
                id: b.id,
                text: format!("{}{}", self.prefix, b.text),
            })
            .collect();
        Ok(TranslationBatchResponse {
            items,
            detected_source_lang: Some("en".into()),
        })
    }

    async fn probe(&self, _target_lang: &str) -> ProbeResult {
        ProbeResult {
            ok: true,
            stage: ProbeStage::Done,
            reason: "ok".into(),
            latency_ms: 1,
            route: "mock".into(),
            plaintext_lan_transport: false,
        }
    }
}

#[test]
fn registry_namespace_guard_and_negotiated_limits() {
    let reg = TranslationRegistry::new();

    // External provider cannot claim reserved `builtin.*` namespace.
    let rogue = Arc::new(MockProvider {
        meta: ProviderMetadata {
            id: "builtin.spoofed".into(),
            kind: ProviderKind::External,
            display_name: "Rogue".into(),
            version: "1.0.0".into(),
            variant_revision: "v1".into(),
            generation: 1,
            capabilities: ProviderCapabilities::default(),
        },
        calls: Arc::new(AtomicUsize::new(0)),
        fail_on_batch: None,
        fail_all: Arc::new(AtomicBool::new(false)),
        prefix: "".into(),
        on_batch_hook: None,
    });
    assert!(matches!(
        reg.register_external(rogue),
        Err(TranslateError::InvalidConfig(_))
    ));

    // Negotiated batch limits always take min(host, provider).
    let host = BatchLimits {
        max_batch_items: 20,
        max_batch_chars: 10_000,
        max_concurrency: 3,
    };
    let provider = BatchLimits {
        max_batch_items: 8,
        max_batch_chars: 25_000,
        max_concurrency: 1,
    };
    assert_eq!(
        BatchLimits::negotiate(host, provider),
        BatchLimits {
            max_batch_items: 8,
            max_batch_chars: 10_000,
            max_concurrency: 1,
        }
    );
}

#[test]
fn cache_key_distinguishes_same_text_different_anchors_and_variants() {
    let block_a = vec![TextBlock {
        id: 0,
        text: "Click [[1]]here[[/1]] now".into(),
        anchors: vec![AnchorSlot {
            slot_id: 1,
            link_index: 0,
        }],
    }];
    let block_b = vec![TextBlock {
        id: 0,
        text: "Click [[1]]here[[/1]] now".into(),
        anchors: vec![AnchorSlot {
            slot_id: 1,
            link_index: 1,
        }],
    }];
    let block_plain = vec![TextBlock {
        id: 0,
        text: "Click [[1]]here[[/1]] now".into(),
        anchors: vec![],
    }];

    let key = |blocks: &[TextBlock], kind: ContentKind, rev: &str| {
        CacheKeyInput {
            schema_ver: CACHE_SCHEMA_VERSION,
            resolved_source_lang: "en",
            resolved_target_lang: "zh-CN",
            content_kind: kind,
            chunker_and_placeholder_ver: CHUNKER_AND_PLACEHOLDER_VERSION,
            prompt_ver: PROMPT_VERSION,
            blocks,
            provider_id: "builtin.openai_compatible",
            provider_variant_revision: rev,
        }
        .compute_key()
    };

    let k_a = key(&block_a, ContentKind::Html, "https://api.openai.com:443/v1|gpt-4o-mini");
    let k_b = key(&block_b, ContentKind::Html, "https://api.openai.com:443/v1|gpt-4o-mini");
    let k_p = key(&block_plain, ContentKind::Html, "https://api.openai.com:443/v1|gpt-4o-mini");
    let k_plain_kind =
        key(&block_a, ContentKind::Plain, "https://api.openai.com:443/v1|gpt-4o-mini");
    let k_other_model = key(&block_a, ContentKind::Html, "https://api.openai.com:443/v1|gpt-4o");

    assert_ne!(k_a, k_b, "same text with different link_index must differ");
    assert_ne!(k_a, k_p, "same text with vs without anchors must differ");
    assert_ne!(k_a, k_plain_kind, "html vs plain must differ");
    assert_ne!(k_a, k_other_model, "different model variant must differ");
}

#[tokio::test]
async fn engine_cache_hit_partial_failure_force_refresh_and_oversized_paragraph() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(TranslationCache::open(dir.path()).unwrap());
    let reg = Arc::new(TranslationRegistry::new());
    let engine = TranslationEngine::new(Arc::clone(&reg), Arc::clone(&cache));

    let limits = BatchLimits {
        max_batch_items: 1,
        max_batch_chars: 40,
        max_concurrency: 1,
    };

    // 1. Partial failure (batch 0 ok, batch 1 fails) MUST NOT commit anything to cache.
    let partial = Arc::new(MockProvider {
        fail_on_batch: Some(1),
        ..MockProvider::new("com.example.mock", 1, "v1", limits)
    });
    reg.register_external(partial).unwrap();

    let blocks = vec![
        TextBlock {
            id: 0,
            text: "Subject line".into(),
            anchors: vec![],
        },
        TextBlock {
            id: 1,
            text: "Second block".into(),
            anchors: vec![],
        },
    ];
    let err = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-partial",
            "com.example.mock",
            "en",
            "zh-CN",
            ContentKind::Plain,
            blocks.clone(),
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, TranslateError::UpstreamStatus { status: 503, .. }));
    assert_eq!(cache.stats().unwrap().entry_count, 0);

    // 2. Full success commits atomically; second call hits cache with 0 provider calls.
    let fail_toggle = Arc::new(AtomicBool::new(false));
    let healthy = Arc::new(MockProvider {
        fail_all: Arc::clone(&fail_toggle),
        prefix: "V1:".into(),
        ..MockProvider::new("com.example.mock", 2, "v1", limits)
    });
    let healthy_calls = Arc::clone(&healthy.calls);
    reg.register_external(healthy).unwrap();

    let first = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-ok-1",
            "com.example.mock",
            "en",
            "zh-CN",
            ContentKind::Plain,
            blocks.clone(),
        ))
        .await
        .unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.cache_write, Some(CacheWriteOutcome::Committed));
    assert_eq!(healthy_calls.load(Ordering::SeqCst), 2);
    assert_eq!(first.items[0].text, "V1:Subject line");
    assert_eq!(first.items[1].text, "V1:Second block");

    let second = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-ok-2",
            "com.example.mock",
            "en",
            "zh-CN",
            ContentKind::Plain,
            blocks.clone(),
        ))
        .await
        .unwrap();
    assert!(second.from_cache);
    assert_eq!(healthy_calls.load(Ordering::SeqCst), 2);

    // 3. Force-refresh failure MUST preserve the previous valid cache entry intact!
    fail_toggle.store(true, Ordering::SeqCst);
    let mut refresh_req = TranslateDocumentRequest::new(
        "job-refresh-fail",
        "com.example.mock",
        "en",
        "zh-CN",
        ContentKind::Plain,
        blocks.clone(),
    );
    refresh_req.force_refresh = true;
    assert!(engine.translate_document(refresh_req).await.is_err());

    // Old cached value is still returned cleanly on normal lookup.
    let preserved = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-after-failed-refresh",
            "com.example.mock",
            "en",
            "zh-CN",
            ContentKind::Plain,
            blocks.clone(),
        ))
        .await
        .unwrap();
    assert!(preserved.from_cache);
    assert_eq!(preserved.items[0].text, "V1:Subject line");

    // 4. Oversized single paragraph (> max_batch_chars) splits preserving placeholders & reassembles.
    fail_toggle.store(false, Ordering::SeqCst);
    let oversized = vec![TextBlock {
        id: 0,
        text: "First sentence is quite long here. Second sentence has [[1]]a link[[/1]] inside. Third sentence finishes cleanly.".into(),
        anchors: vec![AnchorSlot {
            slot_id: 1,
            link_index: 0,
        }],
    }];
    let plan = plan_batches(&oversized, limits).unwrap();
    assert!(plan.batches.len() >= 3);
    for b in &plan.batches {
        // Ensure placeholder tag was never torn across chunks.
        let txt = &b.items[0].text;
        assert_eq!(txt.contains("[[1]]"), txt.contains("[[/1]]"));
    }
    let res = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-oversized",
            "com.example.mock",
            "en",
            "zh-CN",
            ContentKind::Html,
            oversized,
        ))
        .await
        .unwrap();
    assert_eq!(res.items.len(), 1);
    assert!(res.items[0].text.contains("[[1]]a link[[/1]]"));
}

#[tokio::test]
async fn clear_epoch_prevents_inflight_writeback_and_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(TranslationCache::open(dir.path()).unwrap());
    let reg = Arc::new(TranslationRegistry::new());
    let engine = TranslationEngine::new(Arc::clone(&reg), Arc::clone(&cache));

    let cache_for_hook = Arc::clone(&cache);
    let provider = Arc::new(MockProvider {
        on_batch_hook: Some(Arc::new(move |_| {
            // Simulate user clicking "Clear Cache" while translation is in flight!
            let report = cache_for_hook.clear().unwrap();
            assert_eq!(report.new_clear_epoch, 1);
            assert!(report.physical_reclaim_succeeded);
        })),
        ..MockProvider::new("com.example.race", 1, "v1", BatchLimits::default())
    });
    reg.register_external(provider).unwrap();

    let blocks = vec![TextBlock {
        id: 0,
        text: "In-flight email text".into(),
        anchors: vec![],
    }];
    let res = engine
        .translate_document(TranslateDocumentRequest::new(
            "job-race",
            "com.example.race",
            "en",
            "zh-CN",
            ContentKind::Plain,
            blocks.clone(),
        ))
        .await
        .unwrap();

    // Translation still returned to caller, but cache write-back was discarded due to clear_epoch!
    assert_eq!(res.cache_write, Some(CacheWriteOutcome::DiscardedByClear));
    assert_eq!(cache.stats().unwrap().entry_count, 0);
    assert_eq!(cache.current_clear_epoch().unwrap(), 1);

    // Persisted clear_epoch survives reopen, and Unix permissions remain 0600.
    drop(engine);
    drop(cache);
    let reopened = TranslationCache::open(dir.path()).unwrap();
    assert_eq!(reopened.current_clear_epoch().unwrap(), 1);

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(reopened.db_path()).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
    }
}

#[test]
fn ttl_expiration_and_lru_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let clock_ms = Arc::new(AtomicI64::new(1_000_000));
    let clock_c = Arc::clone(&clock_ms);

    let cache = TranslationCache::open_with_clock(
        dir.path(),
        CacheConfig {
            max_entries: 2,
            max_logical_bytes: 1024 * 1024,
            ttl: Duration::from_secs(60),
        },
        move || clock_c.load(Ordering::SeqCst),
    )
    .unwrap();

    let make_write = |key: &str, text: &str| CacheWriteInput {
        cache_key: key.to_string(),
        provider_id: "builtin.openai_compatible".into(),
        provider_variant_revision: "v1".into(),
        content_kind: ContentKind::Plain,
        source_lang: "en".into(),
        target_lang: "zh-CN".into(),
        expected_blocks: vec![TextBlock {
            id: 0,
            text: text.into(),
            anchors: vec![],
        }],
        translated_blocks: vec![TranslatedBlock {
            id: 0,
            text: format!("TR:{text}"),
        }],
    };

    let epoch = cache.current_clear_epoch().unwrap();
    cache.put_atomic(make_write("k1", "one"), epoch).unwrap();
    clock_ms.fetch_add(1_000, Ordering::SeqCst);
    cache.put_atomic(make_write("k2", "two"), epoch).unwrap();

    // Touch k1 so k2 becomes least-recently-used.
    clock_ms.fetch_add(1_000, Ordering::SeqCst);
    assert!(cache
        .get(
            "k1",
            &[TextBlock {
                id: 0,
                text: "one".into(),
                anchors: vec![],
            }],
        )
        .unwrap()
        .is_some());

    // Inserting k3 evicts k2 (LRU), keeping k1 and k3.
    clock_ms.fetch_add(1_000, Ordering::SeqCst);
    cache.put_atomic(make_write("k3", "three"), epoch).unwrap();

    let stats = cache.stats().unwrap();
    assert_eq!(stats.entry_count, 2);
    assert!(cache
        .get(
            "k2",
            &[TextBlock {
                id: 0,
                text: "two".into(),
                anchors: vec![],
            }],
        )
        .unwrap()
        .is_none());
    assert!(cache
        .get(
            "k1",
            &[TextBlock {
                id: 0,
                text: "one".into(),
                anchors: vec![],
            }],
        )
        .unwrap()
        .is_some());

    // Advance past 60s TTL -> k1 and k3 expire!
    clock_ms.fetch_add(70_000, Ordering::SeqCst);
    assert!(cache
        .get(
            "k1",
            &[TextBlock {
                id: 0,
                text: "one".into(),
                anchors: vec![],
            }],
        )
        .unwrap()
        .is_none());
    assert!(cache
        .get(
            "k3",
            &[TextBlock {
                id: 0,
                text: "three".into(),
                anchors: vec![],
            }],
        )
        .unwrap()
        .is_none());

    // Logical clear + physical reclaim reports clean stats.
    let report = cache.clear().unwrap();
    assert!(report.physical_reclaim_succeeded);
    assert_eq!(report.stats_after.entry_count, 0);
    assert_eq!(report.stats_after.logical_bytes, 0);
}
