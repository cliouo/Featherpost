//! Pure-Rust core DTOs, batch partitioning, response validation, and the Cordis
//! `TranslationProvider` / `TranslationRegistry` contract.
//!
//! Designed with zero Tauri or UI dependencies so both PR 3's HTML/plain-text rewriter and
//! `maild-ui`'s `ExternalTranslationProviderAdapter` can consume these types directly.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use cordis::Service;
use serde::{Deserialize, Serialize};

use crate::error::TranslateError;

/// Reserved provider ID prefix for built-in providers. External plugins MUST NOT claim it.
pub const BUILTIN_PROVIDER_PREFIX: &str = "builtin.";

/// Built-in OpenAI-compatible provider identifier.
pub const BUILTIN_OPENAI_PROVIDER_ID: &str = "builtin.openai_compatible";

/// Default host-side batch limits used when negotiating with providers.
pub const HOST_DEFAULT_BATCH_LIMITS: BatchLimits = BatchLimits {
    max_batch_items: 24,
    max_batch_chars: 12_000,
    max_concurrency: 2,
};

/// Absolute upper ceilings enforced by the host regardless of what a provider advertises.
pub const HOST_MAX_BATCH_LIMITS: BatchLimits = BatchLimits {
    max_batch_items: 64,
    max_batch_chars: 32_000,
    max_concurrency: 4,
};

/// Maximum UTF-8 byte length permitted for a single input block before rejection.
pub const MAX_SINGLE_BLOCK_BYTES: usize = 256 * 1024;

/// Default maximum ratio of translated byte length to source byte length (with minimum slack).
pub const DEFAULT_MAX_EXPANSION_RATIO: usize = 8;

/// Minimum allowed byte budget per translated item (so short strings like "OK" -> multi-byte UTF-8 pass).
pub const MIN_TRANSLATED_ITEM_BYTES: usize = 1024;

/// Source document format for translation and cache partitioning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Html,
    Plain,
}

impl ContentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Plain => "plain",
        }
    }
}

/// Reading pane presentation mode configured in Settings -> Translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    #[default]
    Bilingual,
    TranslatedOnly,
}

impl DisplayMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bilingual => "bilingual",
            Self::TranslatedOnly => "translated_only",
        }
    }
}

/// Local link placeholder topology within a [`TextBlock`].
///
/// Only local slot/link indices are stored in core DTOs and cache keys; session-bound
/// document tokens and external URLs always come from the active document at render time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnchorSlot {
    /// 1-based placeholder index inside `TextBlock::text` (`[[1]]...[[/1]]`).
    pub slot_id: u32,
    /// Stable local link index inside the document's link map.
    pub link_index: u32,
}

/// An ordered, translatable text unit (e.g. `id = 0` for subject, `1..` for body blocks).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextBlock {
    pub id: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<AnchorSlot>,
}

/// Translated output corresponding to a [`TextBlock`] by `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslatedBlock {
    pub id: u32,
    pub text: String,
}

/// Batch size and concurrency limits for a translation provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchLimits {
    pub max_batch_items: usize,
    pub max_batch_chars: usize,
    pub max_concurrency: usize,
}

impl Default for BatchLimits {
    fn default() -> Self {
        HOST_DEFAULT_BATCH_LIMITS
    }
}

impl BatchLimits {
    /// Negotiate effective batch limits by taking `min(host_limit, provider_limit)` for each
    /// dimension, clamped into `[1, HOST_MAX_BATCH_LIMITS]`.
    pub fn negotiate(host: BatchLimits, provider: BatchLimits) -> BatchLimits {
        let host_clamped = host.clamped_to_host_ceiling();
        let provider_clamped = provider.clamped_to_host_ceiling();
        BatchLimits {
            max_batch_items: host_clamped
                .max_batch_items
                .min(provider_clamped.max_batch_items)
                .max(1),
            max_batch_chars: host_clamped
                .max_batch_chars
                .min(provider_clamped.max_batch_chars)
                .max(1),
            max_concurrency: host_clamped
                .max_concurrency
                .min(provider_clamped.max_concurrency)
                .max(1),
        }
    }

