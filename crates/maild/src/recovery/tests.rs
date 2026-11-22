use super::*;
use std::sync::Arc;

use maild_brand::{APP_BUNDLE_NAME, LAUNCHER_BIN, PRODUCT_NAME, UI_BIN, UI_BUNDLE_NAME};

/// `<package>/<UI_BUNDLE_NAME>/Contents/MacOS/<UI_BIN>`.
pub(crate) fn ui_exe_in(package: &Path) -> PathBuf {
    package.join(ui_exe_rel())
}

/// Package-relative UI executable path.
fn ui_exe_rel() -> String {
    format!("{UI_BUNDLE_NAME}/Contents/MacOS/{UI_BIN}")
}

/// Package-relative UI `Info.plist` path.
fn ui_plist_rel() -> String {
    format!("{UI_BUNDLE_NAME}/Contents/Info.plist")
}

pub(crate) fn source_fixture(root: &Path) -> BundledFrontend {
    let app = root.join(APP_BUNDLE_NAME);
    let exe = app.join("Contents/MacOS").join(LAUNCHER_BIN);
    let source = app
        .join("Contents/Resources/frontends")
        .join(BUNDLED_FRONTEND_ID);
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, b"launcher fixture").unwrap();
    fs::create_dir_all(source.join(UI_BUNDLE_NAME).join("Contents/MacOS")).unwrap();
    fs::write(
        source.join("frontend.toml"),
        format!(
            r#"
schema_version = 2
protocol_version = 1
id = "{BUNDLED_FRONTEND_ID}"
name = "{PRODUCT_NAME}"
version = "0.1.0"
lifecycle = "on_demand"
capabilities = ["inbox", "message", "compose", "settings", "background"]
permissions = ["mailbox_write", "plugins_manage"]
[launch]
kind = "macos_application"
path = "{UI_BUNDLE_NAME}"
"#
        ),
    )
    .unwrap();
    let binary = ui_exe_in(&source);
    // Non-runnable binary fixture: only tests copy integrity, not OS activation/signatures.
    let mut bytes = vec![0; 512];
    bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    fs::write(&binary, &bytes).unwrap();
    use sha2::Digest as _;
    fs::write(
        source.join("recovery.sha256"),
        format!("{:x}\n", sha2::Sha256::digest(&bytes)),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        source.join(ui_plist_rel()),
        format!(
            r#"<?xml version="1.0"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>{UI_BIN}</string>
<key>CFBundleIdentifier</key><string>{BUNDLED_FRONTEND_ID}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
</dict></plist>"#
        ),
    )
    .unwrap();
    BundledFrontend::from_executable(&exe).unwrap().unwrap()
}

fn installed(root: &Path) -> PathBuf {
    let path = root.join(BUNDLED_FRONTEND_ID);
    fs::create_dir(&path).unwrap();
    fs::write(path.join("original"), b"old package bytes").unwrap();
    path
}

fn state(root: &Path, bytes: &[u8]) {
    fs::write(root.join(STATE), bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(root.join(STATE), fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn preinstall_is_once_and_removal_is_respected() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        source.preinstall(root.path()).unwrap(),
        PreinstallOutcome::Installed
    );
    fs::remove_dir_all(root.path().join(BUNDLED_FRONTEND_ID)).unwrap();
    assert_eq!(
        source.preinstall(root.path()).unwrap(),
        PreinstallOutcome::AlreadyConsidered
    );
    assert!(!root.path().join(BUNDLED_FRONTEND_ID).exists());
    assert!(!root.path().join(STATE).exists()); // preinstall does not change user preference
}

#[test]
fn preinstall_preserves_existing_even_if_corrupt_and_adopts_marker() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    let old = installed(root.path());
    assert_eq!(
        source.preinstall(root.path()).unwrap(),
        PreinstallOutcome::ExistingPreserved
    );
    assert_eq!(
        fs::read(old.join("original")).unwrap(),
        b"old package bytes"
    );
    fs::remove_dir_all(old).unwrap();
    assert_eq!(
        source.preinstall(root.path()).unwrap(),
        PreinstallOutcome::AlreadyConsidered
    );
}

#[test]
fn repair_preserves_old_bytes_and_other_packages_and_can_repeat_same_version() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    installed(root.path());
    state(root.path(), b"damaged selection");
    fs::create_dir(root.path().join("another.ui")).unwrap();
    fs::write(root.path().join("another.ui/keep"), b"other").unwrap();
    let result = source.repair(root.path()).unwrap();
    let backup = root.path().join(BACKUPS).join(&result.backup);
    assert_eq!(
        fs::read(backup.join(BUNDLED_FRONTEND_ID).join("original")).unwrap(),
        b"old package bytes"
    );
    assert_eq!(fs::read(backup.join(STATE)).unwrap(), b"damaged selection");
    assert_eq!(fs::read(root.path().join(STATE)).unwrap(), default_state());
    assert_eq!(
        fs::read(root.path().join("another.ui/keep")).unwrap(),
        b"other"
    );
    let again = source.repair(root.path()).unwrap();
    assert_ne!(result.backup, again.backup);
    assert!(backup.exists());
    let broker = crate::frontend::FrontendBroker::with_install_root(
        &crate::frontend::FrontendConfig::default(),
        Some(root.path()),
    )
    .unwrap();
    assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
}

