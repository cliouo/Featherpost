use std::fs;
use std::path::Path;

use maild_translate::{
    DisplayMode, EndpointTransport, OpenAiConfigUpdate, TranslateError, TranslationConfigStore,
    TranslationPreferenceUpdate, BUILTIN_OPENAI_PROVIDER_ID, MAX_CONFIG_BYTES,
    TRANSLATION_PREFERENCE_FILE,
};

fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

fn save_openai_initial(store: &TranslationConfigStore, base_url: &str, api_key: &str) {
    let view = store
        .save(&TranslationPreferenceUpdate {
            expected_revision: Some(0),
            active_provider_id: BUILTIN_OPENAI_PROVIDER_ID.into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: true,
            auto_translate_provider_id: Some(BUILTIN_OPENAI_PROVIDER_ID.into()),
            ignored_languages: vec!["zh".into()],
            openai_compatible: Some(OpenAiConfigUpdate {
                preset: Some("openai".into()),
                base_url: base_url.into(),
                model: "gpt-4o-mini".into(),
                api_key: Some(api_key.into()),
                clear_api_key: false,
                allow_plaintext_lan: false,
            }),
        })
        .unwrap();
    assert_eq!(view.revision, 1);
    assert!(view.state_healthy);
    assert!(view.auto_translate);
    assert_eq!(
        view.auto_translate_provider_id.as_deref(),
        Some(BUILTIN_OPENAI_PROVIDER_ID)
    );
    let openai = view.openai_compatible.as_ref().unwrap();
    assert!(openai.has_api_key);
    // Read DTO JSON must never contain the secret key.
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains(api_key));
}

#[test]
fn changing_base_url_rejects_inheriting_old_api_key_on_draft_and_save() {
    let dir = private_tempdir();
    let store = TranslationConfigStore::open(dir.path());
    let secret = "sk-super-secret-tenant-a-key";
    save_openai_initial(&store, "https://gateway.example.com/tenant-a/v1", secret);

    // Debug output must redact API keys.
    let resolved = store.resolved_openai_config().unwrap();
    let dbg = format!("{resolved:?}");
    assert!(!dbg.contains(secret));
    assert!(dbg.contains("[REDACTED]"));

    // 1. Same normalized endpoint (case/port/trailing slash/chat-completions suffix) inherits key.
    let same_endpoint_draft = OpenAiConfigUpdate {
        preset: Some("openai".into()),
        base_url: "https://GATEWAY.EXAMPLE.COM:443/tenant-a/v1/chat/completions".into(),
        model: "gpt-4o".into(),
        api_key: None,
        clear_api_key: false,
        allow_plaintext_lan: false,
    };
    let inherited = store.resolve_draft_openai(&same_endpoint_draft).unwrap();
    assert_eq!(inherited.api_key.as_deref().map(String::as_str), Some(secret));

    // 2. Changing host, port, OR shared-origin gateway API root path MUST reject inheriting old key.
    for new_url in [
        "https://evil.example.net/tenant-a/v1",
        "https://gateway.example.com:8443/tenant-a/v1",
        "https://gateway.example.com/tenant-b/v1",
    ] {
        let draft = OpenAiConfigUpdate {
            preset: Some("custom".into()),
            base_url: new_url.into(),
            model: "gpt-4o-mini".into(),
            api_key: None,
            clear_api_key: false,
            allow_plaintext_lan: false,
        };
        assert_eq!(
            store.resolve_draft_openai(&draft).unwrap_err(),
            TranslateError::CredentialRebindRequired,
            "draft test must reject inheriting old key for {new_url}"
        );
        let save_err = store
            .save(&TranslationPreferenceUpdate {
                expected_revision: Some(1),
                active_provider_id: BUILTIN_OPENAI_PROVIDER_ID.into(),
                display_mode: DisplayMode::Bilingual,
                target_lang: "zh-CN".into(),
                detect_banner_enabled: true,
                auto_translate: true,
                auto_translate_provider_id: Some(BUILTIN_OPENAI_PROVIDER_ID.into()),
                ignored_languages: vec![],
                openai_compatible: Some(draft),
            })
            .unwrap_err();
        assert_eq!(
            save_err,
            TranslateError::CredentialRebindRequired,
            "save must reject inheriting old key for {new_url}"
        );
    }

    // 3. Switching to keyless loopback Ollama succeeds WITHOUT inheriting old API key.
    let ollama_draft = OpenAiConfigUpdate {
        preset: Some("ollama".into()),
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "qwen2.5:7b".into(),
        api_key: None,
        clear_api_key: false,
        allow_plaintext_lan: false,
    };
    let resolved_ollama = store.resolve_draft_openai(&ollama_draft).unwrap();
    assert!(resolved_ollama.api_key.is_none());
    assert_eq!(resolved_ollama.identity.transport, EndpointTransport::LoopbackHttp);

    let saved_ollama = store
        .save(&TranslationPreferenceUpdate {
            expected_revision: Some(1),
            active_provider_id: BUILTIN_OPENAI_PROVIDER_ID.into(),
            display_mode: DisplayMode::TranslatedOnly,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: false,
            auto_translate_provider_id: None,
            ignored_languages: vec![],
            openai_compatible: Some(ollama_draft),
        })
        .unwrap();
    assert!(!saved_ollama.openai_compatible.unwrap().has_api_key);
    assert!(store.resolved_openai_config().unwrap().api_key.is_none());
}

