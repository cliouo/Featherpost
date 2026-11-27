//! No-follow `0600` fail-closed configuration store for `translation_preference.toml`.
//!
//! Key security invariants:
//! - Lives in the caller-supplied active config directory (so PR 3's `maild-ui` passes the true
//!   `--config` parent directory).
//! - `format_version = 1`, `#[serde(deny_unknown_fields)]`, bounded `64 KiB + 1` read on a
//!   no-follow opened file handle (Unix: regular file, current user owner, `0600`, `nlink == 1`;
//!   Windows: non-reparse regular file, `nlink == 1`, user-private protected DACL).
//! - Fail-closed on corruption or unknown future `format_version`: disables auto-translate,
//!   refuses overwrite, and never prints raw TOML source lines to stderr.
//! - Cross-process exclusive file lock + revision check + atomic exclusive `0600` temp file write
//!   with `fsync` + `rename` + parent dir sync.
//! - P0 Credential-to-Normalized-Endpoint Binding: stored `api_key` is wrapped in
//!   [`Zeroizing<String>`], redacted in `Debug`, never exposed in read DTOs, and strictly bound
//!   to [`NormalizedServiceIdentity::canonical_key`]. Changing Base URL in either save or draft
//!   probe NEVER inherits the old key; simultaneous `clear_api_key: true` + non-empty `api_key`
//!   is rejected.
//! - `auto_translate` is explicitly bound to `auto_translate_provider_id`: changing the active
//!   provider automatically disables `auto_translate` until explicitly re-confirmed for the new
//!   provider.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::endpoint::{EndpointTransport, NormalizedServiceIdentity};
use crate::error::TranslateError;
use crate::fs_security::{
    acquire_process_lock, publish_private_atomically, read_private_file_bounded,
};
use crate::types::{DisplayMode, BUILTIN_OPENAI_PROVIDER_ID};

/// Preference file name stored inside the active config directory.
pub const TRANSLATION_PREFERENCE_FILE: &str = "translation_preference.toml";
const TRANSLATION_LOCK_FILE: &str = ".translation_preference.toml.lock";
pub const CONFIG_FORMAT_VERSION: i64 = 1;
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_MODEL_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 4096;
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_IGNORED_LANGUAGES: usize = 64;
const MAX_LANG_TAG_BYTES: usize = 32;

/// On-disk TOML schema (`format_version = 1`, `deny_unknown_fields`).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceFile {
    format_version: i64,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    active_provider_id: String,
    #[serde(default)]
    display_mode: DisplayMode,
    #[serde(default = "default_target_lang")]
    target_lang: String,
    #[serde(default = "default_true")]
    detect_banner_enabled: bool,
    #[serde(default)]
    auto_translate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_translate_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ignored_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openai_compatible: Option<StoredOpenAiFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredOpenAiFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    base_url: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key_bound_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(default)]
    allow_plaintext_lan: bool,
}

fn default_target_lang() -> String {
    "zh-CN".to_string()
}

const fn default_true() -> bool {
    true
}

/// In-memory stored OpenAI-compatible provider settings with zeroized secret and redacted Debug.
#[derive(Clone)]
pub struct StoredOpenAiConfig {
    pub preset: Option<String>,
    pub base_url: String,
    pub identity: NormalizedServiceIdentity,
    pub model: String,
    pub api_key_bound_identity: Option<String>,
    pub api_key: Option<Zeroizing<String>>,
    pub allow_plaintext_lan: bool,
}

impl fmt::Debug for StoredOpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredOpenAiConfig")
            .field("preset", &self.preset)
            .field("base_url", &self.base_url)
            .field("identity", &self.identity.canonical_key())
            .field("model", &self.model)
            .field("api_key_bound_identity", &self.api_key_bound_identity)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("allow_plaintext_lan", &self.allow_plaintext_lan)
            .finish()
    }
}

impl StoredOpenAiConfig {
    pub fn has_api_key(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }

    pub fn to_view(&self) -> OpenAiConfigView {
        OpenAiConfigView {
            preset: self.preset.clone(),
            base_url: self.base_url.clone(),
            normalized_identity: self.identity.canonical_key(),
            transport: self.identity.transport,
            plaintext_lan_transport: self.identity.transport.is_plaintext_lan(),
            model: self.model.clone(),
            has_api_key: self.has_api_key(),
            bound_service_identity: self.api_key_bound_identity.clone(),
            allow_plaintext_lan: self.allow_plaintext_lan,
        }
    }