#[test]
fn failure_at_each_publication_boundary_restores_original_package_and_state() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    for point in ["backed_up", "package_published", "state_published"] {
        let root = tempfile::tempdir().unwrap();
        installed(root.path());
        state(root.path(), b"future format original");
        let error = source
            .repair_with(root.path(), |at| {
                if at == point {
                    Err("injected failure".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.contains("已回退"), "{error}");
        assert_eq!(
            fs::read(root.path().join(BUNDLED_FRONTEND_ID).join("original")).unwrap(),
            b"old package bytes"
        );
        assert_eq!(
            fs::read(root.path().join(STATE)).unwrap(),
            b"future format original"
        );
    }
}

#[test]
fn interrupted_publication_without_originals_returns_to_absence() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    source
        .repair_with(root.path(), |at| {
            if at == "state_published" {
                Err("fail".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert!(!root.path().join(BUNDLED_FRONTEND_ID).exists());
    assert!(!root.path().join(STATE).exists());
    assert!(root.path().join(marker()).exists());
}

#[test]
fn concurrent_unexpected_entry_is_never_overwritten_and_uncertain_rollback_is_reported() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    installed(root.path());
    let error = source
        .repair_with(root.path(), |at| {
            if at == "backed_up" {
                fs::create_dir(root.path().join(BUNDLED_FRONTEND_ID)).unwrap();
            }
            Ok(())
        })
        .unwrap_err();
    assert!(error.contains("回退状态不确定"));
    assert_eq!(
        fs::read_dir(root.path().join(BUNDLED_FRONTEND_ID))
            .unwrap()
            .count(),
        0
    );
    let backup = fs::read_dir(root.path().join(BACKUPS))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read(backup.join(BUNDLED_FRONTEND_ID).join("original")).unwrap(),
        b"old package bytes"
    );
}

#[test]
fn damaged_source_is_rejected_before_any_replacement_or_marker() {
    for file in ["frontend.toml".to_owned(), ui_plist_rel(), ui_exe_rel()] {
        let bundle = tempfile::tempdir().unwrap();
        let source = source_fixture(bundle.path());
        let root = tempfile::tempdir().unwrap();
        installed(root.path());
        fs::write(source.source.join(&file), b"bad source").unwrap();
        assert!(source.repair(root.path()).is_err());
        assert!(root
            .path()
            .join(BUNDLED_FRONTEND_ID)
            .join("original")
            .exists());
        assert!(!root.path().join(marker()).exists());
        assert!(!root.path().join(BACKUPS).exists());
    }
}

#[test]
fn source_identity_launch_path_and_plist_version_must_match() {
    for (file, before, after) in [
        (
            "frontend.toml".to_owned(),
            format!("id = \"{BUNDLED_FRONTEND_ID}\""),
            "id = \"another.ui\"".to_owned(),
        ),
        (
            "frontend.toml".to_owned(),
            format!("path = \"{UI_BUNDLE_NAME}\""),
            format!("path = \"../{UI_BUNDLE_NAME}\""),
        ),
        (
            ui_plist_rel(),
            "<string>0.1.0</string>".to_owned(),
            "<string>9.0.0</string>".to_owned(),
        ),
        (
            ui_plist_rel(),
            format!("<string>{UI_BIN}</string>"),
            "<string>other</string>".to_owned(),
        ),
    ] {
        let bundle = tempfile::tempdir().unwrap();
        let source = source_fixture(bundle.path());
        let file = source.source.join(file);
        fs::write(
            &file,
            fs::read_to_string(&file).unwrap().replace(&before, &after),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        assert!(source.preinstall(root.path()).is_err());
        assert!(!root.path().join(marker()).exists());
    }
}

#[test]
fn source_discovery_accepts_only_fixed_launcher_or_agent_layout() {
    let root = tempfile::tempdir().unwrap();
    let source = source_fixture(root.path());
    let agent = source
        .app
        .join("Contents/Library/LoginItems")
        .join(maild_brand::AGENT_BUNDLE_NAME)
        .join("Contents/MacOS")
        .join(maild_brand::AGENT_BIN);
    fs::create_dir_all(agent.parent().unwrap()).unwrap();
    fs::write(&agent, b"agent fixture").unwrap();
    assert!(BundledFrontend::from_executable(&agent).unwrap().is_some());
    for wrong in [
        PathBuf::from(maild_brand::AGENT_BIN),
        Path::new("Contents/MacOS").join(maild_brand::AGENT_BIN),
    ] {
        assert!(BundledFrontend::from_executable(&source.app.join(wrong))
            .unwrap()
            .is_none());
    }
    fs::remove_dir_all(&source.source).unwrap();
    assert!(BundledFrontend::from_executable(&agent).unwrap().is_none());
}

fn offline_paths(root: &Path, store: &str) -> AppPaths {
    // macOS /var is a symlink; maintenance leases require physical paths.
    let root = root.canonicalize().unwrap();
    let paths = AppPaths::from_roots(&root.join("config"), &root.join("data"), None, None, &root);
    files::ensure_directory(&paths.config_dir).unwrap();
    files::ensure_directory(&paths.data_dir).unwrap();
    fs::write(
        &paths.config_file,
        format!("[plugins.store.config]\ndb_path = {store:?}\n"),
    )
    .unwrap();
    paths
}

#[test]
fn offline_repair_holds_both_locks_without_opening_sqlite() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    let paths = offline_paths(root.path(), "custom.db");
    let database = paths.data_dir.join("custom.db");
    fs::write(&database, b"opaque original database bytes").unwrap();
    let lease = AgentLifecycleLock::acquire(&paths.data_dir, &paths.runtime_dir).unwrap();
    assert!(offline(&paths, |root| source.repair(root))
        .unwrap_err()
        .contains("生命周期锁"));
    assert!(!paths.frontend_dir.exists());
    drop(lease);
    let database_lease = DatabaseLock::acquire(&database).unwrap();
    assert!(offline(&paths, |root| source.repair(root))
        .unwrap_err()
        .contains("维护锁"));
    assert!(!paths.frontend_dir.exists());
    drop(database_lease);
    offline(&paths, |root| {
        assert!(AgentLifecycleLock::acquire(&paths.data_dir, &paths.runtime_dir).is_err());
        assert!(DatabaseLock::acquire(&database).is_err());
        source.repair(root)
    })
    .unwrap();
    assert_eq!(
        fs::read(database).unwrap(),
        b"opaque original database bytes"
    );
}

#[test]
fn offline_config_failure_does_not_guess_database_or_mutate_frontends() {
    let root = tempfile::tempdir().unwrap();
    let paths = offline_paths(root.path(), "custom.db");
    fs::write(&paths.config_file, "broken = [").unwrap();
    assert!(offline(&paths, |_| Ok(())).is_err());
    assert!(!paths.frontend_dir.exists());
}

#[test]
fn broker_can_reload_a_previously_corrupt_selection_after_explicit_repair() {
    let root = tempfile::tempdir().unwrap();
    state(root.path(), b"unknown format");
    let broker = Arc::new(
        crate::frontend::FrontendBroker::with_install_root(
            &crate::frontend::FrontendConfig::default(),
            Some(root.path()),
        )
        .unwrap(),
    );
    assert!(broker
        .set_default(crate::frontend::BUILTIN_RECOVERY_FRONTEND)
        .is_err());
    let bundle = tempfile::tempdir().unwrap();
    source_fixture(bundle.path()).repair(root.path()).unwrap();
    broker.reload_after_repair(root.path()).unwrap();
    assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
    broker
        .set_default(crate::frontend::BUILTIN_RECOVERY_FRONTEND)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_payload_and_backup_root_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"outside").unwrap();
    let root = tempfile::tempdir().unwrap();
    installed(root.path());
    symlink(outside.path(), source.source.join("extra")).unwrap();
    assert!(source.repair(root.path()).is_err());
    fs::remove_file(source.source.join("extra")).unwrap();
    symlink(outside.path(), root.path().join(BACKUPS)).unwrap();
    assert!(source.repair(root.path()).is_err());
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    assert!(root
        .path()
        .join(BUNDLED_FRONTEND_ID)
        .join("original")
        .exists());
}

#[cfg(unix)]
#[test]
fn repair_quarantines_damaged_destination_links_without_following_them() {
    use std::os::unix::fs::symlink;
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"outside").unwrap();
    let root = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join(BUNDLED_FRONTEND_ID)).unwrap();
    symlink(outside.path().join("keep"), root.path().join(STATE)).unwrap();
    let result = source.repair(root.path()).unwrap();
    assert!(
        fs::symlink_metadata(root.path().join(BACKUPS).join(result.backup).join(STATE))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn unsafe_permissions_and_symlink_ancestors_are_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(source.repair(root.path()).is_err());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let resources = source.app.join("Contents/Resources");
    let moved = bundle.path().join("moved");
    fs::rename(&resources, &moved).unwrap();
    symlink(&moved, &resources).unwrap();
    assert!(source.repair(root.path()).is_err());
    assert!(
        BundledFrontend::from_executable(&source.app.join("Contents/MacOS").join(LAUNCHER_BIN))
            .is_err()
    );
}

#[test]
fn exclusive_rename_preserves_existing_empty_directory_and_file() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    assert!(files::rename_new(&a, &b).is_err());
    assert!(a.exists() && b.exists());
    fs::write(root.path().join("c"), b"c").unwrap();
    fs::write(root.path().join("d"), b"d").unwrap();
    assert!(files::rename_new(&root.path().join("c"), &root.path().join("d")).is_err());
    assert_eq!(fs::read(root.path().join("d")).unwrap(), b"d");
}

