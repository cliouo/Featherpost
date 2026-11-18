//! M3.7-03 恢复矩阵（仓库内 hermetic 部分，ADR-0015 实施切片三）。
//!
//! 覆盖：目录损坏 / 未来版本 fail-closed 且不清理凭据、凭据丢失进入
//! needs_credentials 并可重入恢复、目录发布失败回滚 staged 凭据、
//! 孤儿 revision 收敛、服务端密码撤销只进重连/错误（不回滚配置）。
//! 真实 Keychain 锁定与真实邮箱全链路是真机外部证据（见 testing 文档）。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use maild::account_catalog::{CredentialKey, CredentialRole};
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{AccountDraftInput, Endpoint, IncomingInput, RpcClient, UpdatePatchInput};
use maild_paths::AppPaths;
use tokio::sync::oneshot;

const PASSWORD_V1: &str = "pw-one";
const PASSWORD_V2: &str = "pw-two";

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

fn prepare_platform_root(paths: &AppPaths) {
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.runtime_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(
        &paths.config_file,
        r#"
            [plugins.store.config]
            db_path = "mail.sqlite3"
            [plugins.api.config]
            transport = "unix"
            listen = "mail.sock"
        "#,
    )
    .unwrap();
}

async fn spawn_daemon(
    paths: &AppPaths,
) -> (
    tokio::task::JoinHandle<Result<(), maild::DaemonError>>,
    tokio::sync::oneshot::Sender<()>,
    std::path::PathBuf,
) {
    let options =
        DaemonOptions::new(paths.config_file.clone(), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    let socket = paths.runtime_dir.join("mail.sock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(tokio::time::Instant::now() < deadline, "socket 未出现");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (task, shutdown_tx, socket)
}

/// 视图 → 输入：重输密码时保持 endpoint 不变。
fn incoming_input_from_view(
    view: &maild_client::IncomingView,
) -> maild_client::IncomingInput {
    maild_client::IncomingInput::imap(
        view.server.clone(),
        view.port,
        view.security.clone(),
        view.username.clone(),
        view.mailbox.clone(),
        view.idle,
        view.poll_secs,
    )
}

fn client_for(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap()
}

fn draft_for(server: &FakeMail, password: &str) -> (AccountDraftInput, String) {
    let draft = AccountDraftInput {
        display_name: "恢复矩阵".into(),
        address: "rcpt@test.local".into(),
        incoming: IncomingInput::imap(
            "127.0.0.1",
            server.imap_addr.port(),
            "implicit",
            "matrix-user",
            "INBOX",
            true,
            60,
        ),
        outgoing: None,
    };
    (draft, password.to_string())
}

async fn wait_online(client: &RpcClient, id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "{id} 未上线");
        let profiles = client.list_account_profiles(false).await.unwrap().profiles;
        if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
            match profile.runtime.state.as_str() {
                "online" => return,
                state => assert!(
                    matches!(state, "connecting" | "reconnecting"),
                    "异常状态 {state}: {}",
                    profile.runtime.detail
                ),
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 目录损坏：配置停止、不产生任何网络任务、已存凭据原样保留。
#[tokio::test(flavor = "current_thread")]
async fn corrupt_catalog_fails_closed_and_keeps_credentials() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);
    std::fs::write(&paths.accounts_file, "format_version = 1\n[[profiles]]\ngarbage!!!").unwrap();

    // 预置一个本 service 下的凭据 item；解析失败绝不清理。
    let store = maild::credential::platform_store_for(&paths.accounts_file);
    let key = CredentialKey::new(
        "acct_aaaaaaaaaaaaaaaaaaaaaa",
        CredentialRole::Incoming,
        3,
    );
    store.save(&key, b"survivor").unwrap();

    let error = run_daemon_until(
        DaemonOptions::new(paths.config_file.clone(), paths.clone()).unwrap(),
        std::future::pending(),
    )
    .await
    .expect_err("损坏目录必须配置停止");
    assert!(error.to_string().contains("账户目录"), "{error}");
    assert_eq!(
        store.load(&key).unwrap().expose(),
        b"survivor",
        "解析失败不得清理凭据"
    );
}

/// 未来 format_version：在任何副作用前拒绝，凭据保留。
#[tokio::test(flavor = "current_thread")]
async fn future_catalog_version_rejects_before_side_effects() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&paths.accounts_file)
            .unwrap()
            .write_all(
                r#"format_version = 999
[[profiles]]
id = "acct_aaaaaaaaaaaaaaaaaaaaaa"
revision = 1
display_name = "x"
address = "a@b.test"
enabled = false
[profiles.incoming]
kind = "imap"
server = "imap.example.test"
port = 993
security = "implicit"
username = "u@b.test"
mailbox = "INBOX"
idle = true
poll_secs = 60
"#
                .as_bytes(),
            )
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::write(&paths.accounts_file, "format_version = 999\n").unwrap();

    let store = maild::credential::platform_store_for(&paths.accounts_file);
    let key = CredentialKey::new("acct_aaaaaaaaaaaaaaaaaaaaaa", CredentialRole::Incoming, 1);
    store.save(&key, b"keep-me").unwrap();

    let error = run_daemon_until(
        DaemonOptions::new(paths.config_file.clone(), paths.clone()).unwrap(),
        std::future::pending(),
    )
    .await
    .expect_err("未来版本必须拒绝");
    assert!(error.to_string().contains("999"), "{error}");
    assert_eq!(store.load(&key).unwrap().expose(), b"keep-me");
}

