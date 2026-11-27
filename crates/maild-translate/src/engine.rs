//! High-level translation orchestrator combining [`TranslationRegistry`], [`TranslationCache`],
//! negotiated [`BatchLimits`], frozen provider `(id, generation, variant_revision)` across
//! multi-batch jobs, atomic all-or-nothing cache commits, `force_refresh` failure preservation,
//! and `clear_epoch` write-back protection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cache::{
    CacheKeyInput, CacheWriteInput, CacheWriteOutcome, TranslationCache, CACHE_SCHEMA_VERSION,
};
use crate::error::TranslateError;
use crate::types::{
    plan_batches, validate_batch_response, BatchLimits, ContentKind, TextBlock, TranslatedBlock,
    TranslationBatchRequest, TranslationRegistry, DEFAULT_MAX_EXPANSION_RATIO,
    HOST_DEFAULT_BATCH_LIMITS,
};

/// Current version of block chunking and placeholder formatting (included in cache key).
pub const CHUNKER_AND_PLACEHOLDER_VERSION: u32 = 1;

/// Current version of built-in prompt contract (included in cache key).
pub const PROMPT_VERSION: u32 = 1;

/// Request to translate an ordered set of text blocks through a specific provider.
#[derive(Clone, Debug)]
pub struct TranslateDocumentRequest {
    pub job_id: String,
    pub provider_id: String,
    /// Optional required provider generation; if set and mismatched, fails closed immediately.
    pub expected_generation: Option<u64>,
    pub resolved_source_lang: String,
    pub target_lang: String,
    pub content_kind: ContentKind,
    pub blocks: Vec<TextBlock>,
    /// When true, bypasses reading from cache, calls the provider, and ONLY replaces the cached
    /// entry if ALL batches succeed (preserving the previous valid cache entry on failure).
    pub force_refresh: bool,
    pub host_limits: BatchLimits,
    pub total_deadline: Option<Duration>,
    pub cancel_flag: Option<Arc<AtomicBool>>,
}

impl TranslateDocumentRequest {
    pub fn new(
        job_id: impl Into<String>,
        provider_id: impl Into<String>,
        resolved_source_lang: impl Into<String>,
        target_lang: impl Into<String>,
        content_kind: ContentKind,
        blocks: Vec<TextBlock>,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            provider_id: provider_id.into(),
            expected_generation: None,
            resolved_source_lang: resolved_source_lang.into(),
            target_lang: target_lang.into(),
            content_kind,
            blocks,
            force_refresh: false,
            host_limits: HOST_DEFAULT_BATCH_LIMITS,
            total_deadline: None,
            cancel_flag: None,
        }
    }
}

/// Completed translation output from [`TranslationEngine::translate_document`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateDocumentResult {
    pub cache_key: String,
    pub from_cache: bool,
    pub cache_write: Option<CacheWriteOutcome>,
    pub provider_id: String,
    pub provider_generation: u64,
    pub provider_variant_revision: String,
    pub items: Vec<TranslatedBlock>,
}

/// Pure-Rust translation engine orchestrator.
pub struct TranslationEngine {
    registry: Arc<TranslationRegistry>,
    cache: Arc<TranslationCache>,
}

impl TranslationEngine {
    pub fn new(registry: Arc<TranslationRegistry>, cache: Arc<TranslationCache>) -> Self {
        Self { registry, cache }
    }

    pub fn registry(&self) -> &Arc<TranslationRegistry> {
        &self.registry
    }

    pub fn cache(&self) -> &Arc<TranslationCache> {
        &self.cache
    }