#[test]
fn full_catalog_is_rejected_before_marker_or_default_changes() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    for n in 0..64 {
        fs::create_dir(root.path().join(format!("frontend{n}"))).unwrap();
    }
    for result in [
        source.preinstall(root.path()).map(|_| ()),
        source.repair(root.path()).map(|_| ()),
    ] {
        assert!(result.unwrap_err().contains("数量上限"));
    }
    assert!(!root.path().join(marker()).exists());
    assert!(!root.path().join(STATE).exists());
    assert!(!root.path().join(BUNDLED_FRONTEND_ID).exists());
    fs::remove_dir(root.path().join("frontend0")).unwrap();
    source.preinstall(root.path()).unwrap();
    source.repair(root.path()).unwrap(); // replacement at the limit does not add a slot
}

#[test]
fn intact_header_with_truncated_or_changed_binary_fails_checksum() {
    for truncate in [false, true] {
        let bundle = tempfile::tempdir().unwrap();
        let source = source_fixture(bundle.path());
        let exe = ui_exe_in(&source.source);
        let mut bytes = fs::read(&exe).unwrap();
        if truncate {
            bytes.truncate(32);
        } else {
            bytes[100] ^= 1;
        }
        fs::write(exe, bytes).unwrap();
        let root = tempfile::tempdir().unwrap();
        assert!(source.repair(root.path()).unwrap_err().contains("校验和"));
        assert!(!root.path().join(marker()).exists());
    }
}