/// 凭据丢失（模拟 Keychain 项被删）：profile 保留并进入 needs_credentials，
/// 不回退 TOML 明文；重新输入密码后恢复在线。
#[tokio::test(flavor = "current_thread")]
async fn lost_credentials_enter_needs_credentials_and_recover_after_reentry() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);
    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();

    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);
    let (draft, secret) = draft_for(&server, PASSWORD_V1);
    let created = client
        .create_account(&draft, &secret, None)
        .await
        .unwrap();
    let id = created.profile.id.clone();
    wait_online(&client, &id).await;
    shutdown_tx.send(()).unwrap();
    task.await.unwrap().unwrap();

    // 模拟 Keychain item 丢失。
    let store = maild::credential::platform_store_for(&paths.accounts_file);
    let key = CredentialKey::new(id.clone(), CredentialRole::Incoming, 1);
    assert!(store.delete(&key).unwrap());

    // 重启：enabled profile 无法取到凭据 → needs_credentials，而非崩溃或回退。
    let (task2, shutdown_tx2, socket2) = spawn_daemon(&paths).await;
    let client2 = client_for(&socket2);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut saw_needs_credentials = false;
    while tokio::time::Instant::now() < deadline {
        let profiles = client2.list_account_profiles(false).await.unwrap().profiles;
        if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
            if profile.runtime.state == "needs_credentials" {
                assert_eq!(profile.credentials.incoming, "missing");
                saw_needs_credentials = true;
                break;
            }
            assert!(
                !matches!(profile.runtime.state.as_str(), "error"),
                "凭据丢失不应显示为普通错误: {}",
                profile.runtime.detail
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(saw_needs_credentials, "应进入 needs_credentials");

    // 重新输入密码（携带新秘密的 updateAccount）→ 立即重挂并在线。
    let current = client2
        .list_account_profiles(false)
        .await
        .unwrap()
        .profiles
        .into_iter()
        .find(|profile| profile.id == id)
        .unwrap();
    let patch = UpdatePatchInput {
        incoming: Some(incoming_input_from_view(&current.incoming)),
        ..UpdatePatchInput::default()
    };
    let updated = client2
        .update_account(
            &id,
            current.revision,
            &patch,
            maild_client::AccountSecretUpdate {
                incoming: Some(PASSWORD_V2),
                outgoing: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.credentials.incoming, "saved");
    wait_online(&client2, &id).await;

    shutdown_tx2.send(()).unwrap();
    task2.await.unwrap().unwrap();
}

/// 目录发布失败（rename 目标被占）：staged 凭据回滚删除、内存目录回滚，
/// 清除障碍后同一请求可以成功。
#[tokio::test(flavor = "current_thread")]
async fn failed_catalog_publish_rolls_back_staged_credential() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);

    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();
    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    // 启动后再占住发布目标：rename 到非空目录必然失败。
    std::fs::create_dir(&paths.accounts_file).unwrap();
    std::fs::write(paths.accounts_file.join("blocker"), b"x").unwrap();

    let client = client_for(&socket);
    let (draft, secret) = draft_for(&server, PASSWORD_V1);

    let error = client.create_account(&draft, &secret, None).await.err().unwrap();
    assert_ne!(error.rpc_code, Some(-32012), "探测应当通过");
    // staged item 已回滚删除。
    let store = maild::credential::platform_store_for(&paths.accounts_file);
    let attrs = store.list_attrs().unwrap();
    assert!(
        attrs.iter().all(|attr| CredentialKey::parse(attr).is_none()),
        "发布失败后不得残留 staged 凭据: {attrs:?}"
    );

    // 清除障碍后重试成功——证明失败路径没有留下半提交状态。
    std::fs::remove_dir_all(&paths.accounts_file).unwrap();
    let created = client
        .create_account(&draft, &secret, None)
        .await
        .expect("清除障碍后应成功");
    wait_online(&client, &created.profile.id).await;
    assert!(paths.accounts_file.is_file());

    shutdown_tx.send(()).unwrap();
    task.await.unwrap().unwrap();
}