    pub fn to_resolved(&self, revision: u64) -> ResolvedOpenAiConfig {
        ResolvedOpenAiConfig {
            identity: self.identity.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            allow_plaintext_lan: self.allow_plaintext_lan,
            revision,
        }
    }
}

/// Validated runtime OpenAI-compatible config consumed by [`crate::OpenAiCompatibleProvider`].
#[derive(Clone)]
pub struct ResolvedOpenAiConfig {
    pub identity: NormalizedServiceIdentity,
    pub model: String,
    pub api_key: Option<Zeroizing<String>>,
    pub allow_plaintext_lan: bool,
    pub revision: u64,
}

impl fmt::Debug for ResolvedOpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedOpenAiConfig")
            .field("identity", &self.identity.canonical_key())
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("allow_plaintext_lan", &self.allow_plaintext_lan)
            .field("revision", &self.revision)
            .finish()
    }
}

impl ResolvedOpenAiConfig {
    /// Cache variant revision excluding secrets (`{canonical_identity}|{model}`).
    pub fn variant_revision(&self) -> String {
        format!("{}|{}", self.identity.canonical_key(), self.model)
    }
}

/// Full in-memory preference snapshot.
#[derive(Clone, Debug)]
pub struct TranslationPreference {
    pub revision: u64,
    pub active_provider_id: String,
    pub display_mode: DisplayMode,
    pub target_lang: String,
    pub detect_banner_enabled: bool,
    pub auto_translate: bool,
    pub auto_translate_provider_id: Option<String>,
    pub ignored_languages: Vec<String>,
    pub openai_compatible: Option<StoredOpenAiConfig>,
}

impl Default for TranslationPreference {
    fn default() -> Self {
        Self {
            revision: 0,
            active_provider_id: String::new(),
            display_mode: DisplayMode::Bilingual,
            target_lang: default_target_lang(),
            detect_banner_enabled: true,
            auto_translate: false,
            auto_translate_provider_id: None,
            ignored_languages: Vec::new(),
            openai_compatible: None,
        }
    }
}

impl TranslationPreference {
    /// True only when auto-translate is enabled AND explicitly bound to the active provider.
    pub fn effective_auto_translate(&self) -> bool {
        self.auto_translate
            && !self.active_provider_id.is_empty()
            && self.auto_translate_provider_id.as_deref() == Some(self.active_provider_id.as_str())
    }

    pub fn to_view(&self, state_healthy: bool) -> TranslationPreferenceView {
        TranslationPreferenceView {
            revision: self.revision,
            state_healthy,
            active_provider_id: self.active_provider_id.clone(),
            display_mode: self.display_mode,
            target_lang: self.target_lang.clone(),
            detect_banner_enabled: self.detect_banner_enabled,
            auto_translate: state_healthy && self.effective_auto_translate(),
            auto_translate_provider_id: if state_healthy && self.effective_auto_translate() {
                self.auto_translate_provider_id.clone()
            } else {
                None
            },
            ignored_languages: self.ignored_languages.clone(),
            openai_compatible: self.openai_compatible.as_ref().map(StoredOpenAiConfig::to_view),
        }
    }
}

/// Safe read-only DTO for OpenAI-compatible settings. NEVER contains plaintext `api_key`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiConfigView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub base_url: String,
    pub normalized_identity: String,
    pub transport: EndpointTransport,
    pub plaintext_lan_transport: bool,
    pub model: String,
    pub has_api_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_service_identity: Option<String>,
    pub allow_plaintext_lan: bool,
}

/// Safe read-only DTO for translation preferences. NEVER contains plaintext `api_key`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationPreferenceView {
    pub revision: u64,
    pub state_healthy: bool,
    pub active_provider_id: String,
    pub display_mode: DisplayMode,
    pub target_lang: String,
    pub detect_banner_enabled: bool,
    pub auto_translate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_translate_provider_id: Option<String>,
    pub ignored_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_compatible: Option<OpenAiConfigView>,
}

/// Draft/update payload for OpenAI-compatible settings.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiConfigUpdate {
    #[serde(default)]
    pub preset: Option<String>,
    pub base_url: String,
    pub model: String,
    /// Optional new API key. When omitted/empty, the stored key may ONLY be inherited if the
    /// normalized service identity matches the stored key's bound identity.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Explicitly clear any stored API key. Mutually exclusive with a non-empty `api_key`.
    #[serde(default)]
    pub clear_api_key: bool,
    /// Explicit acknowledgment required when targeting a non-loopback `http://` LAN endpoint.
    #[serde(default)]
    pub allow_plaintext_lan: bool,
}