#[cfg(unix)]
#[test]
fn owner_must_have_execute_permission() {
    use std::os::unix::fs::PermissionsExt as _;
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    fs::set_permissions(
        ui_exe_in(&source.source),
        fs::Permissions::from_mode(0o655),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    assert!(source.repair(root.path()).unwrap_err().contains("执行权限"));
}

#[tokio::test]
async fn alternate_config_daemons_without_store_also_exclude_offline_recovery() {
    let root = tempfile::tempdir().unwrap();
    let paths = offline_paths(root.path(), "");
    let alternate = paths.config_dir.join("alternate.toml");
    let context = cordis::Context::new();
    let mut assembly = crate::paths::assemble_with_paths_from(
        &context,
        "[plugins.frontend]\n",
        &paths,
        None,
        &alternate,
    )
    .unwrap();
    assert!(offline(&paths, |_| Ok(()))
        .unwrap_err()
        .contains("前端目录仍被"));
    // A second daemon remains usable but has no frontend-directory ownership or write path.
    let mut second = crate::paths::assemble_with_paths_from(
        &cordis::Context::new(), "[plugins.frontend]\n", &paths, None, &alternate,
    ).unwrap();
    assert_eq!(second.ctx.get::<crate::frontend::FrontendBroker>().unwrap().default_frontend(), crate::frontend::BUILTIN_RECOVERY_FRONTEND);
    assert!(second.ctx.get::<crate::frontend::FrontendDirectoryLease>().is_none());
    second.dispose().await;
    assembly.dispose().await;
    drop(assembly);
    drop(context);
    offline(&paths, |_| Ok(())).unwrap();
}

/// 09-C: the Agent's frontend plugin preinstalls under its directory lease before the first scan,
/// so a fresh install opens the bundled Tauri default; a later user removal is respected.
#[cfg(unix)]
#[test]
fn agent_startup_preinstalls_once_and_routes_default_to_bundled_frontend() {
    use cordis::{Context, Plugin as _};
    let bundle = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let start = |root: &Path| {
        let ctx = Context::new();
        ctx.provide(Arc::new(crate::frontend::FrontendConfig::default()));
        ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(root.into())));
        ctx.provide(Arc::new(source_fixture(bundle.path())));
        ctx.provide(crate::plugin_identity::IdentitySeam::open(root.join("auth.toml")));
        crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
        ctx
    };
    let ctx = start(root.path());
    let broker = ctx.get::<crate::frontend::FrontendBroker>().unwrap();
    assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
    assert!(ctx
        .get::<crate::plugin_identity::IdentitySeam>()
        .unwrap()
        .contains(crate::plugin_identity::PluginKind::Frontend, BUNDLED_FRONTEND_ID));
    assert!(!root.path().join(STATE).exists(), "preinstall never writes a preference");
    drop(broker);
    drop(ctx); // releases the directory lease

    fs::remove_dir_all(root.path().join(BUNDLED_FRONTEND_ID)).unwrap();
    let ctx = start(root.path());
    assert!(!root.path().join(BUNDLED_FRONTEND_ID).exists());
    assert_eq!(
        ctx.get::<crate::frontend::FrontendBroker>().unwrap().default_frontend(),
        crate::frontend::BUILTIN_RECOVERY_FRONTEND
    );
}

/// Expand the packaged, brand-neutral frontend manifest template (`__BRAND_*__` placeholders)
/// with `maild_brand` values, exactly as build-bundle.sh renders it from brand.env.
fn bundled_manifest_template(root: &Path, version: &str) -> String {
    fs::read_to_string(root.join("packaging/macos/templates/frontend.toml.in"))
        .expect("packaging/macos/templates/frontend.toml.in must exist")
        .replace("__BRAND_UI_BUNDLE_ID__", maild_brand::UI_BUNDLE_ID)
        .replace("__BRAND_PRODUCT_NAME__", PRODUCT_NAME)
        .replace("__BRAND_BUNDLE_SHORT_VERSION__", version)
        .replace("__BRAND_UI_BUNDLE__", UI_BUNDLE_NAME)
}

