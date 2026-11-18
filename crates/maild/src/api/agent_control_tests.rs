use super::*;

fn request(params: Value) -> JsonRpcRequest {
    serde_json::from_value(
        json!({"jsonrpc":"2.0","id":1,"method":"stopManagedAgent","params":params}),
    )
    .unwrap()
}

#[tokio::test]
async fn only_user_ipc_can_request_a_managed_graceful_stop() {
    let ctx = Context::new();
    let control = Arc::new(crate::agent_control::ManagedAgentControl::default());
    ctx.provide(control.clone());
    for principal in [
        Principal::Plugin("some.plugin".into()),
        Principal::Frontend(maild_brand::BUNDLED_FRONTEND_ID.into()),
    ] {
        let mut auth = SessionAuth::new(RequestOrigin::UnixSocket);
        auth.principal.set(principal);
        let denied = dispatch_with_session(
            &ctx,
            RequestOrigin::UnixSocket,
            &mut auth,
            &request(json!({})),
        )
        .await;
        assert_eq!(denied.error.unwrap().code, METHOD_NOT_PERMITTED);
    }
    for origin in [RequestOrigin::Http, RequestOrigin::Stdio] {
        assert!(dispatch_with_origin(&ctx, origin, &request(json!({})))
            .await
            .error
            .is_some());
    }
    assert!(dispatch_with_origin(
        &ctx,
        RequestOrigin::UnixSocket,
        &request(json!({"pid":1234}))
    )
    .await
    .error
    .is_some());
    assert!(
        tokio::time::timeout(Duration::from_millis(200), control.wait())
            .await
            .is_err()
    );
    let stopped = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(json!({}))).await;
    assert_eq!(stopped.result.unwrap()["stopping"], true);
    tokio::time::timeout(Duration::from_secs(1), control.wait())
        .await
        .unwrap();
}

#[tokio::test]
async fn bare_daemons_do_not_offer_managed_stop() {
    let response = dispatch_with_origin(
        &Context::new(),
        RequestOrigin::UnixSocket,
        &request(json!({})),
    )
    .await;
    assert!(response
        .error
        .unwrap()
        .message
        .contains("platform Agent control"));
}

#[tokio::test]
async fn stop_request_pushes_agent_stopping_only_to_frontend_streams() {
    let ctx = Context::new();
    let control = Arc::new(crate::agent_control::ManagedAgentControl::default());
    ctx.provide(control.clone());

    let mut sessions = Vec::new();
    for principal in [
        Principal::Frontend(maild_brand::BUNDLED_FRONTEND_ID.into()),
        Principal::User,
        Principal::Plugin("some.plugin".into()),
    ] {
        let shared = SharedPrincipal::new();
        shared.set(principal.clone());
        let (tx, rx) = mpsc::channel(8);
        let guard = agent_stopping_listener(&ctx, tx, shared);
        sessions.push((principal, rx, guard));
    }

    let stopped = dispatch_with_origin(&ctx, RequestOrigin::UnixSocket, &request(json!({}))).await;
    assert_eq!(stopped.result.unwrap()["stopping"], true);

    for (principal, rx, _guard) in &mut sessions {
        match principal {
            Principal::Frontend(_) => {
                let line = rx.try_recv().expect("前端长连接应在停机前收到推送");
                let frame: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(frame["jsonrpc"], "2.0");
                assert_eq!(frame["method"], "agent/stopping");
                assert_eq!(frame["params"], json!({}));
                assert!(frame.get("id").is_none());
            }
            _ => assert!(rx.try_recv().is_err(), "{principal:?} 不应收到 agent/stopping"),
        }
    }
    tokio::time::timeout(Duration::from_secs(1), control.wait())
        .await
        .unwrap();
}

#[tokio::test]
async fn rejected_stop_requests_push_nothing() {
    let ctx = Context::new();
    ctx.provide(Arc::new(
        crate::agent_control::ManagedAgentControl::default(),
    ));
    let shared = SharedPrincipal::new();
    shared.set(Principal::Frontend(maild_brand::BUNDLED_FRONTEND_ID.into()));
    let (tx, mut rx) = mpsc::channel(8);
    let _guard = agent_stopping_listener(&ctx, tx, shared);
    for origin in [RequestOrigin::Http, RequestOrigin::Stdio] {
        assert!(dispatch_with_origin(&ctx, origin, &request(json!({})))
            .await
            .error
            .is_some());
    }
    assert!(rx.try_recv().is_err(), "被拒绝的停机请求不得通知前端");
}

/// 真实流会话：未认证（user）连接订阅了推送，但 `agent/stopping` 不会出现在它的流里。
#[tokio::test(flavor = "current_thread")]
async fn unauthenticated_streams_never_see_agent_stopping() {
    let ctx = Context::new();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(server);
    let task = tokio::spawn(serve(ctx.clone(), BufReader::new(read), write));
    tokio::task::yield_now().await;

    ctx.emit_agent_stopping();
    ctx.emit::<AccountState>(AccountStateInfo {
        account: "marker".into(),
        state: crate::events::AccountStateKind::Online,
        detail: String::new(),
    });
    let mut lines = BufReader::new(client).lines();
    let first = tokio::time::timeout(Duration::from_secs(1), lines.next_line())
        .await
        .expect("推送应到达")
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["method"], "account/state", "user 会话不该收到停机推送: {first}");
    drop(lines);
    let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
}
