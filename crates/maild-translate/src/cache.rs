//! Content-addressed persistent SQLite cache (`translation_cache.sqlite3`).
//!
//! Architecture & invariants:
//! - Standalone SQLite file inside a caller-supplied data directory namespace, naturally
//!   excluded from `crates/maild/src/store/backup.rs` main-DB backups.
//! - Private permissions (`0600` on Unix / user-private protected DACL on Windows) enforced on
//!   the main DB, `-wal`, and `-shm` sidecar files.
//! - Low-footprint PRAGMAs: `WAL`, `cache_size = -512`, `temp_store = FILE`, `mmap_size = 0`,
//!   `journal_size_limit = 2097152`.
//! - Deterministic typed & length-prefixed SHA-256 key covering `schema_ver`, resolved
//!   `source_lang`, resolved `target_lang`, `content_kind`, `chunker_and_placeholder_ver`,
//!   `prompt_ver`, ordered blocks + local anchor topology, `provider_id`, and
//!   `provider_variant_revision`.
//! - Atomic all-or-nothing writes; persisted `clear_epoch` prevents in-flight tasks from writing
//!   back after cache clear; logical delete takes effect immediately, followed by serialized
//!   out-of-transaction `VACUUM` + `wal_checkpoint(TRUNCATE)` reporting logical quota vs physical
//!   reclaim cleanly.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::detect::normalize_lang_tag;
use crate::error::TranslateError;
use crate::fs_security::secure_sqlite_files;
use crate::types::{
    validate_batch_response, ContentKind, TextBlock, TranslatedBlock, DEFAULT_MAX_EXPANSION_RATIO,
};

/// Standalone SQLite filename inside the caller-supplied data directory namespace.
pub const TRANSLATION_CACHE_FILE: &str = "translation_cache.sqlite3";

/// Current SQLite schema version (`PRAGMA user_version`) and cache key schema version.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Default maximum cached entries (logical quota).
pub const DEFAULT_CACHE_MAX_ENTRIES: usize = 2_000;

/// Default maximum cached payload size (32 MiB logical quota).
pub const DEFAULT_CACHE_MAX_LOGICAL_BYTES: usize = 32 * 1024 * 1024;

/// Default entry TTL (30 days).
pub const DEFAULT_CACHE_TTL_DAYS: u64 = 30;

/// Configuration for [`TranslationCache`] logical quotas and TTL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    pub max_entries: usize,
    pub max_logical_bytes: usize,
    pub ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_CACHE_MAX_ENTRIES,
            max_logical_bytes: DEFAULT_CACHE_MAX_LOGICAL_BYTES,
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_DAYS * 24 * 60 * 60),
        }
    }
}

/// Typed input to the deterministic content-addressed SHA-256 cache key.
#[derive(Clone, Debug)]
pub struct CacheKeyInput<'a> {
    pub schema_ver: u32,
    pub resolved_source_lang: &'a str,
    pub resolved_target_lang: &'a str,
    pub content_kind: ContentKind,
    pub chunker_and_placeholder_ver: u32,
    pub prompt_ver: u32,
    pub blocks: &'a [TextBlock],
    pub provider_id: &'a str,
    pub provider_variant_revision: &'a str,
}

impl<'a> CacheKeyInput<'a> {
    /// Compute a collision-resistant, typed and length-prefixed SHA-256 hex digest.
    ///
    /// Every field is tagged with a unique type byte and fixed-width little-endian length so
    /// differing field boundaries, block splits, or anchor topologies can NEVER alias.
    pub fn compute_key(&self) -> String {
        let mut h = Sha256::new();
        // Domain separator.
        h.update(b"fp-translate-cache-v1\0");

        hash_u32(&mut h, 0x01, self.schema_ver);
        hash_str(&mut h, 0x02, &normalize_lang_tag(self.resolved_source_lang));
        hash_str(&mut h, 0x03, &normalize_lang_tag(self.resolved_target_lang));
        hash_str(&mut h, 0x04, self.content_kind.as_str());
        hash_u32(&mut h, 0x05, self.chunker_and_placeholder_ver);
        hash_u32(&mut h, 0x06, self.prompt_ver);
        hash_str(&mut h, 0x07, self.provider_id.trim());
        hash_str(&mut h, 0x08, self.provider_variant_revision.trim());

        // Ordered blocks + local anchor topology.
        hash_u64(&mut h, 0x10, self.blocks.len() as u64);
        for block in self.blocks {
            hash_u32(&mut h, 0x11, block.id);
            hash_str(&mut h, 0x12, &block.text);
            hash_u64(&mut h, 0x13, block.anchors.len() as u64);
            for slot in &block.anchors {
                hash_u32(&mut h, 0x14, slot.slot_id);
                hash_u32(&mut h, 0x15, slot.link_index);
            }
        }

        hex::encode(h.finalize())
    }
}