/// The packaged recovery manifest is generated from a template (build-bundle.sh). Its routes and
/// permissions must never drift from the development manifest of the same frontend id.
#[test]
fn bundled_manifest_template_matches_the_development_manifest_and_is_launchable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let template = bundled_manifest_template(&root, "1.2.3");
    let bundled: toml::Table = toml::from_str(&template).unwrap();
    let development: toml::Table = toml::from_str(
        &fs::read_to_string(root.join("crates/maild-ui/package/frontend.toml")).unwrap(),
    )
    .unwrap();
    for key in ["schema_version", "protocol_version", "id", "name", "capabilities", "permissions"] {
        assert_eq!(bundled.get(key), development.get(key), "{key} drifted");
    }
    assert_eq!(bundled["id"].as_str(), Some(BUNDLED_FRONTEND_ID));
    assert_eq!(bundled["version"].as_str(), Some("1.2.3"));
    assert_eq!(bundled["lifecycle"].as_str(), Some("on_demand"));
    assert_eq!(bundled["launch"]["kind"].as_str(), Some("macos_application"));
    assert_eq!(bundled["launch"]["path"].as_str(), Some(UI_BUNDLE_NAME));

    // The real template passes the same inspection the Agent applies before preinstall/repair.
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let payload = source.source.clone();
    fs::write(payload.join("frontend.toml"), template).unwrap();
    let plist = payload.join(ui_plist_rel());
    let text = fs::read_to_string(&plist).unwrap().replace("0.1.0", "1.2.3");
    fs::write(&plist, text).unwrap();
    let root = tempfile::tempdir().unwrap();
    assert_eq!(source.preinstall(root.path()).unwrap(), PreinstallOutcome::Installed);
}

fn set_installed_version(root: &Path, version: &str) {
    let manifest = root.join(BUNDLED_FRONTEND_ID).join("frontend.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    let line = text.lines().find(|l| l.starts_with("version = ")).unwrap().to_owned();
    fs::write(&manifest, text.replace(&line, &format!("version = \"{version}\""))).unwrap();
}

#[test]
fn version_parse_accepts_only_numeric_bundle_versions() {
    assert_eq!(parse_version("1"), Some([1, 0, 0]));
    assert_eq!(parse_version("0.10.2"), Some([0, 10, 2]));
    assert!(parse_version("0.10.2") > parse_version("0.9.9"));
    for bad in ["", "1.", ".1", "1.2.3.4", "1.0-beta", "v1.0", "1..2"] {
        assert_eq!(parse_version(bad), None, "{bad}");
    }
}

/// App upgrade: an older installed bundled UI follows the newer bundled copy; the old package is
/// backed up and the user's default selection is untouched.
#[test]
fn refresh_upgrades_only_an_older_installed_copy_and_keeps_preference() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path()); // bundled 0.1.0
    let root = tempfile::tempdir().unwrap();
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Absent);
    source.preinstall(root.path()).unwrap();
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept);

    state(root.path(), br#"{"version":1,"default":"another.ui"}"#);
    set_installed_version(root.path(), "0.0.9");
    assert_eq!(
        source.refresh(root.path()).unwrap(),
        RefreshOutcome::Upgraded { from: "0.0.9".into(), to: "0.1.0".into() }
    );
    let installed = crate::frontend::inspect_staged_frontend(&root.path().join(BUNDLED_FRONTEND_ID));
    assert_eq!(installed.unwrap().version, "0.1.0");
    assert_eq!(
        fs::read(root.path().join(STATE)).unwrap(),
        br#"{"version":1,"default":"another.ui"}"#,
        "upgrade never changes the default"
    );
    let backups: Vec<_> = fs::read_dir(root.path().join(BACKUPS)).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(backups.len(), 1);
    let saved = fs::read_to_string(backups[0].join(BUNDLED_FRONTEND_ID).join("frontend.toml")).unwrap();
    assert!(saved.contains("version = \"0.0.9\""));
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept, "idempotent");
}

#[test]
fn refresh_leaves_newer_unparseable_and_damaged_packages_alone() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    source.preinstall(root.path()).unwrap();
    for version in ["0.2.0", "0.1.0-dev"] {
        set_installed_version(root.path(), version);
        assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept, "{version}");
    }
    let manifest = root.path().join(BUNDLED_FRONTEND_ID).join("frontend.toml");
    fs::write(&manifest, b"damaged = [").unwrap();
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept);
    assert_eq!(fs::read(&manifest).unwrap(), b"damaged = [", "damage is left for explicit repair");
    assert!(!root.path().join(BACKUPS).exists());
}