#[test]
fn simultaneous_clear_and_set_api_key_is_rejected_on_draft_and_save() {
    let dir = private_tempdir();
    let store = TranslationConfigStore::open(dir.path());
    let conflict = OpenAiConfigUpdate {
        preset: Some("openai".into()),
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4o-mini".into(),
        api_key: Some("sk-new-key".into()),
        clear_api_key: true,
        allow_plaintext_lan: false,
    };
    assert!(matches!(
        store.resolve_draft_openai(&conflict),
        Err(TranslateError::InvalidConfig(_))
    ));
    assert!(matches!(
        store.save(&TranslationPreferenceUpdate {
            expected_revision: None,
            active_provider_id: BUILTIN_OPENAI_PROVIDER_ID.into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: false,
            auto_translate_provider_id: None,
            ignored_languages: vec![],
            openai_compatible: Some(conflict),
        }),
        Err(TranslateError::InvalidConfig(_))
    ));
}

#[test]
fn changing_active_provider_disables_auto_translate_until_reconfirmed() {
    let dir = private_tempdir();
    let store = TranslationConfigStore::open(dir.path());
    save_openai_initial(&store, "https://api.openai.com/v1", "sk-initial");

    // Switching active provider while passing stale auto_translate_provider_id disables auto_translate.
    let switched = store
        .save(&TranslationPreferenceUpdate {
            expected_revision: Some(1),
            active_provider_id: "com.example.deepl".into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: true,
            auto_translate_provider_id: Some(BUILTIN_OPENAI_PROVIDER_ID.into()),
            ignored_languages: vec![],
            openai_compatible: None,
        })
        .unwrap();
    assert_eq!(switched.active_provider_id, "com.example.deepl");
    assert!(!switched.auto_translate);
    assert!(switched.auto_translate_provider_id.is_none());

    // Explicitly re-confirming with the new provider ID re-enables auto_translate.
    let confirmed = store
        .save(&TranslationPreferenceUpdate {
            expected_revision: Some(2),
            active_provider_id: "com.example.deepl".into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: true,
            auto_translate_provider_id: Some("com.example.deepl".into()),
            ignored_languages: vec![],
            openai_compatible: None,
        })
        .unwrap();
    assert!(confirmed.auto_translate);
    assert_eq!(
        confirmed.auto_translate_provider_id.as_deref(),
        Some("com.example.deepl")
    );
}

#[test]
fn revision_conflict_and_path_isolation() {
    let dir_a = private_tempdir();
    let dir_b = private_tempdir();
    let store_a = TranslationConfigStore::open(dir_a.path());
    let store_b = TranslationConfigStore::open(dir_b.path());

    save_openai_initial(&store_a, "https://api.openai.com/v1", "sk-profile-a");
    assert_eq!(store_b.view().revision, 0);
    assert!(store_b.view().openai_compatible.is_none());

    // Stale expected_revision is rejected.
    let err = store_a
        .save(&TranslationPreferenceUpdate {
            expected_revision: Some(0),
            active_provider_id: "".into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "en".into(),
            detect_banner_enabled: true,
            auto_translate: false,
            auto_translate_provider_id: None,
            ignored_languages: vec![],
            openai_compatible: None,
        })
        .unwrap_err();
    assert_eq!(
        err,
        TranslateError::RevisionConflict {
            expected: 0,
            actual: 1
        }
    );

    // Reopening from disk preserves persisted state & private permissions.
    let reopened = TranslationConfigStore::open(dir_a.path());
    assert!(reopened.is_healthy());
    assert_eq!(reopened.view().revision, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = fs::symlink_metadata(dir_a.path().join(TRANSLATION_PREFERENCE_FILE)).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
    }
}

