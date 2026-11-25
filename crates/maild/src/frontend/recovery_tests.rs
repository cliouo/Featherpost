use super::*;

struct RecordingLauncher(Mutex<Vec<String>>);
impl FrontendLauncher for RecordingLauncher {
    fn activate(&self, frontend: &FrontendRecord, _is_default: bool) -> Result<bool, &'static str> {
        self.0.lock().unwrap().push(frontend.info.id.clone());
        if is_builtin_frontend(&frontend.info.id) {
            Ok(true)
        } else {
            Err("injected activation failure")
        }
    }
}

fn package(root: &Path) {
    let package = root.join("test.reader");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("frontend.toml"),
        r#"
schema_version = 1
protocol_version = 1
id = "test.reader"
name = "Reader"
version = "1.0.0"
lifecycle = "independent"
capabilities = ["inbox", "message"]
"#,
    )
    .unwrap();
}

#[test]
fn missing_then_installed_default_is_recovered_without_overwriting_preference() {
    let root = tempfile::tempdir().unwrap();
    let config = FrontendConfig {
        default: "test.reader".into(),
    };
    let broker = FrontendBroker::with_install_root(&config, Some(root.path())).unwrap();
    assert_eq!(broker.default_frontend(), BUILTIN_RECOVERY_FRONTEND);
    assert!(!root.path().join(".default.json").exists());
    package(root.path());
    broker.reload_from_disk(Some(root.path())).unwrap();
    assert_eq!(broker.default_frontend(), "test.reader");
    fs::remove_file(root.path().join("test.reader/frontend.toml")).unwrap();
    broker.reload_from_disk(Some(root.path())).unwrap();
    assert_eq!(broker.default_frontend(), BUILTIN_RECOVERY_FRONTEND);
    assert!(broker.api_capabilities_of("test.reader").is_none());
}

#[test]
fn failed_activation_carries_only_background_to_native_recovery() {
    let root = tempfile::tempdir().unwrap();
    package(root.path());
    let launcher = Arc::new(RecordingLauncher(Mutex::new(vec![])));
    let broker = FrontendBroker::from_catalog(
        &FrontendConfig {
            default: "test.reader".into(),
        },
        FrontendCatalog::load(Some(root.path())).unwrap(),
        launcher.clone(),
    );
    assert!(broker
        .open(OpenIntent::Message {
            account: "private-account".into(),
            folder: Some("private-folder".into()),
            uid: "secret-id".into()
        })
        .unwrap());
    assert_eq!(
        *launcher.0.lock().unwrap(),
        ["test.reader", BUILTIN_RECOVERY_FRONTEND]
    );
    assert_eq!(broker.claim("test.reader").unwrap(), None);
    assert_eq!(
        broker.claim(BUILTIN_RECOVERY_FRONTEND).unwrap(),
        Some(OpenIntent::Background)
    );
    assert!(broker.claim(LEGACY_EGUI_FRONTEND).is_err());
    assert_eq!(
        broker
            .api_capabilities_of(BUILTIN_RECOVERY_FRONTEND)
            .unwrap(),
        ["frontend_open"]
    );
}

#[test]
fn unreadable_catalog_replaces_cached_launch_targets_and_does_not_read_bad_state() {
    let root = tempfile::tempdir().unwrap();
    package(root.path());
    let broker = FrontendBroker::with_install_root(
        &FrontendConfig {
            default: "test.reader".into(),
        },
        Some(root.path()),
    )
    .unwrap();
    assert!(broker
        .reload_from_disk(Some(&root.path().join("absent")))
        .is_err());
    assert_eq!(broker.frontend_ids(), [BUILTIN_RECOVERY_FRONTEND]);
    assert!(broker.api_capabilities_of("test.reader").is_none());
    assert_eq!(broker.default_frontend(), BUILTIN_RECOVERY_FRONTEND);
}

#[test]
fn corrupt_persisted_selection_uses_recovery_and_preserves_original_bytes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(".default.json"), b"broken state").unwrap();
    let broker =
        FrontendBroker::with_install_root(&FrontendConfig::default(), Some(root.path())).unwrap();
    assert_eq!(broker.default_frontend(), BUILTIN_RECOVERY_FRONTEND);
    assert!(broker.set_default(DEFAULT_FRONTEND).is_err());
    assert_eq!(
        fs::read(root.path().join(".default.json")).unwrap(),
        b"broken state"
    );
}