/// 孤儿 revision 收敛：启动成功解析目录后，未被引用的旧 revision 被删除，
/// 被引用的原样保留。
#[tokio::test(flavor = "current_thread")]
async fn startup_sweep_removes_only_unreferenced_revisions() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);
    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();

    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);
    let (draft, secret) = draft_for(&server, PASSWORD_V1);
    let created = client.create_account(&draft, &secret, None).await.unwrap();
    let id = created.profile.id.clone();
    wait_online(&client, &id).await;

    // 模拟「写入新 revision 后、删除旧 revision 前」崩溃留下的孤儿。
    let store = maild::credential::platform_store_for(&paths.accounts_file);
    let orphan = CredentialKey::new(id.clone(), CredentialRole::Incoming, 9);
    store.save(&orphan, b"stale").unwrap();
    let referenced = CredentialKey::new(id.clone(), CredentialRole::Incoming, 1);
    assert!(store.load(&referenced).is_ok());

    shutdown_tx.send(()).unwrap();
    task.await.unwrap().unwrap();

    let (task2, shutdown_tx2, _socket2) = spawn_daemon(&paths).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if store.list_attrs().map(|attrs| !attrs.contains(&orphan.attr_account())).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !store
            .list_attrs()
            .unwrap()
            .contains(&orphan.attr_account()),
        "孤儿 revision 应在启动时收敛"
    );
    assert!(
        store.load(&referenced).is_ok(),
        "被引用的 revision 必须保留"
    );

    shutdown_tx2.send(()).unwrap();
    task2.await.unwrap().unwrap();
}

/// 服务端密码撤销：认证失败只进入错误/重连，不回滚已验证配置；
/// 通过 updateAccount 重输新密码后恢复在线。
#[tokio::test(flavor = "current_thread")]
async fn revoked_password_keeps_profile_and_recovers_after_rotation() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    prepare_platform_root(&paths);
    let server =
        FakeMail::start_imap_implicit_and_smtp_starttls_with_login(PASSWORD_V1)
            .await
            .unwrap();

    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);
    let (draft, secret) = draft_for(&server, PASSWORD_V1);
    let created = client.create_account(&draft, &secret, None).await.unwrap();
    let id = created.profile.id.clone();
    let revision_at_create = created.profile.revision;
    wait_online(&client, &id).await;

    // provider 侧改密 → 停用再启用强制新建连接 → 认证失败。
    server.rotate_imap_password(PASSWORD_V2);
    let disabled = client.set_account_enabled(&id, revision_at_create, false).await.unwrap();
    let enabled = client.set_account_enabled(&id, disabled.revision, true).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut saw_auth_error = false;
    while tokio::time::Instant::now() < deadline {
        let profiles = client.list_account_profiles(false).await.unwrap().profiles;
        if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
            if profile.runtime.state == "error"
                && profile.runtime.detail.contains("登录失败")
            {
                saw_auth_error = true;
                break;
            }
            assert_ne!(
                profile.runtime.state, "needs_credentials",
                "凭据仍在保存状态，不应要求重新录入"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(saw_auth_error, "应展示认证失败 detail");
    // 配置未回滚：revision 保持 enable 后的值，凭据仍为 saved。
    let profiles = client.list_account_profiles(false).await.unwrap().profiles;
    let profile = profiles.iter().find(|p| p.id == id).unwrap();
    assert_eq!(profile.revision, enabled.revision);
    assert_eq!(profile.credentials.incoming, "saved");

    // 用户重输新密码 → 在线。
    let patch = UpdatePatchInput {
        incoming: Some(incoming_input_from_view(&profile.incoming)),
        ..UpdatePatchInput::default()
    };
    let updated = client
        .update_account(
            &id,
            profile.revision,
            &patch,
            maild_client::AccountSecretUpdate {
                incoming: Some(PASSWORD_V2),
                outgoing: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.credentials.incoming, "saved");
    wait_online(&client, &id).await;

    shutdown_tx.send(()).unwrap();
    task.await.unwrap().unwrap();
}