/// A rebuilt app installed over the old one keeps the frontend version but changes the UI bytes;
/// refresh resyncs that stale copy (keeping a backup) instead of leaving it for native recovery.
#[cfg(unix)]
#[test]
fn refresh_resyncs_a_same_version_copy_from_a_different_build() {
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    source.preinstall(root.path()).unwrap();
    let installed = root.path().join(BUNDLED_FRONTEND_ID);
    let exe = ui_exe_in(&installed);
    let mut bytes = fs::read(&exe).unwrap();
    bytes[100] ^= 0xff;
    fs::write(&exe, &bytes).unwrap();
    assert!(crate::frontend::check_installed_bundled_frontend(&installed, &source.source).is_err());

    assert_eq!(
        source.refresh(root.path()).unwrap(),
        RefreshOutcome::Resynced {
            version: "0.1.0".into()
        }
    );
    crate::frontend::check_installed_bundled_frontend(&installed, &source.source).unwrap();
    assert!(root.path().join(BACKUPS).exists(), "the previous build is kept as a backup");
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept, "idempotent");

    // A copy that is broken rather than stale is still left for explicit repair.
    fs::remove_file(ui_exe_in(&installed)).unwrap();
    assert_eq!(source.refresh(root.path()).unwrap(), RefreshOutcome::Kept);
    assert!(!ui_exe_in(&installed).exists());
}

/// Agent startup applies the refresh under its lease, after the one-time preinstall.
#[cfg(unix)]
#[test]
fn agent_startup_upgrades_an_older_bundled_frontend() {
    use cordis::{Context, Plugin as _};
    let bundle = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    source.preinstall(root.path()).unwrap();
    set_installed_version(root.path(), "0.0.1");
    let ctx = Context::new();
    ctx.provide(Arc::new(crate::frontend::FrontendConfig::default()));
    ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(root.path().into())));
    ctx.provide(Arc::new(source));
    ctx.provide(crate::plugin_identity::IdentitySeam::open(root.path().join("auth.toml")));
    crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
    let listed = ctx.get::<crate::frontend::FrontendBroker>().unwrap().frontends(None);
    let entry = listed.frontends.iter().find(|f| f.id == BUNDLED_FRONTEND_ID).unwrap();
    assert_eq!(entry.version, "0.1.0");
    assert_eq!(listed.default, BUNDLED_FRONTEND_ID);
}

#[cfg(unix)]
fn source_fixture_versioned(root: &Path, version: &str) -> BundledFrontend {
    let source = source_fixture(root);
    let payload = &source.source;
    let manifest = fs::read_to_string(payload.join("frontend.toml"))
        .unwrap()
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    fs::write(payload.join("frontend.toml"), manifest).unwrap();
    let plist = fs::read_to_string(payload.join(ui_plist_rel()))
        .unwrap()
        .replace("0.1.0", version);
    fs::write(payload.join(ui_plist_rel()), plist).unwrap();
    source
}

/// M4.5-10: once the user removes the bundled frontend, upgrading the app to a higher version must NOT
/// reinstall it on Agent startup (`preinstall` marker + `refresh` Absent).
#[cfg(unix)]
#[test]
fn agent_upgrade_after_user_removal_never_reinstalls_bundled_frontend() {
    use cordis::{Context, Plugin as _};
    let bundle_v1 = tempfile::tempdir().unwrap();
    let bundle_v2 = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let start_with = |source: BundledFrontend, root: &Path| {
        let ctx = Context::new();
        ctx.provide(Arc::new(crate::frontend::FrontendConfig::default()));
        ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(root.into())));
        ctx.provide(Arc::new(source));
        ctx.provide(crate::plugin_identity::IdentitySeam::open(root.join("auth.toml")));
        crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
        ctx
    };
    let ctx = start_with(source_fixture_versioned(bundle_v1.path(), "0.1.0"), root.path());
    assert!(root.path().join(BUNDLED_FRONTEND_ID).exists());
    drop(ctx);

    fs::remove_dir_all(root.path().join(BUNDLED_FRONTEND_ID)).unwrap();
    let ctx = start_with(source_fixture_versioned(bundle_v2.path(), "0.2.0"), root.path());
    assert!(
        !root.path().join(BUNDLED_FRONTEND_ID).exists(),
        "app upgrade must not reinstall a user-removed bundled frontend"
    );
    assert_eq!(
        ctx.get::<crate::frontend::FrontendBroker>().unwrap().default_frontend(),
        crate::frontend::BUILTIN_RECOVERY_FRONTEND
    );
}