    fn clamped_to_host_ceiling(self) -> BatchLimits {
        BatchLimits {
            max_batch_items: self
                .max_batch_items
                .clamp(1, HOST_MAX_BATCH_LIMITS.max_batch_items),
            max_batch_chars: self
                .max_batch_chars
                .clamp(1, HOST_MAX_BATCH_LIMITS.max_batch_chars),
            max_concurrency: self
                .max_concurrency
                .clamp(1, HOST_MAX_BATCH_LIMITS.max_concurrency),
        }
    }
}

/// Whether a provider is built-in or an external Cordis plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Builtin,
    External,
}

/// Capabilities declared by a translation provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    #[serde(default)]
    pub supported_languages: Vec<String>,
    #[serde(default)]
    pub limits: BatchLimits,
    #[serde(default)]
    pub supports_cancel: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            supported_languages: Vec::new(),
            limits: HOST_DEFAULT_BATCH_LIMITS,
            supports_cancel: true,
        }
    }
}

/// Immutable snapshot of a registered provider's identity and capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    pub version: String,
    /// Cache-relevant revision excluding secrets (normalized endpoint + model for built-in;
    /// plugin version + `variant_revision` for external plugins).
    pub variant_revision: String,
    /// Monotonic generation used to prevent stale cross-generation batch stitching.
    pub generation: u64,
    pub capabilities: ProviderCapabilities,
}

/// Single batch request dispatched to a [`TranslationProvider`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationBatchRequest {
    pub job_id: String,
    pub batch_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lang: Option<String>,
    pub target_lang: String,
    pub content_kind: ContentKind,
    pub items: Vec<TextBlock>,
    /// Optional total deadline budget shared across retries and batches.
    #[serde(skip)]
    pub deadline: Option<Instant>,
}

/// Single batch response returned by a [`TranslationProvider`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationBatchResponse {
    pub items: Vec<TranslatedBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_source_lang: Option<String>,
}

/// Structured stage reported by `probe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStage {
    Config,
    Proxy,
    Network,
    Tls,
    Auth,
    Upstream,
    Response,
    Done,
}

/// Sanitized result returned by [`TranslationProvider::probe`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub stage: ProbeStage,
    pub reason: String,
    pub latency_ms: u64,
    /// Sanitized route string (never contains proxy credentials or API keys).
    pub route: String,
    /// True when the configured endpoint uses plaintext HTTP on a non-loopback LAN host.
    pub plaintext_lan_transport: bool,
}

/// Core Cordis translation provider trait implemented by built-in and external adapters.
#[async_trait]
pub trait TranslationProvider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;

    async fn translate_batch(
        &self,
        request: TranslationBatchRequest,
    ) -> Result<TranslationBatchResponse, TranslateError>;

    async fn probe(&self, target_lang: &str) -> ProbeResult;

    async fn cancel(&self, _job_id: &str) -> Result<(), TranslateError> {
        Ok(())
    }
}

/// Cordis service managing built-in and external [`TranslationProvider`] instances.
#[derive(Default)]
pub struct TranslationRegistry {
    providers: RwLock<BTreeMap<String, Arc<dyn TranslationProvider>>>,
}

impl Service for TranslationRegistry {
    fn name(&self) -> &'static str {
        "translation.registry"
    }
}