fn hash_u32(h: &mut Sha256, tag: u8, val: u32) {
    h.update([tag]);
    h.update(val.to_le_bytes());
}

fn hash_u64(h: &mut Sha256, tag: u8, val: u64) {
    h.update([tag]);
    h.update(val.to_le_bytes());
}

fn hash_str(h: &mut Sha256, tag: u8, s: &str) {
    h.update([tag]);
    h.update((s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// Validated translation payload stored inside SQLite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTranslation {
    pub cache_key: String,
    pub provider_id: String,
    pub provider_variant_revision: String,
    pub content_kind: ContentKind,
    pub source_lang: String,
    pub target_lang: String,
    pub items: Vec<TranslatedBlock>,
    pub created_at_ms: i64,
}

/// Input to an atomic cache commit.
#[derive(Clone, Debug)]
pub struct CacheWriteInput {
    pub cache_key: String,
    pub provider_id: String,
    pub provider_variant_revision: String,
    pub content_kind: ContentKind,
    pub source_lang: String,
    pub target_lang: String,
    pub expected_blocks: Vec<TextBlock>,
    pub translated_blocks: Vec<TranslatedBlock>,
}

/// Result of an atomic cache write guarded by `clear_epoch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheWriteOutcome {
    Committed,
    /// A `clear()` occurred while the translation was in flight; write-back was discarded.
    DiscardedByClear,
}

/// Separate logical quota vs physical on-disk occupancy report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStats {
    pub entry_count: u64,
    pub logical_bytes: u64,
    pub max_entries: u64,
    pub max_logical_bytes: u64,
    pub ttl_days: u64,
    pub physical_db_bytes: u64,
    pub physical_wal_bytes: u64,
    pub physical_shm_bytes: u64,
    pub clear_epoch: u64,
}

/// Result of logical clear + serialized out-of-transaction physical reclaim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheClearReport {
    pub cleared_entries: u64,
    pub new_clear_epoch: u64,
    pub physical_reclaim_succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_reclaim_note: Option<String>,
    pub stats_after: CacheStats,
}

/// Result of running out-of-transaction physical reclaim (`VACUUM` + `wal_checkpoint(TRUNCATE)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalReclaimResult {
    pub succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub stats_after: CacheStats,
}

type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Standalone persistent content-addressed SQLite translation cache.
pub struct TranslationCache {
    db_path: PathBuf,
    config: CacheConfig,
    conn: Mutex<Connection>,
    /// Dedicated serialization lock for out-of-transaction `VACUUM` + `wal_checkpoint(TRUNCATE)`.
    reclaim_lock: Mutex<()>,
    clock: ClockFn,
}

