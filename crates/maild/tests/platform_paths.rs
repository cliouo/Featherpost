#[cfg(not(windows))]
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(not(windows))]
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
fn paths_command_reports_the_managed_layout() {
    #[cfg(not(windows))]
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(maild_brand::bin_exe_daemon!());
    command.arg("paths");
    #[cfg(not(windows))]
    for (key, value) in platform_environment(directory.path()) {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    #[cfg(not(windows))]
    let (config, database, socket) = expected_paths(directory.path());
    #[cfg(windows)]
    let (config, database, socket) = {
        let paths = maild_paths::AppPaths::discover().unwrap();
        (paths.config_file, paths.database_file, paths.socket_file)
    };
    assert!(stdout.contains(&format!("config={}", config.display())));
    assert!(stdout.contains(&format!("database={}", database.display())));
    assert!(stdout.contains(&format!("socket={}", socket.display())));
    let frontends = database.parent().unwrap().join("frontends");
    assert!(stdout.contains(&format!("frontends={}", frontends.display())));
    let plugins = database.parent().unwrap().join("plugins");
    assert!(stdout.contains(&format!("plugins={}", plugins.display())));
    let plugin_data = database.parent().unwrap().join("plugin-data");
    assert!(stdout.contains(&format!("plugin-data={}", plugin_data.display())));
}

#[test]
fn config_flag_and_positional_path_are_mutually_exclusive() {
    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["--config", "one.toml", "two.toml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[cfg(unix)]
#[test]
fn absolute_config_survives_deleted_cwd_and_sigterm_releases_resources() {
    let directory = tempfile::Builder::new()
        .prefix("mp")
        .tempdir_in("/tmp")
        .unwrap();
    let (config_default, database, socket) = expected_paths(directory.path());
    let config = directory.path().join("explicit.toml");
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

    let deleted_cwd = directory.path().join("deleted-cwd");
    std::fs::create_dir(&deleted_cwd).unwrap();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(r#"cd "$1" && rmdir "$1" && exec "$2" --config "$3""#)
        .arg("maild-test")
        .arg(&deleted_cwd)
        .arg(maild_brand::bin_exe_daemon!())
        .arg(&config)
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
            "daemon did not create {}: {}",
            socket.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let result = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(result, 0);
    let status =
        wait_until(Duration::from_secs(10), || child.try_wait().unwrap()).unwrap_or_else(|| {
            let _ = child.kill();
            child.wait().unwrap()
        });
    assert!(status.success(), "SIGTERM should produce an orderly exit");
    assert!(!socket.exists(), "owned Unix socket is removed on shutdown");
    assert!(database.exists());
    assert!(!config_default.exists(), "explicit config is never copied");

    let store = maild::store::Store::open(database.to_str().unwrap()).unwrap();
    drop(store);
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