impl TranslationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a built-in provider. Its ID MUST start with `builtin.`.
    pub fn register_builtin(
        &self,
        provider: Arc<dyn TranslationProvider>,
    ) -> Result<(), TranslateError> {
        let meta = provider.metadata();
        if meta.kind != ProviderKind::Builtin || !meta.id.starts_with(BUILTIN_PROVIDER_PREFIX) {
            return Err(TranslateError::InvalidConfig(
                "built-in provider id must use builtin.* prefix".into(),
            ));
        }
        let mut guard = self
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(meta.id, provider);
        Ok(())
    }

    /// Register an external provider. Rejects reserved `builtin.*` IDs and stale generations.
    pub fn register_external(
        &self,
        provider: Arc<dyn TranslationProvider>,
    ) -> Result<(), TranslateError> {
        let meta = provider.metadata();
        if meta.kind != ProviderKind::External || meta.id.is_empty() {
            return Err(TranslateError::InvalidConfig(
                "invalid external provider metadata".into(),
            ));
        }
        if meta.id.starts_with(BUILTIN_PROVIDER_PREFIX) {
            return Err(TranslateError::InvalidConfig(
                "external providers cannot claim reserved builtin.* namespace".into(),
            ));
        }
        let mut guard = self
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = guard.get(&meta.id) {
            if meta.generation < existing.metadata().generation {
                return Err(TranslateError::ProviderUnavailable(
                    "stale external provider generation".into(),
                ));
            }
        }
        guard.insert(meta.id, provider);
        Ok(())
    }

    /// Unregister any provider by ID.
    pub fn unregister(&self, id: &str) -> bool {
        let mut guard = self
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(id).is_some()
    }

    /// Unregister a provider only if its current generation matches `expected_generation`.
    pub fn unregister_if_generation(&self, id: &str, expected_generation: u64) -> bool {
        let mut guard = self
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard
            .get(id)
            .is_some_and(|p| p.metadata().generation == expected_generation)
        {
            guard.remove(id);
            true
        } else {
            false
        }
    }

    /// Look up a provider by exact ID. Never falls back to another provider.
    pub fn get(&self, id: &str) -> Option<Arc<dyn TranslationProvider>> {
        let guard = self
            .providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).cloned()
    }

    /// Look up a provider by exact ID and required generation.
    /// Fails closed with [`TranslateError::ProviderUnavailable`] if missing or generation changed.
    pub fn get_with_generation(
        &self,
        id: &str,
        expected_generation: u64,
    ) -> Result<Arc<dyn TranslationProvider>, TranslateError> {
        let provider = self
            .get(id)
            .ok_or_else(|| TranslateError::ProviderUnavailable("provider not registered".into()))?;
        let actual_gen = provider.metadata().generation;
        if actual_gen != expected_generation {
            return Err(TranslateError::ProviderUnavailable(
                "provider generation changed".into(),
            ));
        }
        Ok(provider)
    }

    /// Return metadata for all registered providers in deterministic ID order.
    pub fn list(&self) -> Vec<ProviderMetadata> {
        let guard = self
            .providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.values().map(|p| p.metadata()).collect()
    }
}

/// A planned translation batch, including reassembly metadata for split oversized blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedBatch {
    pub items: Vec<TextBlock>,
}

/// Mapping from an expanded batch item back to an original block (or chunk of an oversized block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    pub batches: Vec<PlannedBatch>,
    /// For each original block ID, the ordered list of synthetic chunk IDs that compose it.
    pub reassembly: Vec<(u32, Vec<u32>)>,
}

impl ChunkPlan {
    /// Reassemble translated synthetic chunk outputs back into the original ordered [`TranslatedBlock`]s.
    pub fn reassemble(
        &self,
        chunk_outputs: &HashMap<u32, String>,
    ) -> Result<Vec<TranslatedBlock>, TranslateError> {
        let mut result = Vec::with_capacity(self.reassembly.len());
        for (orig_id, chunk_ids) in &self.reassembly {
            if chunk_ids.len() == 1 {
                let text = chunk_outputs
                    .get(&chunk_ids[0])
                    .ok_or_else(|| {
                        TranslateError::InvalidResponse("missing translated block".into())
                    })?
                    .clone();
                result.push(TranslatedBlock { id: *orig_id, text });
            } else {
                let mut combined = String::new();
                for (idx, cid) in chunk_ids.iter().enumerate() {
                    let piece = chunk_outputs.get(cid).ok_or_else(|| {
                        TranslateError::InvalidResponse("missing translated block chunk".into())
                    })?;
                    if idx > 0 && !combined.is_empty() && !piece.is_empty() {
                        let last = combined.chars().last().unwrap_or(' ');
                        let first = piece.chars().next().unwrap_or(' ');
                        if !last.is_whitespace()
                            && !first.is_whitespace()
                            && last.is_ascii_alphanumeric()
                            && first.is_ascii_alphanumeric()
                        {
                            combined.push(' ');
                        }
                    }
                    combined.push_str(piece);
                }
                result.push(TranslatedBlock {
                    id: *orig_id,
                    text: combined,
                });
            }
        }
        Ok(result)
    }
}