impl TranslationCache {
    /// Open (or create) `translation_cache.sqlite3` inside `data_dir`.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, TranslateError> {
        Self::open_with_config(data_dir, CacheConfig::default())
    }

    /// Open (or create) `translation_cache.sqlite3` inside `data_dir` with custom limits.
    pub fn open_with_config(
        data_dir: impl AsRef<Path>,
        config: CacheConfig,
    ) -> Result<Self, TranslateError> {
        Self::open_inner(data_dir.as_ref(), config, Arc::new(system_now_ms))
    }

    /// Deterministic clock constructor for tests (e.g., TTL / LRU verification without sleeping).
    pub fn open_with_clock(
        data_dir: impl AsRef<Path>,
        config: CacheConfig,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Result<Self, TranslateError> {
        Self::open_inner(data_dir.as_ref(), config, Arc::new(clock))
    }

    fn open_inner(
        data_dir: &Path,
        config: CacheConfig,
        clock: ClockFn,
    ) -> Result<Self, TranslateError> {
        let db_path = data_dir.join(TRANSLATION_CACHE_FILE);
        secure_sqlite_files(&db_path)?;

        let conn = Connection::open(&db_path)
            .map_err(|_| TranslateError::Cache("failed to open translation cache DB".into()))?;

        apply_low_footprint_pragmas(&conn)?;
        init_schema(&conn)?;
        // Enforce private permissions again after WAL/SHM creation.
        secure_sqlite_files(&db_path)?;

        Ok(Self {
            db_path,
            config,
            conn: Mutex::new(conn),
            reclaim_lock: Mutex::new(()),
            clock,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Read the persisted monotonic `clear_epoch`. Callers capture this BEFORE launching
    /// provider translation batches so any intervening `clear()` prevents stale write-back.
    pub fn current_clear_epoch(&self) -> Result<u64, TranslateError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read_clear_epoch(&conn)
    }

    /// Look up a cached translation by deterministic key and re-validate every returned block
    /// against `expected_blocks` before returning.
    pub fn get(
        &self,
        cache_key: &str,
        expected_blocks: &[TextBlock],
    ) -> Result<Option<CachedTranslation>, TranslateError> {
        let now_ms = (self.clock)();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        struct CachedRow {
            provider_id: String,
            provider_variant_revision: String,
            kind_str: String,
            source_lang: String,
            target_lang: String,
            payload_json: String,
            created_at_ms: i64,
            expires_at_ms: i64,
        }

        let row: Option<CachedRow> = conn
            .query_row(
                "SELECT provider_id, provider_variant_revision, content_kind, source_lang, \
                        target_lang, payload_json, created_at_ms, expires_at_ms \
                 FROM translation_entries WHERE cache_key = ?1",
                params![cache_key],
                |r| {
                    Ok(CachedRow {
                        provider_id: r.get(0)?,
                        provider_variant_revision: r.get(1)?,
                        kind_str: r.get(2)?,
                        source_lang: r.get(3)?,
                        target_lang: r.get(4)?,
                        payload_json: r.get(5)?,
                        created_at_ms: r.get(6)?,
                        expires_at_ms: r.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|_| TranslateError::Cache("cache lookup failed".into()))?;

        let Some(CachedRow {
            provider_id,
            provider_variant_revision,
            kind_str,
            source_lang,
            target_lang,
            payload_json,
            created_at_ms,
            expires_at_ms,
        }) = row
        else {
            return Ok(None);
        };

        if expires_at_ms <= now_ms {
            let _ = conn.execute(
                "DELETE FROM translation_entries WHERE cache_key = ?1",
                params![cache_key],
            );
            return Ok(None);
        }

        let content_kind = match kind_str.as_str() {
            "html" => ContentKind::Html,
            "plain" => ContentKind::Plain,
            _ => {
                let _ = conn.execute(
                    "DELETE FROM translation_entries WHERE cache_key = ?1",
                    params![cache_key],
                );
                return Ok(None);
            }
        };

        let parsed: Vec<TranslatedBlock> = match serde_json::from_str(&payload_json) {
            Ok(v) => v,
            Err(_) => {
                let _ = conn.execute(
                    "DELETE FROM translation_entries WHERE cache_key = ?1",
                    params![cache_key],
                );
                return Ok(None);
            }
        };

        // Re-validate cached payload against expected block count/IDs/expansion on read.
        let validated =
            match validate_batch_response(expected_blocks, &parsed, DEFAULT_MAX_EXPANSION_RATIO) {
                Ok(items) => items,
                Err(_) => {
                    let _ = conn.execute(
                        "DELETE FROM translation_entries WHERE cache_key = ?1",
                        params![cache_key],
                    );
                    return Ok(None);
                }
            };

        let _ = conn.execute(
            "UPDATE translation_entries SET last_accessed_at_ms = ?1 WHERE cache_key = ?2",
            params![now_ms, cache_key],
        );

        Ok(Some(CachedTranslation {
            cache_key: cache_key.to_string(),
            provider_id,
            provider_variant_revision,
            content_kind,
            source_lang,
            target_lang,
            items: validated,
            created_at_ms,
        }))
    }

    /// Atomically commit a completed translation ONLY if:
    /// 1. Every translated block passes strict count/uniqueness/completeness validation.
    /// 2. The persisted `clear_epoch` still equals `expected_clear_epoch`.
    ///
    /// Also prunes expired entries and enforces logical entry count (2,000) and logical byte
    /// (32 MiB) LRU quotas within the same transaction.
    pub fn put_atomic(
        &self,
        input: CacheWriteInput,
        expected_clear_epoch: u64,
    ) -> Result<CacheWriteOutcome, TranslateError> {
        let validated = validate_batch_response(
            &input.expected_blocks,
            &input.translated_blocks,
            DEFAULT_MAX_EXPANSION_RATIO,
        )?;
        let payload_json = serde_json::to_string(&validated)
            .map_err(|_| TranslateError::Cache("failed to serialize cache payload".into()))?;
        let logical_bytes = payload_json.len();
        if logical_bytes > self.config.max_logical_bytes {
            return Err(TranslateError::Cache(
                "single cache entry exceeds logical byte quota".into(),
            ));
        }

        let now_ms = (self.clock)();
        let ttl_ms = i64::try_from(self.config.ttl.as_millis()).unwrap_or(i64::MAX / 2);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);

        let mut conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| TranslateError::Cache("failed to start cache transaction".into()))?;

        let current_epoch = read_clear_epoch(&tx)?;
        if current_epoch != expected_clear_epoch {
            // In-flight write-back arrived after a cache clear; discard without modifying cache.
            let _ = tx.rollback();
            return Ok(CacheWriteOutcome::DiscardedByClear);
        }

        tx.execute(
            "INSERT OR REPLACE INTO translation_entries (
                cache_key, provider_id, provider_variant_revision, content_kind,
                source_lang, target_lang, payload_json, logical_bytes,
                created_at_ms, last_accessed_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                input.cache_key,
                input.provider_id,
                input.provider_variant_revision,
                input.content_kind.as_str(),
                normalize_lang_tag(&input.source_lang),
                normalize_lang_tag(&input.target_lang),
                payload_json,
                logical_bytes as i64,
                now_ms,
                now_ms,
                expires_at_ms,
            ],
        )
        .map_err(|_| TranslateError::Cache("failed to upsert cache entry".into()))?;

        // Evict expired rows first.
        tx.execute(
            "DELETE FROM translation_entries WHERE expires_at_ms <= ?1",
            params![now_ms],
        )
        .map_err(|_| TranslateError::Cache("failed to evict expired cache entries".into()))?;

        // Enforce logical entry-count and logical-bytes LRU quotas.
        enforce_lru_quotas(&tx, self.config.max_entries, self.config.max_logical_bytes)?;

        tx.commit()
            .map_err(|_| TranslateError::Cache("failed to commit cache transaction".into()))?;
        drop(conn);

        let _ = secure_sqlite_files(&self.db_path);
        Ok(CacheWriteOutcome::Committed)
    }

    /// Logical clear: immediately increments persisted `clear_epoch` and deletes all rows in an
    /// atomic transaction. In-flight tasks captured under an older `clear_epoch` will be discarded.
    /// Returns `(cleared_entries, new_clear_epoch)`.
    pub fn clear_logical(&self) -> Result<(u64, u64), TranslateError> {
        let mut conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| TranslateError::Cache("failed to start clear transaction".into()))?;

        let cleared: usize = tx
            .execute("DELETE FROM translation_entries", [])
            .map_err(|_| TranslateError::Cache("failed to delete cache entries".into()))?;
        tx.execute(
            "UPDATE cache_meta SET value = value + 1 WHERE key = 'clear_epoch'",
            [],
        )
        .map_err(|_| TranslateError::Cache("failed to increment clear_epoch".into()))?;
        let new_epoch = read_clear_epoch(&tx)?;

        tx.commit()
            .map_err(|_| TranslateError::Cache("failed to commit logical clear".into()))?;

        Ok((cleared as u64, new_epoch))
    }

    /// Serialized out-of-transaction physical reclaim (`VACUUM` + `PRAGMA wal_checkpoint(TRUNCATE)`).
    /// Handles SQLite busy / active readers / disk pressure gracefully without failing logical state.
    pub fn reclaim_physical(&self) -> Result<PhysicalReclaimResult, TranslateError> {
        let _reclaim_guard = self
            .reclaim_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let (succeeded, note) = {
            let conn = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match conn.execute_batch("VACUUM;") {
                Ok(()) => match run_wal_checkpoint_truncate(&conn) {
                    Ok(0) => (true, None),
                    Ok(_) => (
                        false,
                        Some("wal_checkpoint_busy_active_reader".to_string()),
                    ),
                    Err(reason) => (false, Some(reason)),
                },
                Err(err) => (false, Some(classify_sqlite_reclaim_error(&err))),
            }
        };

        let _ = secure_sqlite_files(&self.db_path);
        let stats_after = self.stats()?;
        Ok(PhysicalReclaimResult {
            succeeded,
            note,
            stats_after,
        })
    }

    /// Clear all cached translations: performs immediate atomic logical clear (`clear_epoch += 1` +
    /// `DELETE`), then runs serialized out-of-transaction `VACUUM` + `wal_checkpoint(TRUNCATE)`
    /// and reports logical vs physical reclaim cleanly.
    pub fn clear(&self) -> Result<CacheClearReport, TranslateError> {
        let (cleared_entries, new_clear_epoch) = self.clear_logical()?;
        let reclaim = self.reclaim_physical()?;
        Ok(CacheClearReport {
            cleared_entries,
            new_clear_epoch,
            physical_reclaim_succeeded: reclaim.succeeded,
            physical_reclaim_note: reclaim.note,
            stats_after: reclaim.stats_after,
        })
    }

    /// Return current logical quota usage and physical DB/WAL/SHM file sizes.
    pub fn stats(&self) -> Result<CacheStats, TranslateError> {
        let (entry_count, logical_bytes, clear_epoch) = {
            let conn = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (count, bytes): (i64, i64) = conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(logical_bytes), 0) FROM translation_entries",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| TranslateError::Cache("failed to query cache stats".into()))?;
            let epoch = read_clear_epoch(&conn)?;
            (count.max(0) as u64, bytes.max(0) as u64, epoch)
        };

        let physical_db_bytes = file_len_or_zero(&self.db_path);
        let physical_wal_bytes = sidecar_len_or_zero(&self.db_path, "-wal");
        let physical_shm_bytes = sidecar_len_or_zero(&self.db_path, "-shm");

        Ok(CacheStats {
            entry_count,
            logical_bytes,
            max_entries: self.config.max_entries as u64,
            max_logical_bytes: self.config.max_logical_bytes as u64,
            ttl_days: self.config.ttl.as_secs() / (24 * 60 * 60),
            physical_db_bytes,
            physical_wal_bytes,
            physical_shm_bytes,
            clear_epoch,
        })
    }
}

