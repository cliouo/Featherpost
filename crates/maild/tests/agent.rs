use std::process::Command;

#[cfg(unix)]
use std::io::{BufRead as _, BufReader};
#[cfg(unix)]
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const TEST_TEMP_PARENT: &str = "/private/tmp";
#[cfg(all(unix, not(target_os = "macos")))]
const TEST_TEMP_PARENT: &str = "/tmp";

#[cfg(unix)]
fn temporary_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ma")
        .tempdir_in(TEST_TEMP_PARENT)
        .unwrap()
}

#[cfg(unix)]
fn platform_environment(root: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", root.join("home")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_RUNTIME_DIR", root.join("runtime")),
        ("XDG_STATE_HOME", root.join("state")),
        ("LOCALAPPDATA", root.join("local-app-data")),
        ("APPDATA", root.join("app-data")),
    ]
}

#[cfg(target_os = "macos")]
fn expected_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let managed = root
        .join("home")
        .join("Library")
        .join("Application Support")
        .join(maild_brand::SLUG);
    (
        managed.join(maild_brand::CONFIG_FILE_NAME),
        managed.join(maild_brand::DATABASE_FILE_NAME),
        managed.join("run").join(maild_brand::SOCKET_FILE_NAME),
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn expected_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join("config").join(maild_brand::SLUG).join(maild_brand::CONFIG_FILE_NAME),
        root.join("data").join(maild_brand::SLUG).join(maild_brand::DATABASE_FILE_NAME),
        root.join("runtime").join(maild_brand::SLUG).join(maild_brand::SOCKET_FILE_NAME),
    )
}

