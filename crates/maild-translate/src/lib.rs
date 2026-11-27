//! Featherpost pure-Rust email translation core engine.
//!
//! Deliberately has ZERO dependency on Tauri or `maild-ui` so both the desktop UI shell and
//! PR 3's HTML/plain-text rewriter can consume it cleanly without circular dependencies.
//!
//! Modules:
//! - [`types`]: Pure-Rust ordered block/slot/batch DTOs, negotiated batch limits, response
//!   validator, Cordis [`TranslationProvider`] trait, and [`TranslationRegistry`] service.
//! - [`endpoint`]: Strict endpoint validation, normalization, transport classification, and
//!   service identity binding.
//! - [`config`]: No-follow `0600` (Unix) / user-private protected DACL (Windows) fail-closed
//!   [`TranslationConfigStore`] with credential-to-normalized-endpoint binding.
//! - [`openai`]: Built-in [`OpenAiCompatibleProvider`] routing through `maild-proxy`
//!   (`ProxyPurpose::Translation`) with first-snapshot guard, DNS pinning, redirect blocking,
//!   hard response/deadline caps, and narrow 400 `response_format` fallback.
//! - [`cache`]: Standalone content-addressed persistent SQLite [`TranslationCache`] with
//!   `clear_epoch` write-back guard and serialized out-of-transaction `VACUUM`.
//! - [`detect`]: Zero-dependency `<1ms` lightweight email language detector.
//! - [`engine`]: Multi-batch [`TranslationEngine`] orchestrating registry + cache atomically.

pub mod cache;
pub mod config;
pub mod detect;
pub mod endpoint;
pub mod engine;
pub mod error;
pub mod fs_security;
pub mod html_blocks;
pub mod openai;
pub mod plain_text;
pub mod types;

pub use cache::{
    CacheClearReport, CacheConfig, CacheKeyInput, CacheStats, CacheWriteInput, CacheWriteOutcome,
    CachedTranslation, PhysicalReclaimResult, TranslationCache, CACHE_SCHEMA_VERSION,
    DEFAULT_CACHE_MAX_ENTRIES, DEFAULT_CACHE_MAX_LOGICAL_BYTES, DEFAULT_CACHE_TTL_DAYS,
    TRANSLATION_CACHE_FILE,
};
pub use config::{
    OpenAiConfigUpdate, OpenAiConfigView, ResolvedOpenAiConfig, StoredOpenAiConfig,
    TranslationConfigStore, TranslationPreference, TranslationPreferenceUpdate,
    TranslationPreferenceView, CONFIG_FORMAT_VERSION, MAX_CONFIG_BYTES,
    TRANSLATION_PREFERENCE_FILE,
};
pub use detect::{
    detect_language, normalize_lang_tag, DetectedLanguage, DetectionResult, MAX_SAMPLE_CHARS,
    MIN_EFFECTIVE_CHARS,
};
pub use endpoint::{
    is_public_ip, EndpointTransport, NormalizedServiceIdentity, MAX_BASE_URL_BYTES,
};
pub use engine::{
    TranslateDocumentRequest, TranslateDocumentResult, TranslationEngine,
    CHUNKER_AND_PLACEHOLDER_VERSION, PROMPT_VERSION,
};
pub use error::{sanitize_http_status, sanitize_reqwest_error, TranslateError};
pub use html_blocks::{
    decode_html_entities, escape_html_text_with_br, extract_html_blocks, render_html_translation,
    ExtractedHtmlBlocks, RenderedHtmlTranslation, TRUSTED_TR_ATTR, TRUSTED_TR_VALUE,
};
pub use openai::{
    OpenAiCompatibleProvider, OpenAiHttpLimits, ProxySnapshotOutcome, DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_PROBE_DEADLINE, DEFAULT_TOTAL_DEADLINE,
};
pub use plain_text::{
    extract_plain_text, render_plain_text, ExtractedPlainText, PlainTextBlockView,
    RenderedPlainText,
};
pub use types::{
    plan_batches, validate_batch_response, AnchorSlot, BatchLimits, ChunkPlan, ContentKind,
    DisplayMode, PlannedBatch, ProbeResult, ProbeStage, ProviderCapabilities, ProviderKind,
    ProviderMetadata, TextBlock, TranslatedBlock, TranslationBatchRequest,
    TranslationBatchResponse, TranslationProvider, TranslationRegistry, BUILTIN_OPENAI_PROVIDER_ID,
    BUILTIN_PROVIDER_PREFIX, DEFAULT_MAX_EXPANSION_RATIO, HOST_DEFAULT_BATCH_LIMITS,
    HOST_MAX_BATCH_LIMITS, MAX_SINGLE_BLOCK_BYTES, MIN_TRANSLATED_ITEM_BYTES,
};