fn write_private_file(path: &Path, body: &[u8]) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn corrupted_future_version_unknown_field_and_oversize_fail_closed_without_overwrite() {
    // 1. Corrupted TOML containing a secret must fail closed and never leak source lines in error.
    let dir1 = private_tempdir();
    let p1 = dir1.path().join(TRANSLATION_PREFERENCE_FILE);
    let raw_corrupt = "format_version = 1\napi_key = \"sk-never-leak-in-error\"\n[[[broken";
    write_private_file(&p1, raw_corrupt.as_bytes());
    let store1 = TranslationConfigStore::open(dir1.path());
    assert!(!store1.is_healthy());
    assert!(!store1.view().auto_translate);
    assert!(!store1.view().state_healthy);
    let err = store1
        .save(&TranslationPreferenceUpdate {
            expected_revision: None,
            active_provider_id: "".into(),
            display_mode: DisplayMode::Bilingual,
            target_lang: "zh-CN".into(),
            detect_banner_enabled: true,
            auto_translate: false,
            auto_translate_provider_id: None,
            ignored_languages: vec![],
            openai_compatible: None,
        })
        .unwrap_err();
    assert!(matches!(err, TranslateError::ConfigUnavailable(_)));
    assert!(!err.to_string().contains("sk-never-leak-in-error"));
    assert_eq!(fs::read_to_string(&p1).unwrap(), raw_corrupt);

    // 2. Unknown future format_version fails closed without overwriting.
    let dir2 = private_tempdir();
    let p2 = dir2.path().join(TRANSLATION_PREFERENCE_FILE);
    let future = "format_version = 99\nfuture_feature = true\n";
    write_private_file(&p2, future.as_bytes());
    let store2 = TranslationConfigStore::open(dir2.path());
    assert!(!store2.is_healthy());
    assert_eq!(fs::read_to_string(&p2).unwrap(), future);

    // 3. Unknown field fails closed (deny_unknown_fields).
    let dir3 = private_tempdir();
    let p3 = dir3.path().join(TRANSLATION_PREFERENCE_FILE);
    write_private_file(&p3, b"format_version = 1\nsurprise_field = 123\n");
    let store3 = TranslationConfigStore::open(dir3.path());
    assert!(!store3.is_healthy());

    // 4. Oversize (64 KiB + 1 byte) fails closed without overwriting.
    let dir4 = private_tempdir();
    let p4 = dir4.path().join(TRANSLATION_PREFERENCE_FILE);
    let oversize = vec![b'a'; (MAX_CONFIG_BYTES + 1) as usize];
    write_private_file(&p4, &oversize);
    let store4 = TranslationConfigStore::open(dir4.path());
    assert!(!store4.is_healthy());
    assert_eq!(fs::read(&p4).unwrap().len(), (MAX_CONFIG_BYTES + 1) as usize);
}

#[cfg(unix)]
#[test]
fn unix_no_follow_symlink_hardlink_and_bad_mode_fail_closed() {
    use std::os::unix::fs::PermissionsExt as _;

    // 1. Symlink at config path is refused without following.
    let dir_sym = private_tempdir();
    let outside = dir_sym.path().join("outside.toml");
    write_private_file(&outside, b"format_version = 1\n");
    std::os::unix::fs::symlink(&outside, dir_sym.path().join(TRANSLATION_PREFERENCE_FILE)).unwrap();
    let store_sym = TranslationConfigStore::open(dir_sym.path());
    assert!(!store_sym.is_healthy());

    // 2. Hardlink (nlink == 2) fails closed.
    let dir_hl = private_tempdir();
    let cfg_path = dir_hl.path().join(TRANSLATION_PREFERENCE_FILE);
    write_private_file(&cfg_path, b"format_version = 1\n");
    fs::hard_link(&cfg_path, dir_hl.path().join("alias.toml")).unwrap();
    let store_hl = TranslationConfigStore::open(dir_hl.path());
    assert!(!store_hl.is_healthy());

    // 3. Permissive mode (0644) fails closed.
    let dir_mode = private_tempdir();
    let cfg_mode = dir_mode.path().join(TRANSLATION_PREFERENCE_FILE);
    fs::write(&cfg_mode, b"format_version = 1\n").unwrap();
    fs::set_permissions(&cfg_mode, fs::Permissions::from_mode(0o644)).unwrap();
    let store_mode = TranslationConfigStore::open(dir_mode.path());
    assert!(!store_mode.is_healthy());
}