impl fmt::Debug for OpenAiConfigUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiConfigUpdate")
            .field("preset", &self.preset)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("clear_api_key", &self.clear_api_key)
            .field("allow_plaintext_lan", &self.allow_plaintext_lan)
            .finish()
    }
}

/// Update payload for saving `translation_preference.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationPreferenceUpdate {
    /// Optional optimistic concurrency check against the on-disk revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    pub active_provider_id: String,
    pub display_mode: DisplayMode,
    pub target_lang: String,
    pub detect_banner_enabled: bool,
    pub auto_translate: bool,
    /// Must explicitly match `active_provider_id` to enable `auto_translate`.
    #[serde(default)]
    pub auto_translate_provider_id: Option<String>,
    #[serde(default)]
    pub ignored_languages: Vec<String>,
    #[serde(default)]
    pub openai_compatible: Option<OpenAiConfigUpdate>,
}

enum StoreState {
    Ready(Box<TranslationPreference>),
    /// Corrupted, unsafe permissions/links, or unknown future version: fail-closed.
    Unavailable(String),
}

/// Fail-closed configuration store managing `translation_preference.toml` in a caller-supplied
/// active configuration directory.
pub struct TranslationConfigStore {
    config_dir: PathBuf,
    path: PathBuf,
    state: Mutex<StoreState>,
}

impl TranslationConfigStore {
    /// Open or inspect `translation_preference.toml` inside `config_dir`.
    /// Missing file -> default `Ready` state (never creates a file on read).
    /// Corrupted / unsafe / future version -> `Unavailable` (fail-closed, refuses overwrite).
    pub fn open(config_dir: impl Into<PathBuf>) -> Self {
        let config_dir = config_dir.into();
        let path = config_dir.join(TRANSLATION_PREFERENCE_FILE);
        let state = match load_preference_from_disk(&path) {
            Ok(pref) => StoreState::Ready(Box::new(pref)),
            Err(err) => {
                // Deliberately print only the sanitized reason — NEVER raw TOML lines or secrets.
                eprintln!(
                    "[maild-translate] translation preference unavailable (fail-closed): {}",
                    err.reason_code()
                );
                StoreState::Unavailable(err.to_string())
            }
        };
        Self {
            config_dir,
            path,
            state: Mutex::new(state),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True if the store loaded cleanly and accepts updates.
    pub fn is_healthy(&self) -> bool {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(*guard, StoreState::Ready(_))
    }

    /// Return a safe read DTO (never includes plaintext API key).
    /// When fail-closed, returns defaults with `state_healthy = false` and `auto_translate = false`.
    pub fn view(&self) -> TranslationPreferenceView {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*guard {
            StoreState::Ready(pref) => pref.to_view(true),
            StoreState::Unavailable(_) => TranslationPreference::default().to_view(false),
        }
    }

    /// Return the validated runtime OpenAI config (including zeroized key) for internal execution.
    pub fn resolved_openai_config(&self) -> Option<ResolvedOpenAiConfig> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*guard {
            StoreState::Ready(pref) => pref
                .openai_compatible
                .as_ref()
                .map(|cfg| cfg.to_resolved(pref.revision)),
            StoreState::Unavailable(_) => None,
        }
    }

    /// Resolve a draft OpenAI config for `probe` / "Test Connection" WITHOUT writing to disk.
    ///
    /// Enforces the exact P0 credential-to-normalized-endpoint binding rule:
    /// - Rejects simultaneous `clear_api_key: true` and non-empty `api_key`.
    /// - Changing Base URL (scheme, host, port, or API root path) MUST NOT inherit the stored key.
    pub fn resolve_draft_openai(
        &self,
        draft: &OpenAiConfigUpdate,
    ) -> Result<ResolvedOpenAiConfig, TranslateError> {
        let (stored, rev) = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*guard {
                StoreState::Ready(pref) => (pref.openai_compatible.clone(), pref.revision),
                StoreState::Unavailable(reason) => {
                    return Err(TranslateError::ConfigUnavailable(reason.clone()));
                }
            }
        };
        let resolved = resolve_openai_update(draft, stored.as_ref())?;
        Ok(resolved.to_resolved(rev))
    }