/// Partition ordered [`TextBlock`]s into batches honoring negotiated `limits`.
///
/// If a single block exceeds `limits.max_batch_chars` (and has no anchors crossing split boundaries
/// or exceeds char cap), it is deterministically split on sentence / whitespace / UTF-8 boundaries
/// into chunks of at most `max_batch_chars` chars so oversized single paragraphs never overflow
/// provider batch limits, then reassembled via [`ChunkPlan::reassemble`].
pub fn plan_batches(
    blocks: &[TextBlock],
    limits: BatchLimits,
) -> Result<ChunkPlan, TranslateError> {
    let mut seen_ids = HashSet::with_capacity(blocks.len());
    for block in blocks {
        if !seen_ids.insert(block.id) {
            return Err(TranslateError::InvalidConfig(
                "duplicate input block id".into(),
            ));
        }
        if block.text.len() > MAX_SINGLE_BLOCK_BYTES {
            return Err(TranslateError::InvalidConfig(
                "single text block exceeds maximum size limit".into(),
            ));
        }
    }

    let max_items = limits.max_batch_items.max(1);
    let max_chars = limits.max_batch_chars.max(1);

    let mut expanded: Vec<TextBlock> = Vec::with_capacity(blocks.len());
    let mut reassembly: Vec<(u32, Vec<u32>)> = Vec::with_capacity(blocks.len());
    let mut next_synthetic_id: u32 = 0;

    for block in blocks {
        let char_count = block.text.chars().count();
        if char_count <= max_chars {
            let sid = next_synthetic_id;
            next_synthetic_id = next_synthetic_id
                .checked_add(1)
                .ok_or_else(|| TranslateError::InvalidConfig("too many blocks".into()))?;
            expanded.push(TextBlock {
                id: sid,
                text: block.text.clone(),
                anchors: block.anchors.clone(),
            });
            reassembly.push((block.id, vec![sid]));
        } else {
            let pieces = split_text_preserving_placeholders(&block.text, max_chars);
            let mut chunk_ids = Vec::with_capacity(pieces.len());
            for piece in pieces {
                let sid = next_synthetic_id;
                next_synthetic_id = next_synthetic_id
                    .checked_add(1)
                    .ok_or_else(|| TranslateError::InvalidConfig("too many blocks".into()))?;
                expanded.push(TextBlock {
                    id: sid,
                    text: piece,
                    anchors: Vec::new(),
                });
                chunk_ids.push(sid);
            }
            reassembly.push((block.id, chunk_ids));
        }
    }

    let mut batches = Vec::new();
    let mut current_items: Vec<TextBlock> = Vec::new();
    let mut current_chars: usize = 0;

    for item in expanded {
        let item_chars = item.text.chars().count().max(1);
        if !current_items.is_empty()
            && (current_items.len() >= max_items || current_chars + item_chars > max_chars)
        {
            batches.push(PlannedBatch {
                items: std::mem::take(&mut current_items),
            });
            current_chars = 0;
        }
        current_chars += item_chars;
        current_items.push(item);
    }
    if !current_items.is_empty() {
        batches.push(PlannedBatch {
            items: current_items,
        });
    }

    Ok(ChunkPlan {
        batches,
        reassembly,
    })
}

