use super::*;
use crate::frontend::BUILTIN_RECOVERY_FRONTEND;
use crate::frontend::{FrontendBroker, FrontendConfig, FrontendInstallRoot};
use crate::recovery::{
    tests::{source_fixture, studio_source_fixture},
    BUNDLED_FRONTEND_ID, STUDIO_FRONTEND_ID,
};

fn request(params: Value) -> JsonRpcRequest {
    serde_json::from_value(json!({"jsonrpc":"2.0", "id":1,
        "method":"repairBuiltinFrontend", "params":params}))
    .unwrap()
}

fn context(root: &Path, source: crate::recovery::BundledFrontend) -> Context {
    let ctx = Context::new();
    ctx.provide(Arc::new(FrontendInstallRoot(root.into())));
    ctx.provide(Arc::new(
        FrontendBroker::with_install_root(&FrontendConfig::default(), Some(root)).unwrap(),
    ));
    ctx.provide(Arc::new(source));
    ctx.provide(Arc::new(
        crate::frontend::FrontendDirectoryLease::acquire(root).unwrap(),
    ));
    ctx.provide(IdentitySeam::open(root.join("auth.toml")));
    ctx
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_round_trips_real_same_user_socket_and_denies_authenticated_frontend() {
    use maild_client::{Endpoint, RpcClient};
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), source_fixture(bundle.path()));
    let socket = root.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(serve_unix(listener, ctx, async {
        let _ = stopped.await;
    }));
    let client = RpcClient::new(Endpoint::Unix { path: socket }).unwrap();
    let outcome = client
        .call("repairBuiltinFrontend", json!({}))
        .await
        .unwrap();
    assert_eq!(outcome["id"], BUNDLED_FRONTEND_ID);
    let frontends = client.call("listFrontends", json!({})).await.unwrap();
    assert_eq!(frontends["default"], BUNDLED_FRONTEND_ID);
    let token = client
        .call(
            "revealPluginToken",
            json!({"kind":"frontend", "id":BUNDLED_FRONTEND_ID}),
        )
        .await
        .unwrap();
    let mut connection = client.connect_stream().await.unwrap();
    connection
        .call("authenticate", json!({"token":token["token"]}), |_| {})
        .await
        .unwrap();
    let denied = connection
        .call("repairBuiltinFrontend", json!({}), |_| {})
        .await
        .unwrap_err();
    assert_eq!(denied.rpc_code, Some(METHOD_NOT_PERMITTED));
    drop(connection);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn recovery_is_user_ipc_only_with_no_caller_selected_paths() {
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), source_fixture(bundle.path()));
    for principal in [
        Principal::Plugin("plugin.test".into()),
        Principal::Frontend(BUNDLED_FRONTEND_ID.into()),
    ] {
        let mut session = SessionAuth::new(RequestOrigin::UnixSocket);
        session.principal.set(principal);
        let response = dispatch_with_session(
            &ctx,
            RequestOrigin::UnixSocket,
            &mut session,
            &request(json!({})),
        )
        .await;
        assert_eq!(response.error.unwrap().code, METHOD_NOT_PERMITTED);
    }
    for origin in [RequestOrigin::Stdio, RequestOrigin::Http] {
        assert!(dispatch_with_origin(&ctx, origin, &request(json!({})))
            .await
            .error
            .is_some());
    }
    for params in [
        json!({"source":"/tmp/other"}),
        json!({"id":"other.ui"}),
        json!({"root":"/tmp/other"}),
        json!([]),
    ] {
        assert!(
            dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(params))
                .await
                .error
                .is_some()
        );
    }
    assert!(!root.path().join(BUNDLED_FRONTEND_ID).exists());
    assert!(!root
        .path()
        .join(format!(".preinstalled-{}", maild_brand::BUNDLED_FRONTEND_ID)).exists());
}

#[tokio::test]
async fn recovery_reloads_default_catalog_and_identity_before_success() {
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), source_fixture(bundle.path()));
    let response = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(json!({}))).await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["id"], BUNDLED_FRONTEND_ID);
    assert!(result["backup"].as_str().unwrap().starts_with("repair-"));
    let broker = ctx.get::<FrontendBroker>().unwrap();
    assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
    assert!(broker
        .api_capabilities_of(BUNDLED_FRONTEND_ID)
        .unwrap()
        .contains(&"plugins_manage"));
    assert!(ctx
        .get::<IdentitySeam>()
        .unwrap()
        .contains(PluginKind::Frontend, BUNDLED_FRONTEND_ID));
    // The existing settings path becomes writable after explicit recovery of damaged state.
    broker.set_default(BUILTIN_RECOVERY_FRONTEND).unwrap();
}