    /// Validate and atomically save updated translation preferences under a cross-process file lock.
    pub fn save(
        &self,
        update: &TranslationPreferenceUpdate,
    ) -> Result<TranslationPreferenceView, TranslateError> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let StoreState::Unavailable(ref reason) = *guard {
            return Err(TranslateError::ConfigUnavailable(reason.clone()));
        }

        // Cross-process exclusive lock serializes concurrent Studio instances.
        let _lock = acquire_process_lock(&self.config_dir, TRANSLATION_LOCK_FILE)?;

        // Re-read on-disk file under lock via no-follow handle to catch corruption, tampering,
        // or concurrent revision updates before writing.
        let current = match load_preference_from_disk(&self.path) {
            Ok(pref) => pref,
            Err(err) => {
                *guard = StoreState::Unavailable(err.to_string());
                return Err(err);
            }
        };

        if let Some(expected) = update.expected_revision {
            if expected != current.revision {
                return Err(TranslateError::RevisionConflict {
                    expected,
                    actual: current.revision,
                });
            }
        }

        let next = apply_update(&current, update)?;
        write_preference_to_disk(&self.path, &next)?;
        let view = next.to_view(true);
        *guard = StoreState::Ready(Box::new(next));
        Ok(view)
    }
}

fn apply_update(
    current: &TranslationPreference,
    update: &TranslationPreferenceUpdate,
) -> Result<TranslationPreference, TranslateError> {
    let active_provider_id = validate_provider_id(&update.active_provider_id, true)?;
    let target_lang = validate_lang_tag(&update.target_lang)?;
    let ignored_languages = validate_ignored_languages(&update.ignored_languages)?;

    let openai_compatible = match &update.openai_compatible {
        Some(openai_upd) => Some(resolve_openai_update(
            openai_upd,
            current.openai_compatible.as_ref(),
        )?),
        None => current.openai_compatible.clone(),
    };

    if active_provider_id == BUILTIN_OPENAI_PROVIDER_ID && openai_compatible.is_none() {
        return Err(TranslateError::InvalidConfig(
            "openai_compatible configuration is required when built-in provider is active".into(),
        ));
    }

    // Bind `auto_translate` explicitly to `auto_translate_provider_id`:
    // Changing the active provider disables auto_translate unless explicitly re-confirmed for
    // the new provider ID.
    let provider_changed = active_provider_id != current.active_provider_id;
    let (auto_translate, auto_translate_provider_id) = if !update.auto_translate
        || active_provider_id.is_empty()
    {
        (false, None)
    } else {
        match update.auto_translate_provider_id.as_deref().map(str::trim) {
            Some(bound) if bound == active_provider_id => (true, Some(active_provider_id.clone())),
            _ if provider_changed => (false, None),
            _ => {
                return Err(TranslateError::InvalidConfig(
                    "auto_translate must explicitly bind to active_provider_id".into(),
                ));
            }
        }
    };

    Ok(TranslationPreference {
        revision: current.revision.saturating_add(1),
        active_provider_id,
        display_mode: update.display_mode,
        target_lang,
        detect_banner_enabled: update.detect_banner_enabled,
        auto_translate,
        auto_translate_provider_id,
        ignored_languages,
        openai_compatible,
    })
}