/// Split an oversized string into chunks of at most `max_chars` Unicode scalar values,
/// preferring sentence boundaries, newlines, or whitespace, while never splitting inside
/// a `[[k]]...[[/k]]` placeholder span when avoidable.
fn split_text_preserving_placeholders(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let total_chars = remaining.chars().count();
        if total_chars <= max_chars {
            chunks.push(remaining.to_string());
            break;
        }

        // Find byte offset of the `max_chars`-th char.
        let hard_byte = remaining
            .char_indices()
            .nth(max_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(remaining.len());

        let window = &remaining[..hard_byte];
        // Avoid splitting inside an open `[[n]]...[[/n]]` tag if possible.
        let safe_end = avoid_open_placeholder_cut(window);
        let search_slice = if safe_end > 0 {
            &remaining[..safe_end]
        } else {
            window
        };

        let split_at = find_best_break(search_slice).unwrap_or(search_slice.len());
        let split_at = if split_at == 0 {
            // Guarantee forward progress even on pathological single-char limit.
            remaining
                .char_indices()
                .nth(1)
                .map(|(idx, _)| idx)
                .unwrap_or(remaining.len())
        } else {
            split_at
        };

        let (head, tail) = remaining.split_at(split_at);
        chunks.push(head.to_string());
        remaining = tail.trim_start_matches([' ', '\t']);
    }

    chunks
}

fn avoid_open_placeholder_cut(window: &str) -> usize {
    // If there is an unmatched `[[` open tag without a matching `[[/` close after it,
    // back up to before the opening `[[` so we don't tear a link slot in half.
    if let Some(open_pos) = window.rfind("[[") {
        let after = &window[open_pos..];
        if !after.starts_with("[[/") {
            // Check if a closing `[[/` exists after `open_pos` inside window.
            if !after.contains("[[/") && open_pos > 0 {
                return open_pos;
            }
        } else if !after.contains("]]") && open_pos > 0 {
            // Partial closing tag `[[/1` at window edge.
            return open_pos;
        }
    }
    window.len()
}

fn find_best_break(slice: &str) -> Option<usize> {
    let min_keep = slice.len() / 3;
    // 1. Prefer newline.
    if let Some(pos) = slice.rfind('\n') {
        if pos >= min_keep {
            return Some(pos + 1);
        }
    }
    // 2. Prefer sentence-ending punctuation.
    for (idx, ch) in slice.char_indices().rev() {
        if idx < min_keep {
            break;
        }
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？' | ';' | '；') {
            return Some(idx + ch.len_utf8());
        }
    }
    // 3. Prefer whitespace.
    for (idx, ch) in slice.char_indices().rev() {
        if idx < min_keep {
            break;
        }
        if ch.is_whitespace() {
            return Some(idx + ch.len_utf8());
        }
    }
    None
}

/// Strictly validate translated batch outputs against expected input blocks:
/// - Exact count match
/// - Unique and complete ID set (no duplicates, no missing, no unknown IDs)
/// - Per-item byte expansion cap
///
/// Returns items ordered identically to `expected`.
pub fn validate_batch_response(
    expected: &[TextBlock],
    returned: &[TranslatedBlock],
    max_expansion_ratio: usize,
) -> Result<Vec<TranslatedBlock>, TranslateError> {
    if returned.len() != expected.len() {
        return Err(TranslateError::InvalidResponse(format!(
            "item count mismatch (expected {}, got {})",
            expected.len(),
            returned.len()
        )));
    }

    let expected_map: HashMap<u32, &TextBlock> = expected.iter().map(|b| (b.id, b)).collect();
    let mut returned_map: HashMap<u32, &str> = HashMap::with_capacity(returned.len());

    for item in returned {
        let Some(source) = expected_map.get(&item.id) else {
            return Err(TranslateError::InvalidResponse(
                "response contained unknown item id".into(),
            ));
        };
        if returned_map.insert(item.id, item.text.as_str()).is_some() {
            return Err(TranslateError::InvalidResponse(
                "response contained duplicate item id".into(),
            ));
        }
        let max_bytes = source
            .text
            .len()
            .saturating_mul(max_expansion_ratio.max(1))
            .max(MIN_TRANSLATED_ITEM_BYTES);
        if item.text.len() > max_bytes {
            return Err(TranslateError::InvalidResponse(
                "translated item exceeded expansion ratio cap".into(),
            ));
        }
    }

    let mut ordered = Vec::with_capacity(expected.len());
    for source in expected {
        let Some(text) = returned_map.get(&source.id) else {
            return Err(TranslateError::InvalidResponse(
                "response missing expected item id".into(),
            ));
        };
        ordered.push(TranslatedBlock {
            id: source.id,
            text: (*text).to_string(),
        });
    }

    Ok(ordered)
}