#[test]
fn agent_rejects_configuration_arguments() {
    let output = Command::new(maild_brand::bin_exe_agent!())
        .args(["--config", "ignored.toml"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
}

#[cfg(unix)]
#[test]
fn configuration_stop_is_successful_and_does_not_log_config_contents() {
    let directory = temporary_directory();
    let (config, _, _) = expected_paths(directory.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        config.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let secret = "agent-parser-secret-should-not-appear";
    std::fs::write(&config, format!("password = '{secret}")).unwrap();

    let mut probe = Command::new(maild_brand::bin_exe_agent!());
    probe
        .args(["maintenance-probe"])
        .env(maild_brand::CONFIG_ENV, &config)
        .env(maild_brand::TOKEN_ENV, secret);
    for (key, value) in platform_environment(directory.path()) {
        probe.env(key, value);
    }
    let probe_output = probe.output().unwrap();
    assert!(probe_output.status.success());
    assert!(String::from_utf8_lossy(&probe_output.stdout).contains("idle"));
    assert!(!String::from_utf8_lossy(&probe_output.stderr).contains(secret));

    let mut command = Command::new(maild_brand::bin_exe_agent!());
    for (key, value) in platform_environment(directory.path()) {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("配置停止"));
    assert!(!stderr.contains(secret));
}

#[cfg(unix)]
#[test]
fn maintenance_lease_blocks_agent_start_until_stdin_closes() {
    let directory = temporary_directory();
    let (_, _, socket) = expected_paths(directory.path());

    let mut lease = Command::new(maild_brand::bin_exe_agent!());
    lease
        .args(["maintenance-lease"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in platform_environment(directory.path()) {
        lease.env(key, value);
    }
    let mut lease = lease.spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(lease.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim_end(), "maintenance-lease: ready");

    let mut daemon = Command::new(maild_brand::bin_exe_agent!());
    for (key, value) in platform_environment(directory.path()) {
        daemon.env(key, value);
    }
    let output = daemon.output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("生命周期锁"));
    assert!(!socket.exists());

    drop(lease.stdin.take());
    assert!(lease.wait().unwrap().success());

    let mut probe = Command::new(maild_brand::bin_exe_agent!());
    probe.args(["maintenance-probe"]);
    for (key, value) in platform_environment(directory.path()) {
        probe.env(key, value);
    }
    assert!(probe.output().unwrap().status.success());
}

#[cfg(unix)]
#[test]
fn maintenance_exec_keeps_lock_after_validator_exits_and_releases_on_completion() {
    let directory = temporary_directory();
    let (_, _, socket) = expected_paths(directory.path());
    let manager = directory.path().join("maintenance-manager.sh");
    let ready = directory.path().join("maintenance-ready");
    let release = directory.path().join("maintenance-release");
    std::fs::write(
        &manager,
        format!(
            r#"#!/bin/sh
set -eu
[ "$1" = "--internal-maintenance" ]
"${validator}" maintenance-validate-fd --fd "${lock_fd}"
: >"$MAILD_TEST_MAINTENANCE_READY"
while [ ! -e "$MAILD_TEST_MAINTENANCE_RELEASE" ]; do
    /bin/sleep 0.01
done
"#,
            validator = maild_brand::MAINTENANCE_VALIDATOR_ENV,
            lock_fd = maild_brand::MAINTENANCE_LOCK_FD_ENV,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&manager, std::fs::Permissions::from_mode(0o700)).unwrap();

    let mut maintenance = Command::new(maild_brand::bin_exe_agent!());
    maintenance
        .args(["maintenance-exec", "--manager"])
        .arg(&manager)
        .env("MAILD_TEST_MAINTENANCE_READY", &ready)
        .env("MAILD_TEST_MAINTENANCE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in platform_environment(directory.path()) {
        maintenance.env(key, value);
    }
    let mut maintenance = maintenance.spawn().unwrap();
    if !wait_for(Duration::from_secs(10), || ready.exists()) {
        let _ = maintenance.kill();
        let output = maintenance.wait_with_output().unwrap();
        panic!(
            "maintenance manager did not become ready: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut daemon = Command::new(maild_brand::bin_exe_agent!());
    for (key, value) in platform_environment(directory.path()) {
        daemon.env(key, value);
    }
    let output = daemon.output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("生命周期锁"));
    assert!(!socket.exists());

    std::fs::write(&release, b"release").unwrap();
    assert!(maintenance.wait().unwrap().success());

    let mut probe = Command::new(maild_brand::bin_exe_agent!());
    probe.args(["maintenance-probe"]);
    for (key, value) in platform_environment(directory.path()) {
        probe.env(key, value);
    }
    assert!(probe.output().unwrap().status.success());
}

#[cfg(unix)]
#[test]
fn agent_ignores_cwd_and_config_environment_and_disposes_on_sigterm() {
    let directory = temporary_directory();
    let (config, database, socket) = expected_paths(directory.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        config.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(
        &config,
        format!(
            r#"
            [plugins.store.config]
            db_path = "{}"

            [plugins.api.config]
            transport = "unix"
            listen = "{}"
        "#,
            maild_brand::DATABASE_FILE_NAME,
            maild_brand::SOCKET_FILE_NAME
        ),
    )
    .unwrap();

    let cwd = directory.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join(maild_brand::CONFIG_FILE_NAME), "not valid toml = [").unwrap();
    let environment_config = directory.path().join("environment.toml");
    std::fs::write(&environment_config, "also not valid toml = [").unwrap();

    let mut command = Command::new(maild_brand::bin_exe_agent!());
    command
        .current_dir(&cwd)
        .env(maild_brand::CONFIG_ENV, &environment_config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in platform_environment(directory.path()) {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    if !wait_for(Duration::from_secs(10), || socket.exists()) {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "agent did not create {}: {}",
            socket.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut probe = Command::new(maild_brand::bin_exe_agent!());
    probe.args(["maintenance-probe"]);
    for (key, value) in platform_environment(directory.path()) {
        probe.env(key, value);
    }
    let busy = probe.output().unwrap();
    assert_eq!(busy.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));

    let client = maild_client::RpcClient::new(maild_client::Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    let status_reply = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.status());

    let result = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(result, 0);
    let status =
        wait_until(Duration::from_secs(10), || child.try_wait().unwrap()).unwrap_or_else(|| {
            let _ = child.kill();
            child.wait().unwrap()
        });
    assert!(status.success(), "SIGTERM should produce an orderly exit");
    assert!(
        status_reply.is_ok(),
        "Agent status failed: {status_reply:?}"
    );
    assert!(!socket.exists(), "owned Unix socket is removed on shutdown");
    assert!(database.exists());

    let mut probe = Command::new(maild_brand::bin_exe_agent!());
    probe.args(["maintenance-probe"]);
    for (key, value) in platform_environment(directory.path()) {
        probe.env(key, value);
    }
    assert!(probe.output().unwrap().status.success());

    let store = maild::store::Store::open(database.to_str().unwrap()).unwrap();
    drop(store);
}

#[cfg(unix)]
#[test]
fn http_agent_ignores_interactive_api_token_environment() {
    let directory = temporary_directory();
    let (config, _, _) = expected_paths(directory.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        config.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    std::fs::write(
        &config,
        format!(
            r#"
                [plugins.api.config]
                transport = "http"
                listen = "{address}"
                bearer_token = "{}"
            "#,
            "a".repeat(32)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut command = Command::new(maild_brand::bin_exe_agent!());
    command
        .env(maild_brand::TOKEN_ENV, "too-short-interactive-override")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in platform_environment(directory.path()) {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    if !wait_for(Duration::from_secs(10), || {
        TcpStream::connect(address).is_ok()
    }) {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "agent did not bind {address}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status =
        wait_until(Duration::from_secs(10), || child.try_wait().unwrap()).unwrap_or_else(|| {
            let _ = child.kill();
            child.wait().unwrap()
        });
    assert!(status.success(), "SIGTERM should produce an orderly exit");
}

#[cfg(unix)]
fn wait_until<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    wait_until(timeout, || predicate().then_some(())).is_some()
}