fn resolve_openai_update(
    update: &OpenAiConfigUpdate,
    stored: Option<&StoredOpenAiConfig>,
) -> Result<StoredOpenAiConfig, TranslateError> {
    let provided_key = update
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());

    // P0: Reject simultaneous clear_api_key: true and non-empty api_key.
    if update.clear_api_key && provided_key.is_some() {
        return Err(TranslateError::InvalidConfig(
            "clear_api_key and api_key cannot both be set".into(),
        ));
    }

    let identity = NormalizedServiceIdentity::parse(&update.base_url)?;
    if identity.transport == EndpointTransport::LanHttp && !update.allow_plaintext_lan {
        return Err(TranslateError::EndpointRejected(
            "plaintext http:// on LAN requires explicit allow_plaintext_lan opt-in".into(),
        ));
    }

    let model = validate_model(&update.model)?;
    let preset = validate_optional_preset(update.preset.as_deref())?;
    let new_canonical = identity.canonical_key();

    let (api_key, api_key_bound_identity) = if update.clear_api_key {
        if !identity.transport.allows_keyless() {
            return Err(TranslateError::InvalidConfig(
                "non-loopback endpoint requires an API key".into(),
            ));
        }
        (None, None)
    } else if let Some(raw_key) = provided_key {
        if raw_key.len() > MAX_API_KEY_BYTES || raw_key.chars().any(char::is_control) {
            return Err(TranslateError::InvalidConfig(
                "invalid API key length or characters".into(),
            ));
        }
        (
            Some(Zeroizing::new(raw_key.to_string())),
            Some(new_canonical.clone()),
        )
    } else {
        // No new key supplied: ONLY inherit stored key if its bound normalized service identity
        // matches `new_canonical` EXACTLY!
        let can_inherit = stored.is_some_and(|prev| {
            prev.has_api_key()
                && prev.api_key_bound_identity.as_deref() == Some(new_canonical.as_str())
        });

        if can_inherit {
            let prev = stored.expect("checked above");
            (prev.api_key.clone(), prev.api_key_bound_identity.clone())
        } else {
            let had_different_bound_key = stored.is_some_and(|prev| {
                prev.has_api_key()
                    && prev.api_key_bound_identity.as_deref() != Some(new_canonical.as_str())
            });
            if identity.transport.allows_keyless() {
                // Explicitly keyless loopback service: NEVER inherit the old key from another URL.
                (None, None)
            } else if had_different_bound_key {
                // P0: Changing Base URL without a new key MUST fail with CredentialRebindRequired.
                return Err(TranslateError::CredentialRebindRequired);
            } else {
                return Err(TranslateError::InvalidConfig(
                    "non-loopback endpoint requires an API key".into(),
                ));
            }
        }
    };

    Ok(StoredOpenAiConfig {
        preset,
        base_url: update.base_url.trim().to_string(),
        identity,
        model,
        api_key_bound_identity,
        api_key,
        allow_plaintext_lan: update.allow_plaintext_lan,
    })
}

fn load_preference_from_disk(path: &Path) -> Result<TranslationPreference, TranslateError> {
    let Some(bytes) = read_private_file_bounded(path, MAX_CONFIG_BYTES)? else {
        return Ok(TranslationPreference::default());
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| TranslateError::ConfigUnavailable("config file is not valid UTF-8".into()))?;

    // Parse into TOML table first so unknown future format_version reports cleanly without
    // leaking raw TOML source lines or secret values.
    let table: toml::Table = toml::from_str(&text)
        .map_err(|_| TranslateError::ConfigUnavailable("corrupted TOML syntax".into()))?;

    match table
        .get("format_version")
        .and_then(toml::Value::as_integer)
    {
        Some(CONFIG_FORMAT_VERSION) => {}
        Some(other) => {
            return Err(TranslateError::ConfigUnavailable(format!(
                "unsupported format_version {other}"
            )));
        }
        None => {
            return Err(TranslateError::ConfigUnavailable(
                "missing format_version".into(),
            ));
        }
    }

    let file: PreferenceFile = table
        .try_into()
        .map_err(|_| TranslateError::ConfigUnavailable("invalid preference schema".into()))?;

    let active_provider_id = validate_provider_id(&file.active_provider_id, true)
        .map_err(|_| TranslateError::ConfigUnavailable("invalid active_provider_id".into()))?;
    let target_lang = validate_lang_tag(&file.target_lang)
        .map_err(|_| TranslateError::ConfigUnavailable("invalid target_lang".into()))?;
    let ignored_languages = validate_ignored_languages(&file.ignored_languages)
        .map_err(|_| TranslateError::ConfigUnavailable("invalid ignored_languages".into()))?;

    let openai_compatible = match file.openai_compatible {
        Some(raw) => Some(validate_stored_openai_file(raw)?),
        None => None,
    };

    // Enforce auto_translate binding even if manually edited on disk.
    let (auto_translate, auto_translate_provider_id) = if file.auto_translate
        && !active_provider_id.is_empty()
        && file.auto_translate_provider_id.as_deref() == Some(active_provider_id.as_str())
    {
        (true, Some(active_provider_id.clone()))
    } else {
        (false, None)
    };

    Ok(TranslationPreference {
        revision: file.revision,
        active_provider_id,
        display_mode: file.display_mode,
        target_lang,
        detect_banner_enabled: file.detect_banner_enabled,
        auto_translate,
        auto_translate_provider_id,
        ignored_languages,
        openai_compatible,
    })
}