#[tokio::test]
async fn recovery_reloads_studio_default_catalog_and_identity_when_bundled() {
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, _studio) = studio_source_fixture(bundle.path());
    std::fs::write(root.path().join(".default.json"), b"damaged state").unwrap();
    let ctx = context(root.path(), legacy);
    let response = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(json!({}))).await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["id"], STUDIO_FRONTEND_ID);
    assert!(result["backup"].as_str().unwrap().starts_with("repair-"));
    let broker = ctx.get::<FrontendBroker>().unwrap();
    assert_eq!(broker.default_frontend(), STUDIO_FRONTEND_ID);
    assert!(broker
        .api_capabilities_of(STUDIO_FRONTEND_ID)
        .unwrap()
        .contains(&"plugins_manage"));
    assert!(ctx
        .get::<IdentitySeam>()
        .unwrap()
        .contains(PluginKind::Frontend, STUDIO_FRONTEND_ID));
    broker.set_default(BUILTIN_RECOVERY_FRONTEND).unwrap();
}

#[tokio::test]
async fn missing_bundled_source_is_an_error_without_creating_any_package() {
    let root = tempfile::tempdir().unwrap();
    let ctx = Context::new();
    ctx.provide(Arc::new(FrontendInstallRoot(root.path().into())));
    let response = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(json!({}))).await;
    assert!(response.error.is_some());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn frontend_writes_after_plugin_withdrawal_require_live_directory_ownership() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join(BUNDLED_FRONTEND_ID);
    std::fs::create_dir(&package).unwrap();
    std::fs::write(package.join("keep"), b"original").unwrap();
    let staging = root.path().join(".incoming/staging123");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("keep"), b"staged").unwrap();
    let ctx = Context::new();
    // A root service can remain after frontend teardown while the API/external plugins drain.
    ctx.provide(Arc::new(FrontendInstallRoot(root.path().into())));
    for (method, params) in [
        (
            "commitPluginInstall",
            json!({"kind":"frontend", "staging":"staging123"}),
        ),
        (
            "removePlugin",
            json!({"kind":"frontend", "id":BUNDLED_FRONTEND_ID}),
        ),
        (
            "rollbackPlugin",
            json!({"kind":"frontend", "id":BUNDLED_FRONTEND_ID}),
        ),
        ("setDefaultFrontend", json!({"id":BUNDLED_FRONTEND_ID})),
    ] {
        let mut request = request(params);
        request.method = method.into();
        let response = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request).await;
        assert!(
            response
                .error
                .unwrap()
                .message
                .contains("frontend directory lease"),
            "{method}"
        );
    }
    assert_eq!(std::fs::read(package.join("keep")).unwrap(), b"original");
    assert_eq!(std::fs::read(staging.join("keep")).unwrap(), b"staged");
}

#[test]
fn disconnected_request_keeps_mutation_lease_until_repair_and_refresh_finish() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let ctx = context(root.path(), source_fixture(bundle.path()));
        // Occupy the sole blocking worker so the repair is queued but has not written anything.
        let (release, blocked) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
        });
        ready.await.unwrap();
        let source = ctx.get::<crate::recovery::BundledFrontend>().unwrap();
        let source_refs = Arc::strong_count(&source);
        let copy = ctx.clone();
        let request_task = tokio::spawn(async move {
            dispatch_with_origin(&copy, RequestOrigin::UnixSocket, &request(json!({}))).await
        });
        // Observe THIS request's captured source, not a global mutex possibly owned by another
        // parallel test. After taking it, the handler has no yield before queuing its worker.
        let queued = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while Arc::strong_count(&source) == source_refs {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .is_ok();
        // Always release the worker before assertions to avoid a hung runtime on test failure.
        request_task.abort();
        let _ = request_task.await;
        let held_after_abort = PLUGIN_MUTATION.try_lock().is_err();
        let broker = ctx.get::<FrontendBroker>().unwrap();
        let identity = ctx.get::<IdentitySeam>().unwrap();
        drop(ctx); // daemon context may be gone before a blocking repair returns
        let filesystem_lease_held =
            crate::frontend::FrontendDirectoryLease::acquire(root.path()).is_err();
        release.send(()).unwrap();
        worker.await.unwrap();
        let _done = PLUGIN_MUTATION.lock().await;
        assert!(queued && held_after_abort);
        assert!(filesystem_lease_held);
        assert!(crate::frontend::FrontendDirectoryLease::acquire(root.path()).is_ok());
        assert_eq!(broker.default_frontend(), BUNDLED_FRONTEND_ID);
        assert!(identity.contains(PluginKind::Frontend, BUNDLED_FRONTEND_ID));
    });
}