fn apply_low_footprint_pragmas(conn: &Connection) -> Result<(), TranslateError> {
    for pragma in [
        "PRAGMA journal_mode = WAL",
        "PRAGMA synchronous = NORMAL",
        "PRAGMA cache_size = -512",
        "PRAGMA temp_store = FILE",
        "PRAGMA mmap_size = 0",
        "PRAGMA journal_size_limit = 2097152",
        "PRAGMA busy_timeout = 3000",
    ] {
        // Consume any returned rows so statements reset cleanly.
        let mut stmt = conn
            .prepare(pragma)
            .map_err(|_| TranslateError::Cache("failed to apply SQLite PRAGMA".into()))?;
        let mut rows = stmt
            .query([])
            .map_err(|_| TranslateError::Cache("failed to execute SQLite PRAGMA".into()))?;
        while rows
            .next()
            .map_err(|_| TranslateError::Cache("failed to drain SQLite PRAGMA".into()))?
            .is_some()
        {}
    }
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<(), TranslateError> {
    let user_ver: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| TranslateError::Cache("failed to read cache schema version".into()))?;

    if user_ver > CACHE_SCHEMA_VERSION {
        return Err(TranslateError::Cache(format!(
            "unsupported future cache schema version {user_ver}"
        )));
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cache_meta (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO cache_meta (key, value) VALUES ('clear_epoch', 0);
         CREATE TABLE IF NOT EXISTS translation_entries (
            cache_key TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            provider_variant_revision TEXT NOT NULL,
            content_kind TEXT NOT NULL,
            source_lang TEXT NOT NULL,
            target_lang TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            logical_bytes INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            last_accessed_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_translation_entries_lru
            ON translation_entries (last_accessed_at_ms ASC);
         CREATE INDEX IF NOT EXISTS idx_translation_entries_exp
            ON translation_entries (expires_at_ms ASC);",
    )
    .map_err(|_| TranslateError::Cache("failed to initialize cache schema".into()))?;

    if user_ver == 0 {
        conn.pragma_update(None, "user_version", CACHE_SCHEMA_VERSION)
            .map_err(|_| TranslateError::Cache("failed to set cache user_version".into()))?;
    }
    Ok(())
}

fn read_clear_epoch(conn: &Connection) -> Result<u64, TranslateError> {
    let val: i64 = conn
        .query_row(
            "SELECT value FROM cache_meta WHERE key = 'clear_epoch'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| TranslateError::Cache("failed to read clear_epoch".into()))?;
    Ok(val.max(0) as u64)
}

fn enforce_lru_quotas(
    conn: &Connection,
    max_entries: usize,
    max_logical_bytes: usize,
) -> Result<(), TranslateError> {
    let max_entries = max_entries.max(1) as i64;
    let max_bytes = max_logical_bytes.max(1) as i64;

    loop {
        let (count, bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(logical_bytes), 0) FROM translation_entries",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| TranslateError::Cache("failed to check cache quotas".into()))?;

        if count <= max_entries && bytes <= max_bytes {
            break;
        }

        let oldest_key: Option<String> = conn
            .query_row(
                "SELECT cache_key FROM translation_entries \
                 ORDER BY last_accessed_at_ms ASC, created_at_ms ASC, cache_key ASC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| TranslateError::Cache("failed to select LRU victim".into()))?;

        let Some(victim) = oldest_key else {
            break;
        };
        conn.execute(
            "DELETE FROM translation_entries WHERE cache_key = ?1",
            params![victim],
        )
        .map_err(|_| TranslateError::Cache("failed to evict LRU victim".into()))?;
    }
    Ok(())
}

fn run_wal_checkpoint_truncate(conn: &Connection) -> Result<i64, String> {
    let mut stmt = conn
        .prepare("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(|err| classify_sqlite_reclaim_error(&err))?;
    let busy: i64 = stmt
        .query_row([], |r| r.get(0))
        .map_err(|err| classify_sqlite_reclaim_error(&err))?;
    Ok(busy)
}

fn classify_sqlite_reclaim_error(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::SqliteFailure(code, _) => match code.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                "sqlite_busy".to_string()
            }
            rusqlite::ErrorCode::DiskFull => "sqlite_disk_full".to_string(),
            _ => "sqlite_reclaim_failed".to_string(),
        },
        _ => "sqlite_reclaim_failed".to_string(),
    }
}

fn file_len_or_zero(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn sidecar_len_or_zero(db_path: &Path, suffix: &str) -> u64 {
    let mut s = OsString::from(db_path.as_os_str());
    s.push(suffix);
    file_len_or_zero(Path::new(&s))
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