/// M4.5-10: cross-protocol upgrade/rollback matrix.
/// - Historical `protocol_version <= current` can be upgraded by `refresh_up_to`.
/// - Future `protocol_version > current` (app rollback) is kept untouched by `refresh`, skipped
///   by catalog scan (default routes to `builtin-repair`), and cleanly replaced by explicit `repair`.
#[cfg(unix)]
#[test]
fn cross_protocol_upgrade_and_rollback_matrix() {
    use cordis::{Context, Plugin as _};
    let bundle_v1 = tempfile::tempdir().unwrap();
    let bundle_v2 = tempfile::tempdir().unwrap();
    let source_v1 = source_fixture_versioned(bundle_v1.path(), "0.1.0");
    let source_v2 = source_fixture_versioned(bundle_v2.path(), "0.2.0");
    let root = tempfile::tempdir().unwrap();
    source_v1.preinstall(root.path()).unwrap();

    // Simulate upgrading onto a protocol-v2 daemon while installed copy is protocol-v1 (0.1.0).
    assert_eq!(
        crate::frontend::inspect_installed_for_upgrade(&root.path().join(BUNDLED_FRONTEND_ID), 2)
            .unwrap(),
        "0.1.0"
    );
    assert_eq!(
        source_v2.refresh_up_to(root.path(), 2).unwrap(),
        RefreshOutcome::Upgraded {
            from: "0.1.0".into(),
            to: "0.2.0".into()
        }
    );

    // Simulate rolling the app back to protocol-v1 (0.1.0) while installed copy has protocol_version = 2.
    let manifest_path = root.path().join(BUNDLED_FRONTEND_ID).join("frontend.toml");
    let text = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("protocol_version = 1", "protocol_version = 2");
    fs::write(&manifest_path, text).unwrap();
    assert_eq!(source_v1.refresh(root.path()).unwrap(), RefreshOutcome::Kept);

    let ctx = Context::new();
    ctx.provide(Arc::new(crate::frontend::FrontendConfig::default()));
    ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(root.path().into())));
    ctx.provide(Arc::new(source_fixture_versioned(bundle_v1.path(), "0.1.0")));
    ctx.provide(crate::plugin_identity::IdentitySeam::open(root.path().join("auth.toml")));
    crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
    let broker = ctx.get::<crate::frontend::FrontendBroker>().unwrap();
    assert_eq!(
        broker.default_frontend(),
        crate::frontend::BUILTIN_RECOVERY_FRONTEND,
        "rolled-back Agent skips future-protocol UI and falls back to native recovery"
    );

    // Explicit repair replaces the future-protocol package with the bundled 0.1.0 copy and backs it up.
    let repaired = source_v1.repair(root.path()).unwrap();
    assert_eq!(repaired.version, "0.1.0");
    broker.reload_after_repair(root.path()).unwrap();
    assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
}

/// M4.5-10: runtime permission checks apply equally to preinstalled packages (no exemption).
#[cfg(unix)]
#[test]
fn runtime_permission_checks_do_not_exempt_preinstalled_frontend() {
    use cordis::{Context, Plugin as _};
    use std::os::unix::fs::PermissionsExt as _;
    let bundle = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let source = source_fixture(bundle.path());
    source.preinstall(root.path()).unwrap();

    // Group-writable package directory is rejected on runtime scan; daemon stays alive on recovery.
    let pkg = root.path().join(BUNDLED_FRONTEND_ID);
    fs::set_permissions(&pkg, fs::Permissions::from_mode(0o775)).unwrap();
    let ctx = Context::new();
    ctx.provide(Arc::new(crate::frontend::FrontendConfig::default()));
    ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(root.path().into())));
    ctx.provide(crate::plugin_identity::IdentitySeam::open(root.path().join("auth.toml")));
    crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
    let broker = ctx.get::<crate::frontend::FrontendBroker>().unwrap();
    assert_eq!(broker.default_frontend(), crate::frontend::BUILTIN_RECOVERY_FRONTEND);
    assert!(
        !broker
            .frontends(None)
            .frontends
            .iter()
            .any(|f| f.id == BUNDLED_FRONTEND_ID)
    );
}

fn studio_source_fixture(root: &Path) -> (BundledFrontend, BundledFrontend) {
    let legacy = source_fixture(root);
    let source = legacy.source.parent().unwrap().join(STUDIO_FRONTEND_ID);
    let old_app = legacy.source.join(UI_BUNDLE_NAME);
    let app = source.join(STUDIO_PAYLOAD.app);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::copy(
        old_app.join("Contents/MacOS").join(UI_BIN),
        app.join("Contents/MacOS").join(STUDIO_PAYLOAD.binary),
    )
    .unwrap();
    fs::copy(
        legacy.source.join("recovery.sha256"),
        source.join("recovery.sha256"),
    )
    .unwrap();
    let manifest = fs::read_to_string(legacy.source.join("frontend.toml"))
        .unwrap()
        .replace(BUNDLED_FRONTEND_ID, STUDIO_FRONTEND_ID)
        .replace(UI_BUNDLE_NAME, STUDIO_PAYLOAD.app)
        .replace("0.1.0", "0.3.3");
    fs::write(source.join("frontend.toml"), manifest).unwrap();
    let plist = fs::read_to_string(old_app.join("Contents/Info.plist"))
        .unwrap()
        .replace(BUNDLED_FRONTEND_ID, STUDIO_FRONTEND_ID)
        .replace(UI_BIN, STUDIO_PAYLOAD.binary)
        .replace("0.1.0", "0.3.3");
    fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    let studio = legacy.studio().unwrap().unwrap();
    (legacy, studio)
}