#[cfg(unix)]
#[tokio::test]
async fn repair_rpc_restores_studio_and_sets_default_when_bundled() {
    use maild_client::{Endpoint, RpcClient};
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, _studio) = studio_source_fixture(bundle.path());
    std::fs::write(root.path().join(".default.json"), b"damaged state").unwrap();
    let ctx = context(root.path(), legacy);
    let socket = root.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(serve_unix(listener, ctx.clone(), async {
        let _ = stopped.await;
    }));
    let client = RpcClient::new(Endpoint::Unix { path: socket }).unwrap();
    let outcome = client
        .call("repairBuiltinFrontend", json!({}))
        .await
        .unwrap();
    assert_eq!(outcome["id"], STUDIO_FRONTEND_ID);
    assert!(outcome["backup"].as_str().unwrap().starts_with("repair-"));
    let frontends = client.call("listFrontends", json!({})).await.unwrap();
    assert_eq!(frontends["default"], STUDIO_FRONTEND_ID);
    assert!(ctx
        .get::<IdentitySeam>()
        .unwrap()
        .contains(PluginKind::Frontend, STUDIO_FRONTEND_ID));
    let token = client
        .call(
            "revealPluginToken",
            json!({"kind":"frontend", "id":STUDIO_FRONTEND_ID}),
        )
        .await
        .unwrap();
    assert!(token["token"].as_str().is_some_and(|t| !t.is_empty()));
    let switched = client
        .call(
            "setDefaultFrontend",
            json!({"id": BUILTIN_RECOVERY_FRONTEND}),
        )
        .await
        .unwrap();
    assert_eq!(switched["default"], BUILTIN_RECOVERY_FRONTEND);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn resync_rpc_covers_studio_without_touching_default_or_newer_standalone() {
    use maild_client::{Endpoint, RpcClient};
    let root = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let (legacy, studio) = studio_source_fixture(bundle.path());
    legacy.preinstall(root.path()).unwrap();
    studio.preinstall(root.path()).unwrap();
    // Persist an explicit default and tamper the installed Studio bytes (same version, stale build).
    let default_state = format!(r#"{{"version":1,"default":"{BUNDLED_FRONTEND_ID}"}}"#);
    std::fs::write(root.path().join(".default.json"), &default_state).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            root.path().join(".default.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let studio_installed = root.path().join(STUDIO_FRONTEND_ID);
    let studio_exe = studio_installed
        .join("Featherpost Studio.app/Contents/MacOS")
        .join(maild_client::STUDIO_BIN);
    let mut stale_bytes = std::fs::read(&studio_exe).unwrap();
    stale_bytes[100] ^= 0xff;
    std::fs::write(&studio_exe, &stale_bytes).unwrap();
    let studio_bundled = bundle
        .path()
        .join(maild_brand::APP_BUNDLE_NAME)
        .join("Contents/Resources/frontends")
        .join(STUDIO_FRONTEND_ID);
    assert_eq!(
        crate::frontend::inspect_installed_frontend(
            STUDIO_FRONTEND_ID,
            &studio_installed,
            &studio_bundled
        )
        .unwrap(),
        crate::frontend::InstalledFrontendMatch::Stale
    );

    let ctx = context(root.path(), legacy);
    let socket = root.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(serve_unix(listener, ctx.clone(), async {
        let _ = stopped.await;
    }));
    let client = RpcClient::new(Endpoint::Unix { path: socket }).unwrap();

    let resynced = client
        .call("resyncBuiltinFrontend", json!({}))
        .await
        .unwrap();
    assert_eq!(resynced["id"], BUNDLED_FRONTEND_ID);
    assert_eq!(
        resynced["ids"],
        json!([BUNDLED_FRONTEND_ID, STUDIO_FRONTEND_ID])
    );
    assert_eq!(resynced["replaced"], true);
    assert_eq!(resynced["replaced_ids"], json!([STUDIO_FRONTEND_ID]));
    assert_eq!(
        crate::frontend::inspect_installed_frontend(
            STUDIO_FRONTEND_ID,
            &studio_installed,
            &studio_bundled
        )
        .unwrap(),
        crate::frontend::InstalledFrontendMatch::MatchesBundled
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join(".default.json")).unwrap(),
        default_state,
        "resync must not overwrite user default selection"
    );

    // Calling again is a no-op with replaced = false.
    let second = client
        .call("resyncBuiltinFrontend", json!({}))
        .await
        .unwrap();
    assert_eq!(second["replaced"], false);
    assert_eq!(second["replaced_ids"], json!([]));

    // A newer standalone Studio version is left untouched by resync.
    let manifest = studio_installed.join("frontend.toml");
    let newer = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("0.3.3", "0.4.0");
    std::fs::write(&manifest, &newer).unwrap();
    let third = client
        .call("resyncBuiltinFrontend", json!({}))
        .await
        .unwrap();
    assert_eq!(third["replaced"], false);
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), newer);

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