    /// Translate ordered blocks end-to-end with cache lookup, frozen provider generation &
    /// variant revision across all batches, negotiated batch limits, atomic commit on full
    /// success, and `clear_epoch` race guard.
    pub async fn translate_document(
        &self,
        req: TranslateDocumentRequest,
    ) -> Result<TranslateDocumentResult, TranslateError> {
        let provider = match req.expected_generation {
            Some(gen) => self.registry.get_with_generation(&req.provider_id, gen)?,
            None => self.registry.get(&req.provider_id).ok_or_else(|| {
                TranslateError::ProviderUnavailable("provider not registered".into())
            })?,
        };

        // Freeze provider identity, generation, and variant revision for the ENTIRE document so
        // an in-flight upgrade can never stitch cross-generation batches into one cache entry.
        let frozen_meta = provider.metadata();
        let cache_key = CacheKeyInput {
            schema_ver: CACHE_SCHEMA_VERSION,
            resolved_source_lang: &req.resolved_source_lang,
            resolved_target_lang: &req.target_lang,
            content_kind: req.content_kind,
            chunker_and_placeholder_ver: CHUNKER_AND_PLACEHOLDER_VERSION,
            prompt_ver: PROMPT_VERSION,
            blocks: &req.blocks,
            provider_id: &frozen_meta.id,
            provider_variant_revision: &frozen_meta.variant_revision,
        }
        .compute_key();

        if req.blocks.is_empty() {
            return Ok(TranslateDocumentResult {
                cache_key,
                from_cache: true,
                cache_write: None,
                provider_id: frozen_meta.id,
                provider_generation: frozen_meta.generation,
                provider_variant_revision: frozen_meta.variant_revision,
                items: Vec::new(),
            });
        }

        // Capture clear_epoch BEFORE checking/starting translation so any concurrent cache clear
        // prevents stale write-back.
        let captured_clear_epoch = self.cache.current_clear_epoch()?;

        if !req.force_refresh {
            if let Some(cached) = self.cache.get(&cache_key, &req.blocks)? {
                return Ok(TranslateDocumentResult {
                    cache_key,
                    from_cache: true,
                    cache_write: None,
                    provider_id: cached.provider_id,
                    provider_generation: frozen_meta.generation,
                    provider_variant_revision: cached.provider_variant_revision,
                    items: cached.items,
                });
            }
        }

        // Negotiate effective batch limits: min(host_limit, provider_limit).
        let effective_limits =
            BatchLimits::negotiate(req.host_limits, frozen_meta.capabilities.limits);
        let plan = plan_batches(&req.blocks, effective_limits)?;
        let deadline = req.total_deadline.map(|d| Instant::now() + d);

        let mut chunk_outputs: HashMap<u32, String> = HashMap::new();
        for (idx, batch) in plan.batches.iter().enumerate() {
            if req
                .cancel_flag
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                let _ = provider.cancel(&req.job_id).await;
                return Err(TranslateError::Cancelled);
            }

            // Re-verify provider generation and variant_revision before every batch so a mid-job
            // hot-swap/upgrade fails closed instead of stitching mixed outputs.
            let current_provider = self
                .registry
                .get_with_generation(&frozen_meta.id, frozen_meta.generation)?;
            if current_provider.metadata().variant_revision != frozen_meta.variant_revision {
                return Err(TranslateError::ProviderUnavailable(
                    "provider variant revision changed mid-job".into(),
                ));
            }

            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    return Err(TranslateError::Timeout);
                }
            }

            let batch_req = TranslationBatchRequest {
                job_id: req.job_id.clone(),
                batch_index: idx as u32,
                source_lang: (!req.resolved_source_lang.is_empty()
                    && !req.resolved_source_lang.eq_ignore_ascii_case("unknown"))
                .then(|| req.resolved_source_lang.clone()),
                target_lang: req.target_lang.clone(),
                content_kind: req.content_kind,
                items: batch.items.clone(),
                deadline,
            };

            let resp = current_provider.translate_batch(batch_req).await?;
            let validated = validate_batch_response(
                &batch.items,
                &resp.items,
                DEFAULT_MAX_EXPANSION_RATIO,
            )?;
            for item in validated {
                chunk_outputs.insert(item.id, item.text);
            }
        }

        // Reassemble any split oversized single blocks and validate the full document result.
        let assembled = plan.reassemble(&chunk_outputs)?;
        let final_items =
            validate_batch_response(&req.blocks, &assembled, DEFAULT_MAX_EXPANSION_RATIO)?;

        // Atomic all-or-nothing commit guarded by captured clear_epoch.
        // Note: on force_refresh failure above, we returned Err BEFORE reaching here, leaving any
        // previous valid cache entry completely untouched!
        let write_outcome = self.cache.put_atomic(
            CacheWriteInput {
                cache_key: cache_key.clone(),
                provider_id: frozen_meta.id.clone(),
                provider_variant_revision: frozen_meta.variant_revision.clone(),
                content_kind: req.content_kind,
                source_lang: req.resolved_source_lang,
                target_lang: req.target_lang,
                expected_blocks: req.blocks,
                translated_blocks: final_items.clone(),
            },
            captured_clear_epoch,
        )?;

        Ok(TranslateDocumentResult {
            cache_key,
            from_cache: false,
            cache_write: Some(write_outcome),
            provider_id: frozen_meta.id,
            provider_generation: frozen_meta.generation,
            provider_variant_revision: frozen_meta.variant_revision,
            items: final_items,
        })
    }
}