#[test]
fn studio_and_recovery_are_both_preinstalled_and_studio_removal_is_respected() {
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, studio) = studio_source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        legacy.preinstall(root.path()).unwrap(),
        PreinstallOutcome::Installed
    );
    assert_eq!(
        studio.preinstall(root.path()).unwrap(),
        PreinstallOutcome::Installed
    );
    let broker = crate::frontend::FrontendBroker::with_install_root(
        &crate::frontend::FrontendConfig {
            default: STUDIO_FRONTEND_ID.into(),
        },
        Some(root.path()),
    )
    .unwrap();
    assert_eq!(broker.default_frontend(), STUDIO_FRONTEND_ID);
    assert!(broker.frontend_ids().contains(&BUNDLED_FRONTEND_ID.into()));
    fs::remove_dir_all(root.path().join(STUDIO_FRONTEND_ID)).unwrap();
    assert_eq!(
        studio.preinstall(root.path()).unwrap(),
        PreinstallOutcome::AlreadyConsidered
    );
    assert_eq!(studio.refresh(root.path()).unwrap(), RefreshOutcome::Absent);
    assert!(root.path().join(BUNDLED_FRONTEND_ID).is_dir());
}

#[test]
fn studio_same_version_resync_preserves_explicit_default_and_legacy_bytes() {
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, studio) = studio_source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    legacy.preinstall(root.path()).unwrap();
    studio.preinstall(root.path()).unwrap();
    let choice = default_state();
    state(root.path(), &choice);
    let legacy_bytes = fs::read(ui_exe_in(&root.path().join(BUNDLED_FRONTEND_ID))).unwrap();
    let installed = root.path().join(STUDIO_FRONTEND_ID);
    let exe = installed
        .join(STUDIO_PAYLOAD.app)
        .join("Contents/MacOS")
        .join(STUDIO_PAYLOAD.binary);
    // Standalone trial packages did not carry the product integrity sidecar.
    fs::remove_file(installed.join("recovery.sha256")).unwrap();
    let mut old = fs::read(&exe).unwrap();
    old.push(42);
    fs::write(&exe, old).unwrap();
    assert!(matches!(
        studio.refresh(root.path()).unwrap(),
        RefreshOutcome::Resynced { .. }
    ));
    assert_eq!(fs::read(root.path().join(STATE)).unwrap(), choice);
    assert_eq!(
        fs::read(ui_exe_in(&root.path().join(BUNDLED_FRONTEND_ID))).unwrap(),
        legacy_bytes
    );
    assert_eq!(
        fs::read(exe).unwrap(),
        fs::read(
            studio
                .source
                .join(STUDIO_PAYLOAD.app)
                .join("Contents/MacOS")
                .join(STUDIO_PAYLOAD.binary)
        )
        .unwrap()
    );
    assert!(studio.repair(root.path()).is_err());
    let manifest = installed.join("frontend.toml");
    let newer = fs::read_to_string(&manifest)
        .unwrap()
        .replace("0.3.3", "0.3.4");
    fs::write(&manifest, &newer).unwrap();
    assert_eq!(studio.refresh(root.path()).unwrap(), RefreshOutcome::Kept);
    assert_eq!(fs::read_to_string(manifest).unwrap(), newer);
}

#[cfg(unix)]
#[test]
fn agent_bootstrap_installs_studio_before_catalog_and_identity_reconciliation() {
    use cordis::{Context, Plugin as _};
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, _) = studio_source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    let ctx = Context::new();
    ctx.provide(Arc::new(crate::frontend::FrontendConfig {
        default: STUDIO_FRONTEND_ID.into(),
    }));
    ctx.provide(Arc::new(crate::frontend::FrontendInstallRoot(
        root.path().into(),
    )));
    ctx.provide(Arc::new(legacy));
    ctx.provide(crate::plugin_identity::IdentitySeam::open(
        root.path().join("auth.toml"),
    ));
    crate::frontend::FrontendPlugin.apply(&ctx).unwrap();
    let broker = ctx.get::<crate::frontend::FrontendBroker>().unwrap();
    assert_eq!(broker.default_frontend(), STUDIO_FRONTEND_ID);
    assert!(broker.frontend_ids().contains(&BUNDLED_FRONTEND_ID.into()));
    assert!(ctx
        .get::<crate::plugin_identity::IdentitySeam>()
        .unwrap()
        .contains(
            crate::plugin_identity::PluginKind::Frontend,
            STUDIO_FRONTEND_ID
        ));
    assert!(
        !root.path().join(STATE).exists(),
        "preinstall never writes a preference"
    );
}

#[test]
fn studio_integrity_and_capacity_fail_before_marker_or_legacy_changes() {
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, studio) = studio_source_fixture(bundle.path());
    let root = tempfile::tempdir().unwrap();
    legacy.preinstall(root.path()).unwrap();
    for n in 0..63 {
        fs::create_dir(root.path().join(format!("extra{n}"))).unwrap();
    }
    assert!(studio.preinstall(root.path()).is_err());
    assert!(!root
        .path()
        .join(format!(".preinstalled-{STUDIO_FRONTEND_ID}"))
        .exists());
    fs::remove_dir(root.path().join("extra0")).unwrap();
    fs::write(
        studio.source.join("recovery.sha256"),
        format!("{}\n", "0".repeat(64)),
    )
    .unwrap();
    assert!(studio.preinstall(root.path()).is_err());
    assert!(!root.path().join(STUDIO_FRONTEND_ID).exists());
    assert!(root.path().join(BUNDLED_FRONTEND_ID).is_dir());
}
