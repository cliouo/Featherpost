//! Supervised menu-bar companion lifecycle.
//!
//! The menu process is a thin API consumer. It never owns Store, receive, notification or
//! frontend rendering state; Agent supervises it as a separate process for the Agent's whole
//! lifetime (it is restarted whenever it exits while the Agent runs) and teardown stops it
//! before the Cordis assembly releases lower-level services.

use std::any::TypeId;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cordis::{Context, Plugin, PluginError, Service};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct MenuConfig {
    #[serde(default = "default_restart_delay_secs")]
    pub restart_delay_secs: u64,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            restart_delay_secs: default_restart_delay_secs(),
        }
    }
}

fn default_restart_delay_secs() -> u64 {
    5
}

impl Service for MenuConfig {
    fn name(&self) -> &'static str {
        "menu.config"
    }
}

pub struct MenuSupervisor {
    running: Arc<AtomicBool>,
}

impl MenuSupervisor {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Service for MenuSupervisor {
    fn name(&self) -> &'static str {
        "menu.supervisor"
    }
}

pub(crate) fn validate_config(config: &MenuConfig) -> Result<(), String> {
    if !(1..=300).contains(&config.restart_delay_secs) {
        return Err("menu restart_delay_secs 必须为 1..=300".into());
    }
    #[cfg(not(target_os = "macos"))]
    return Err("menu companion 当前只支持 macOS".into());
    #[cfg(target_os = "macos")]
    Ok(())
}

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<MenuConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let config = ctx.get::<MenuConfig>().expect("inject 就绪");
        validate_config(&config)?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "menu 插件需要在 tokio 运行时内挂载")?;
        let executable = menu_executable_from_agent(
            &std::env::current_exe().map_err(|_| "menu companion 无法定位当前 Agent executable")?,
        )
        .ok_or_else(|| {
            format!(
                "menu companion 只允许从完整 {} 内的 Agent 启动",
                maild_brand::APP_BUNDLE_NAME
            )
        })?;
        validate_menu_executable(&executable)?;

        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let running = Arc::new(AtomicBool::new(false));
        let task_running = running.clone();
        let delay = Duration::from_secs(config.restart_delay_secs);
        let task = runtime.spawn(supervise_menu(executable, delay, stop_rx, task_running));
        ctx.provide::<MenuSupervisor>(Arc::new(MenuSupervisor { running }));
        ctx.effect_async(async move {
            let _ = stop_tx.send(true);
            let mut task = task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        });
        Ok(())
    }
}

#[cfg(target_os = "macos")]
async fn supervise_menu(
    executable: PathBuf,
    restart_delay: Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
    running: Arc<AtomicBool>,
) {
    use std::process::Stdio;

    loop {
        if *stop.borrow() {
            return;
        }
        let child = tokio::process::Command::new(&executable)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(_) => {
                eprintln!("[maild/menu] companion launch failed; retrying with backoff");
                if wait_or_stop(restart_delay, &mut stop).await {
                    return;
                }
                continue;
            }
        };
        running.store(true, Ordering::Release);
        let status = tokio::select! {
            status = child.wait() => status,
            _ = stop.changed() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                running.store(false, Ordering::Release);
                return;
            }
        };
        running.store(false, Ordering::Release);
        // The menu-bar item is bound to the Agent's whole lifetime: "Quit <PRODUCT_NAME>" stops the Agent
        // (which flips `stop` and reaps the menu above), so any exit observed while the Agent is
        // still running — clean or not — is restarted after the backoff.
        match status {
            Ok(status) if status.success() => {
                eprintln!("[maild/menu] companion exited while Agent is running; restarting")
            }
            _ => eprintln!("[maild/menu] companion exited unexpectedly; retrying with backoff"),
        }
        if wait_or_stop(restart_delay, &mut stop).await {
            return;
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn supervise_menu(
    _executable: PathBuf,
    _restart_delay: Duration,
    _stop: tokio::sync::watch::Receiver<bool>,
    _running: Arc<AtomicBool>,
) {
    // `validate_config` rejects this plugin before the task can be spawned. Keeping the stub
    // makes the shared plugin implementation type-check on cross-platform CI.
}

#[cfg(target_os = "macos")]
async fn wait_or_stop(delay: Duration, stop: &mut tokio::sync::watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = stop.changed() => true,
    }
}

fn menu_executable_from_agent(agent_executable: &Path) -> Option<PathBuf> {
    let agent_bundle = agent_executable.ancestors().nth(3)?;
    let app_bundle = agent_executable.ancestors().nth(7)?;
    if agent_bundle.file_name()?.to_str()? != maild_brand::AGENT_BUNDLE_NAME
        || app_bundle.file_name()?.to_str()? != maild_brand::APP_BUNDLE_NAME
    {
        return None;
    }
    Some(
        app_bundle
            .join("Contents/Library/Helpers")
            .join(maild_brand::MENU_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(maild_brand::MENU_BIN),
    )
}

fn validate_menu_executable(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "menu companion executable 不存在于完整 bundle")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("menu companion executable 类型无效".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_path_is_derived_only_from_exact_nested_agent_layout() {
        use maild_brand::{
            AGENT_BIN, AGENT_BUNDLE_NAME, APP_BUNDLE_NAME, MENU_BIN, MENU_BUNDLE_NAME,
        };
        let app = Path::new("/Applications").join(APP_BUNDLE_NAME);
        let valid = app.join(format!(
            "Contents/Library/LoginItems/{AGENT_BUNDLE_NAME}/Contents/MacOS/{AGENT_BIN}"
        ));
        assert_eq!(
            menu_executable_from_agent(&valid),
            Some(app.join(format!(
                "Contents/Library/Helpers/{MENU_BUNDLE_NAME}/Contents/MacOS/{MENU_BIN}"
            )))
        );
        let bare = Path::new("/tmp").join(AGENT_BIN);
        assert_eq!(menu_executable_from_agent(&bare), None);
        // A renamed top-level bundle is not the product layout.
        let renamed = Path::new("/Applications/Other.app").join(format!(
            "Contents/Library/LoginItems/{AGENT_BUNDLE_NAME}/Contents/MacOS/{AGENT_BIN}"
        ));
        assert_eq!(menu_executable_from_agent(&renamed), None);
    }

    #[test]
    fn menu_restart_delay_is_bounded() {
        assert!(validate_config(&MenuConfig {
            restart_delay_secs: 0,
        })
        .is_err());
        assert!(validate_config(&MenuConfig {
            restart_delay_secs: 301,
        })
        .is_err());
    }
}