fn validate_stored_openai_file(raw: StoredOpenAiFile) -> Result<StoredOpenAiConfig, TranslateError> {
    let identity = NormalizedServiceIdentity::parse(&raw.base_url)
        .map_err(|_| TranslateError::ConfigUnavailable("invalid stored base_url".into()))?;
    let model = validate_model(&raw.model)
        .map_err(|_| TranslateError::ConfigUnavailable("invalid stored model".into()))?;
    let preset = validate_optional_preset(raw.preset.as_deref())
        .map_err(|_| TranslateError::ConfigUnavailable("invalid stored preset".into()))?;

    let canonical = identity.canonical_key();
    let api_key = raw
        .api_key
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .map(Zeroizing::new);

    let api_key_bound_identity = match (&api_key, raw.api_key_bound_identity) {
        (Some(_), Some(bound)) if bound == canonical => Some(bound),
        (Some(_), _) => {
            return Err(TranslateError::ConfigUnavailable(
                "stored API key is not bound to stored normalized endpoint".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(TranslateError::ConfigUnavailable(
                "stored api_key_bound_identity present without api_key".into(),
            ));
        }
        (None, None) => None,
    };

    Ok(StoredOpenAiConfig {
        preset,
        base_url: raw.base_url.trim().to_string(),
        identity,
        model,
        api_key_bound_identity,
        api_key,
        allow_plaintext_lan: raw.allow_plaintext_lan,
    })
}

fn write_preference_to_disk(
    path: &Path,
    pref: &TranslationPreference,
) -> Result<(), TranslateError> {
    let file = PreferenceFile {
        format_version: CONFIG_FORMAT_VERSION,
        revision: pref.revision,
        active_provider_id: pref.active_provider_id.clone(),
        display_mode: pref.display_mode,
        target_lang: pref.target_lang.clone(),
        detect_banner_enabled: pref.detect_banner_enabled,
        auto_translate: pref.effective_auto_translate(),
        auto_translate_provider_id: if pref.effective_auto_translate() {
            pref.auto_translate_provider_id.clone()
        } else {
            None
        },
        ignored_languages: pref.ignored_languages.clone(),
        openai_compatible: pref.openai_compatible.as_ref().map(|cfg| StoredOpenAiFile {
            preset: cfg.preset.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            api_key_bound_identity: cfg.api_key_bound_identity.clone(),
            api_key: cfg.api_key.as_ref().map(|k| k.as_str().to_string()),
            allow_plaintext_lan: cfg.allow_plaintext_lan,
        }),
    };

    let body = Zeroizing::new(
        toml::to_string_pretty(&file)
            .map_err(|_| TranslateError::Io("failed to serialize preference file".into()))?,
    );
    if body.len() as u64 > MAX_CONFIG_BYTES {
        return Err(TranslateError::InvalidConfig(
            "serialized preference exceeds 64 KiB limit".into(),
        ));
    }

    publish_private_atomically(path, body.as_bytes())
}

fn validate_provider_id(raw: &str, allow_empty: bool) -> Result<String, TranslateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if allow_empty {
            return Ok(String::new());
        }
        return Err(TranslateError::InvalidConfig(
            "provider id cannot be empty".into(),
        ));
    }
    let bytes = trimmed.as_bytes();
    if trimmed.len() > MAX_PROVIDER_ID_BYTES
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || trimmed.contains("..")
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(TranslateError::InvalidConfig("invalid provider id".into()));
    }
    Ok(trimmed.to_string())
}

fn validate_model(raw: &str) -> Result<String, TranslateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_MODEL_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return Err(TranslateError::InvalidConfig("invalid model name".into()));
    }
    Ok(trimmed.to_string())
}

fn validate_optional_preset(raw: Option<&str>) -> Result<Option<String>, TranslateError> {
    let Some(val) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if val.len() > 64 || !val.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(TranslateError::InvalidConfig("invalid preset name".into()));
    }
    Ok(Some(val.to_string()))
}

fn validate_lang_tag(raw: &str) -> Result<String, TranslateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_LANG_TAG_BYTES
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(TranslateError::InvalidConfig("invalid language tag".into()));
    }
    Ok(trimmed.replace('_', "-"))
}

fn validate_ignored_languages(raw: &[String]) -> Result<Vec<String>, TranslateError> {
    if raw.len() > MAX_IGNORED_LANGUAGES {
        return Err(TranslateError::InvalidConfig(
            "too many ignored languages".into(),
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let tag = validate_lang_tag(entry)?;
        if !out.iter().any(|existing: &String| existing.eq_ignore_ascii_case(&tag)) {
            out.push(tag);
        }
    }
    Ok(out)
}