#[test]
fn recovery_id_cannot_be_claimed_by_an_installed_package() {
    let root = tempfile::tempdir().unwrap();
    package(root.path());
    let manifest = root.path().join("test.reader/frontend.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("test.reader", BUILTIN_RECOVERY_FRONTEND);
    fs::write(&manifest, text).unwrap();
    assert!(inspect_staged_frontend(&root.path().join("test.reader")).is_err());
}

#[test]
fn nonauthoritative_catalog_preserves_credentials_and_repair_restores_writes() {
    let root = tempfile::tempdir().unwrap();
    package(root.path());
    let identities = tempfile::tempdir().unwrap();
    let registry = identities.path().join("auth.toml");
    let seam = IdentitySeam::open(registry.clone());
    seam.reconcile(PluginKind::Frontend, &["test.reader"])
        .unwrap();
    let original = fs::read(&registry).unwrap();
    let lease = FrontendDirectoryLease::acquire(root.path()).unwrap();
    let ctx = Context::new();
    ctx.provide(Arc::new(FrontendConfig::default()));
    ctx.provide(Arc::new(FrontendInstallRoot(root.path().into())));
    ctx.provide(seam.clone());
    FrontendPlugin.apply(&ctx).unwrap();
    assert_eq!(
        ctx.get::<FrontendBroker>().unwrap().default_frontend(),
        BUILTIN_RECOVERY_FRONTEND
    );
    assert_eq!(fs::read(&registry).unwrap(), original);
    assert!(IdentitySeam::open(registry.clone()).contains(PluginKind::Frontend, "test.reader"));
    drop(lease);
    // A safe, owned root can also have an unscannable catalog; this is not authority to delete
    // credentials or publish ordinary package mutations before successful native recovery.
    for n in 0..64 {
        fs::create_dir(root.path().join(format!("extra{n}"))).unwrap();
    }
    let ctx = Context::new();
    ctx.provide(Arc::new(FrontendConfig::default()));
    ctx.provide(Arc::new(FrontendInstallRoot(root.path().into())));
    ctx.provide(seam);
    FrontendPlugin.apply(&ctx).unwrap();
    let broker = ctx.get::<FrontendBroker>().unwrap();
    assert!(!broker.permits_package_mutation());
    assert_eq!(fs::read(&registry).unwrap(), original);
    for n in 0..64 {
        fs::remove_dir(root.path().join(format!("extra{n}"))).unwrap();
    }
    let source = tempfile::tempdir().unwrap();
    let repaired = crate::recovery::tests::source_fixture(source.path())
        .repair(root.path())
        .unwrap();
    broker.reload_after_repair(root.path(), &repaired.id).unwrap();
    assert!(broker.permits_package_mutation());
    broker.reload_from_disk(Some(root.path())).unwrap();
    assert_eq!(
        broker.default_frontend(),
        crate::recovery::BUNDLED_FRONTEND_ID
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_ancestor_or_permissions_never_publish_a_root_or_lease() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};
    for symlinked in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let paths = maild_paths::AppPaths::from_roots(
            &base.join("config"),
            &base.join("data"),
            None,
            None,
            &base,
        );
        fs::create_dir(&paths.config_dir).unwrap();
        let external = base.join("external");
        fs::create_dir(&external).unwrap();
        if symlinked {
            symlink(&external, &paths.data_dir).unwrap();
        } else {
            fs::create_dir(&paths.data_dir).unwrap();
        }
        fs::set_permissions(
            if symlinked {
                &external
            } else {
                &paths.data_dir
            },
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::create_dir(&paths.frontend_dir).unwrap();
        fs::set_permissions(
            &paths.frontend_dir,
            fs::Permissions::from_mode(if symlinked { 0o700 } else { 0o755 }),
        )
        .unwrap();
        let mut assembly = crate::paths::assemble_with_paths_from(
            &Context::new(),
            "[plugins.frontend]",
            &paths,
            None,
            &paths.config_dir.join("fixture.toml"),
        )
        .unwrap();
        assert!(assembly.ctx.get::<FrontendInstallRoot>().is_none());
        assert!(assembly.ctx.get::<FrontendDirectoryLease>().is_none());
        assert_eq!(
            assembly
                .ctx
                .get::<FrontendBroker>()
                .unwrap()
                .default_frontend(),
            BUILTIN_RECOVERY_FRONTEND
        );
        assert!(!paths.frontend_dir.join(".owner.lock").exists());
        assembly.dispose().await;
    }
}
