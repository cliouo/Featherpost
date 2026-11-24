use maild_client::{
    discover_endpoint, DiscoveryOptions, Endpoint, NativeAppLayout, RpcClient,
    BUILTIN_RECOVERY_FRONTEND,
};
use maild_l10n::Launcher;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::l10n::text;

/// JSON-RPC "method not found": the running Agent predates the method.
const METHOD_NOT_FOUND: i64 = -32601;
/// `repairBuiltinFrontend` could not fully confirm the repair.
const REPAIR_INCOMPLETE: i64 = -32017;

/// Why a daemon call failed. Callers branch on this, never on the (localized) dialog text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallError {
    /// No usable local connection (custom connection, HTTP endpoint, discovery failure).
    NoConnection,
    /// No answer within the deadline; the request may still be running.
    Unconfirmed,
    /// The call failed with this JSON-RPC error code (`None`: transport or decoding failure).
    Rpc(Option<i64>),
}

impl CallError {
    /// The dialog text for this failure.
    fn message(self, t: &'static Launcher) -> &'static str {
        match self {
            CallError::NoConnection => t.error_local_connection_unavailable,
            CallError::Unconfirmed => t.error_operation_unconfirmed,
            CallError::Rpc(Some(METHOD_NOT_FOUND)) => t.error_unsupported_by_agent,
            CallError::Rpc(Some(REPAIR_INCOMPLETE)) => t.error_repair_incomplete,
            CallError::Rpc(_) => t.error_agent_unconfirmed,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Status {
    pub online: bool,
    pub mailbox_available: bool,
    pub can_manage: bool,
    pub login: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    OpenMailbox,
    Repair,
    StartOnce,
    Stop,
    RegisterLogin,
    UnregisterLogin,
    SystemSettings,
}

/// A small seam keeps native interaction tests/preview entirely separate from real user data.
pub trait Controller {
    fn status(&mut self) -> Status;
    fn perform(&mut self, operation: Operation) -> Result<&'static str, &'static str>;
    fn mailbox_host(&mut self) -> Result<Option<PathBuf>, &'static str> {
        Ok(None)
    }
}

pub struct NativeController {
    runtime: tokio::runtime::Runtime,
    client: Option<RpcClient>,
    layout: Option<NativeAppLayout>,
    overrides: bool,
}

impl NativeController {
    pub fn discover() -> Result<Self, &'static str> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|_| text().error_connection_setup)?;
        let overrides = NativeAppLayout::has_custom_connection();
        let client = if overrides {
            None
        } else {
            discover_endpoint(&DiscoveryOptions::default())
                .ok()
                .filter(|endpoint| !matches!(endpoint, Endpoint::Http { .. }))
                .and_then(|endpoint| RpcClient::new(endpoint).ok())
        };
        let layout = NativeAppLayout::discover().map_err(|_| text().error_app_incomplete)?;
        Ok(Self {
            runtime,
            client,
            layout,
            overrides,
        })
    }

    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, &'static str> {
        self.try_call(method, params, timeout)
            .map_err(|error| error.message(text()))
    }

    fn try_call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, CallError> {
        let client = self.client.as_ref().ok_or(CallError::NoConnection)?;
        self.runtime.block_on(async {
            tokio::time::timeout(timeout + Duration::from_secs(1), async {
                let mut connection = client.connect_stream().await?;
                connection.call_with_timeout(method, params, timeout, |_| {}).await
            }).await
                .map_err(|_| CallError::Unconfirmed)?
                .map_err(|error| CallError::Rpc(error.rpc_code))
        })
    }

    /// Re-negotiate the dialog language: the daemon's `effective` list when it answers,
    /// otherwise the OS preferred languages (ADR-0048 decision 6, open item 2).
    fn refresh_locale(&self, online: bool) {
        let effective = if online { self.daemon_locale() } else { None };
        crate::l10n::apply_daemon_preference(effective.as_deref());
    }

    fn daemon_locale(&self) -> Option<Vec<String>> {
        let client = self.client.as_ref()?;
        // Build the timeout inside the runtime: `tokio::time::timeout` needs a reactor at creation.
        let answer = self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), client.locale_preference()).await
        });
        Some(answer.ok()?.ok()?.effective)
    }

    fn managed(&self) -> Result<&NativeAppLayout, &'static str> {
        if self.overrides {
            return Err(text().error_custom_connection);
        }
        self.layout
            .as_ref()
            .ok_or(text().error_open_from_full_app)
    }

    fn online(&self) -> bool {
        self.call("getAgentInfo", json!({}), Duration::from_secs(2))
            .is_ok()
    }

    fn wait_online(&self, budget: Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if self.online() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Normal app-bundle open: ensure a background exists, then route the mailbox (ADR-0044):
    /// the healthy bundled default is hosted in place by this very process (`InPlace`), a
    /// third-party default is activated by the Agent (`Requested`, this process may exit; a later
    /// asynchronous activation failure is routed back to native recovery by the Agent), and
    /// everything else falls through to the recovery window with its honest status.
    pub fn open_on_launch(&mut self, before_request: impl FnOnce()) -> LaunchOutcome {
        if self.client.is_none() {
            return LaunchOutcome::Recovery;
        }
        if !self.online() {
            if self.layout.is_none() || self.overrides {
                return LaunchOutcome::Recovery;
            }
            // Signed builds use the user-registered login item; only the unsigned Preview starts
            // its embedded Agent on open (ADR-0039). Neither registers anything implicitly.
            if super::platform::uses_preview_agent() {
                if self.perform(Operation::StartOnce).is_err() {
                    return LaunchOutcome::Recovery;
                }
            } else if !super::platform::login_enabled() {
                return LaunchOutcome::Recovery;
            }
            if !self.wait_online(Duration::from_secs(10)) {
                return LaunchOutcome::Recovery;
            }
        }
        let Ok(catalog) = self.call("listFrontends", json!({}), Duration::from_secs(2)) else {
            return LaunchOutcome::Recovery;
        };
        let mut in_place = None;
        match launch_route(&catalog, || {
            let (check, exe) = self.in_place_ui();
            in_place = exe;
            check
        }) {
            LaunchRoute::InPlace => {
                // ADR-0045：窗口已在运行（`.main-ui.lock` 有主），它可能正原生最小化在 Dock，
                // 而 Reopen 不一定送达。入队一条收件箱意图——Agent 随即经认证
                // 长连接推 `frontend/intent`，窗口自己回到前台——再按 PID 聚焦，本进程退出。
                if let Some(pid) = super::platform::main_window_pid() {
                    let _ = self.request_mailbox(&MailboxTarget::bundled());
                    if let Some(pid) = pid {
                        super::platform::focus(pid);
                    }
                    return LaunchOutcome::Focused;
                }
                in_place.map_or(LaunchOutcome::Recovery, LaunchOutcome::InPlace)
            }
            LaunchRoute::Frontend(target) => {
                before_request();
                // No redundant inbox intent for the in-place path: it would supersede a queued
                // notification/menu intent, and the bundled UI shows the inbox when nothing is queued.
                if self
                    .request_mailbox(&target)
                    .is_ok_and(|response| response["activation_requested"] == true)
                {
                    LaunchOutcome::Requested
                } else {
                    LaunchOutcome::Recovery
                }
            }
            LaunchRoute::Recovery => LaunchOutcome::Recovery,
        }
    }

    /// Whether this complete app bundle can host the bundled frontend itself. The catalog entry
    /// already proves the managed bundled frontend exists, is a valid package and is protocol
    /// compatible; the installed executable must also be intact against the same-version bundled
    /// `recovery.sha256`, so damage keeps routing to native recovery (ADR-0039/0040).
    fn in_place_ui(&self) -> (InPlace, Option<PathBuf>) {
        let Some(layout) = self.layout.as_ref().filter(|_| !self.overrides) else {
            return (InPlace::Unavailable, None);
        };
        let Ok(exe) = layout.ui_executable() else {
            return (InPlace::Unavailable, None);
        };
        let Ok(paths) = maild_paths::AppPaths::discover() else {
            return (InPlace::Unavailable, None);
        };
        let installed = paths
            .frontend_dir
            .join(maild::recovery::BUNDLED_FRONTEND_ID);
        let payload = layout.bundled_frontend_payload();
        let mut check = maild::frontend::check_installed_bundled_frontend(&installed, &payload);
        if check.is_err()
            && maild::frontend::installed_bundled_frontend_is_stale(&installed, &payload)
            && self.resync_stale_bundled_frontend(layout)
        {
            check = maild::frontend::check_installed_bundled_frontend(&installed, &payload);
        }
        match check {
            Ok(()) => (InPlace::Ready, Some(exe)),
            Err(error) => {
                eprintln!("{}: {error}", maild_brand::LAUNCHER_BIN);
                (InPlace::Damaged, None)
            }
        }
    }

    /// Same version, different build: this app was rebuilt or reinstalled while an Agent kept
    /// running, so the installed copy is the previous build. Ask that Agent to rerun its startup
    /// refresh, but only when it runs from this very app bundle, whose payload the check compares.
    /// Agents that predate `resyncBuiltinFrontend` fall back to `repairBuiltinFrontend`, which
    /// also persists the bundled frontend as default (it already is the default on this route).
    fn resync_stale_bundled_frontend(&self, layout: &NativeAppLayout) -> bool {
        let Ok(info) = self.call("getAgentInfo", json!({}), Duration::from_secs(2)) else {
            return false;
        };
        let same_bundle = info["executable_path"]
            .as_str()
            .is_some_and(|path| Path::new(path).starts_with(layout.app_bundle()));
        if !same_bundle {
            return false;
        }
        let response =
            match self.try_call("resyncBuiltinFrontend", json!({}), Duration::from_secs(60)) {
                Err(CallError::Rpc(Some(METHOD_NOT_FOUND))) => {
                    self.try_call("repairBuiltinFrontend", json!({}), Duration::from_secs(60))
                }
                other => other,
            };
        response.is_ok_and(|response| response["id"] == maild::recovery::BUNDLED_FRONTEND_ID)
    }

    fn request_mailbox(&self, target: &MailboxTarget) -> Result<Value, &'static str> {
        self.call(
            "openFrontend",
            json!({"frontend":target.id,"intent":{"route":"inbox"}}),
            Duration::from_secs(5),
        )
    }

    fn mailbox_target(&self) -> Result<MailboxTarget, &'static str> {
        let catalog = self.call("listFrontends", json!({}), Duration::from_secs(2))?;
        mailbox_target(&catalog).ok_or(text().error_mailbox_unavailable)
    }
}

/// What a normal (non-repair) open of the app bundle resolved to.
#[derive(Debug, Eq, PartialEq)]
pub enum LaunchOutcome {
    /// The Agent was asked to activate a (third-party) frontend; this process may exit.
    Requested,
    /// Host the bundled frontend in place: `exec` this `Contents/MacOS/<UI_BIN>` (ADR-0044).
    InPlace(PathBuf),
    /// Show the native recovery window.
    Recovery,
    /// A running mail window (possibly hidden in the menu bar) was woken and focused (ADR-0045);
    /// this process exits.
    Focused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InPlace {
    /// Same-bundle UI executable and the installed package are both healthy.
    Ready,
    /// No in-place executable (older bundle, development binary, overrides): ask the Agent.
    Unavailable,
    /// The installed bundled frontend is damaged: native recovery, never a window.
    Damaged,
}

#[derive(Debug)]
enum LaunchRoute {
    InPlace,
    Frontend(MailboxTarget),
    Recovery,
}

/// Pure routing of a `listFrontends` catalog (ADR-0044). `in_place` is only consulted when the
/// bundled frontend is the (catalogued, inbox-capable) default.
fn launch_route(catalog: &Value, in_place: impl FnOnce() -> InPlace) -> LaunchRoute {
    let Some(target) = mailbox_target(catalog) else {
        return LaunchRoute::Recovery;
    };
    if target.id != maild::recovery::BUNDLED_FRONTEND_ID {
        return LaunchRoute::Frontend(target);
    }
    match in_place() {
        InPlace::Ready => LaunchRoute::InPlace,
        InPlace::Unavailable => LaunchRoute::Frontend(target),
        InPlace::Damaged => LaunchRoute::Recovery,
    }
}

fn recovery_mailbox_host(
    catalog: &Value,
    main_window_running: bool,
    in_place: impl FnOnce() -> (InPlace, Option<PathBuf>),
) -> Result<Option<PathBuf>, &'static str> {
    let mut exe = None;
    match launch_route(catalog, || {
        let (check, path) = in_place();
        exe = path;
        check
    }) {
        LaunchRoute::InPlace if !main_window_running => Ok(exe),
        LaunchRoute::InPlace => Ok(None),
        LaunchRoute::Frontend(_) => Ok(None),
        LaunchRoute::Recovery => Err(text().error_mailbox_unavailable),
    }
}

#[derive(Debug)]
struct MailboxTarget {
    id: String,
    independent: bool,
}

impl MailboxTarget {
    /// The bundled frontend hosted in place (`on_demand`, never independent).
    fn bundled() -> Self {
        Self {
            id: maild::recovery::BUNDLED_FRONTEND_ID.to_owned(),
            independent: false,
        }
    }
}

fn mailbox_target(catalog: &Value) -> Option<MailboxTarget> {
    let id = catalog["default"].as_str()?;
    if id == BUILTIN_RECOVERY_FRONTEND {
        return None;
    }
    let frontend = catalog["frontends"]
        .as_array()?
        .iter()
        .find(|f| f["id"] == id)?;
    frontend["capabilities"]
        .as_array()?
        .iter()
        .any(|v| v == "inbox")
        .then(|| MailboxTarget {
            id: id.to_owned(),
            independent: frontend["lifecycle"] == "independent",
        })
}

fn mailbox_response(
    t: &'static Launcher,
    target: &MailboxTarget,
    response: &Value,
) -> Result<&'static str, &'static str> {
    if response["activation_requested"] == true {
        Ok(t.result_mailbox_requested)
    } else if target.independent && response["queued"] == true {
        Ok(t.result_mailbox_queued)
    } else {
        Err(t.error_mailbox_cannot_open)
    }
}

fn repair_response_confirmed(response: &Value) -> bool {
    matches!(
        response["id"].as_str(),
        Some(maild::recovery::STUDIO_FRONTEND_ID | maild::recovery::BUNDLED_FRONTEND_ID)
    )
}

impl Controller for NativeController {
    fn mailbox_host(&mut self) -> Result<Option<PathBuf>, &'static str> {
        let catalog = self.call("listFrontends", json!({}), Duration::from_secs(2))?;
        recovery_mailbox_host(&catalog, super::platform::main_window_pid().is_some(), || {
            self.in_place_ui()
        })
    }

    fn status(&mut self) -> Status {
        let online = self.online();
        // Every status refresh is also when the dialogs pick up a changed language preference.
        self.refresh_locale(online);
        let mailbox_available = online && self.mailbox_target().is_ok();
        Status {
            online,
            mailbox_available,
            can_manage: self.layout.is_some() && !self.overrides,
            login: if self.layout.is_some() {
                super::platform::status().into()
            } else {
                (text().login_manage_from_app)(maild_brand::APP_BUNDLE_NAME)
            },
        }
    }

    fn perform(&mut self, operation: Operation) -> Result<&'static str, &'static str> {
        match operation {
            Operation::OpenMailbox => {
                let target = self.mailbox_target()?;
                let response = self.request_mailbox(&target)?;
                mailbox_response(text(), &target, &response)
            }
            Operation::Repair => {
                self.managed()?;
                if self.status().online {
                    let response =
                        self.call("repairBuiltinFrontend", json!({}), Duration::from_secs(60))?;
                    if !repair_response_confirmed(&response) {
                        return Err(text().error_repair_unconfirmed);
                    }
                    Ok(text().result_repaired_online)
                } else {
                    maild::recovery::repair_offline()
                        .map_err(|_| text().error_repair_offline_failed)?;
                    Ok(text().result_repaired_offline)
                }
            }
            Operation::StartOnce => {
                let (exe, agent_app) = {
                    let layout = self.managed()?;
                    // maild-client's path checks are untranslated diagnostics: log them and show
                    // the localized "incomplete app" message instead.
                    let incomplete = |error: &str| {
                        eprintln!("{}: {error}", maild_brand::LAUNCHER_BIN);
                        text().error_app_incomplete
                    };
                    (
                        layout.agent_executable().map_err(incomplete)?,
                        layout.agent_app_bundle().map_err(incomplete)?,
                    )
                };
                if self.status().online {
                    return Ok(text().result_background_connected);
                }
                let paths = maild_paths::AppPaths::discover()
                    .map_err(|_| text().error_platform_dirs_unavailable)?;
                let managed = if paths.runtime_dir.starts_with(&paths.data_dir) {
                    &paths.data_dir
                } else {
                    &paths.runtime_dir
                };
                match maild::agent_lifecycle::AgentLifecycleLock::probe(managed, &paths.runtime_dir)
                {
                    Ok(maild::agent_lifecycle::AgentLifecycleStatus::Idle) => {}
                    _ => return Err(text().error_background_busy),
                }
                if self.client.is_none() {
                    return Err(text().error_background_config_unavailable);
                }
                // ADR-0044: in the real user session, LaunchServices owns the Agent as its own
                // `LSUIElement` app, outside this process's coalition, so the product Dock icon
                // goes dark as soon as the window closes. Isolated HOME trials keep a direct,
                // environment-preserving child (LaunchServices would use the real home).
                if super::platform::launches_through_workspace() {
                    super::platform::open_agent_app(&agent_app)?;
                    return Ok(text().result_start_requested);
                }
                let mut child = maild_client::native_child_command(&exe)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|_| text().error_start_failed)?;
                // Intentionally independent of the recovery/reader window lifetime (ADR-0039).
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                Ok(text().result_start_requested)
            }
            Operation::Stop => {
                self.managed()?;
                let response = self.call("stopManagedAgent", json!({}), Duration::from_secs(5))?;
                if response["stopping"] != true {
                    return Err(text().error_stop_unconfirmed);
                }
                Ok(text().result_stop_accepted)
            }
            Operation::RegisterLogin => {
                self.managed()?;
                super::platform::register()?;
                Ok(text().result_login_register_requested)
            }
            Operation::UnregisterLogin => {
                self.managed()?;
                super::platform::unregister()?;
                Ok(text().result_login_unregistered)
            }
            Operation::SystemSettings => {
                self.managed()?;
                super::platform::open_settings();
                Ok(text().result_login_settings_requested)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_hands_off_healthy_bundled_ui_without_workspace_activation() {
        let exe = PathBuf::from("/app/Contents/MacOS/ui");
        assert_eq!(
            recovery_mailbox_host(&catalog(maild_brand::BUNDLED_FRONTEND_ID), false, || {
                (InPlace::Ready, Some(exe.clone()))
            }),
            Ok(Some(exe))
        );
        assert_eq!(
            recovery_mailbox_host(&catalog("org.example.reader"), false, || panic!("third-party")),
            Ok(None)
        );
        assert_eq!(
            recovery_mailbox_host(&catalog(maild_brand::BUNDLED_FRONTEND_ID), false, || {
                (InPlace::Unavailable, None)
            }),
            Ok(None)
        );
        assert!(recovery_mailbox_host(&json!({}), false, || panic!("no default")).is_err());
        assert!(recovery_mailbox_host(&catalog(maild_brand::BUNDLED_FRONTEND_ID), false, || {
            (InPlace::Damaged, None)
        }).is_err());
    }

    #[test]
    fn recovery_uses_inbox_activation_for_an_existing_main_window() {
        assert_eq!(
            recovery_mailbox_host(&catalog(maild_brand::BUNDLED_FRONTEND_ID), true, || {
                (InPlace::Ready, Some(PathBuf::from("/app/Contents/MacOS/ui")))
            }),
            Ok(None)
        );
    }

    #[test]
    fn independent_delivery_is_queued_and_managed_activation_is_only_a_request() {
        let mut target = MailboxTarget {
            id: "reader".into(),
            independent: true,
        };
        let zh = &maild_l10n::ZH_HANS.launcher;
        let queued = json!({"queued":true,"activation_requested":false});
        let requested = json!({"queued":true,"activation_requested":true});
        assert!(mailbox_response(zh, &target, &queued).unwrap().contains("队列"));
        let en = &maild_l10n::EN.launcher;
        assert_eq!(
            mailbox_response(en, &target, &queued),
            Ok(en.result_mailbox_queued)
        );
        target.independent = false;
        assert!(mailbox_response(zh, &target, &queued).is_err());
        assert_eq!(
            mailbox_response(en, &target, &queued),
            Err(en.error_mailbox_cannot_open)
        );
        assert!(mailbox_response(zh, &target, &requested)
            .unwrap()
            .contains("确认邮件窗口打开后"));
        assert_eq!(
            mailbox_response(en, &target, &requested),
            Ok(en.result_mailbox_requested)
        );
    }

    #[test]
    fn call_errors_branch_on_rpc_codes_not_on_text() {
        for messages in [&maild_l10n::ZH_HANS, &maild_l10n::EN] {
            let t = &messages.launcher;
            assert_eq!(
                CallError::Rpc(Some(METHOD_NOT_FOUND)).message(t),
                t.error_unsupported_by_agent
            );
            assert_eq!(
                CallError::Rpc(Some(REPAIR_INCOMPLETE)).message(t),
                t.error_repair_incomplete
            );
            assert_eq!(CallError::Rpc(Some(-32603)).message(t), t.error_agent_unconfirmed);
            assert_eq!(CallError::Rpc(None).message(t), t.error_agent_unconfirmed);
            assert_eq!(CallError::Unconfirmed.message(t), t.error_operation_unconfirmed);
            assert_eq!(
                CallError::NoConnection.message(t),
                t.error_local_connection_unavailable
            );
        }
        assert_eq!(
            maild_l10n::ZH_HANS.launcher.error_unsupported_by_agent,
            "后台版本不支持此操作，请先退出旧后台并使用完整的新版本应用"
        );
    }
    #[test]
    fn mailbox_target_requires_an_actual_inbox_frontend() {
        for (id, capabilities, expected) in [
            ("builtin-repair", json!(["background"]), None),
            ("settings.only", json!(["settings"]), None),
            ("reader", json!(["inbox", "message"]), Some("reader")),
        ] {
            let catalog = json!({"default":id,"frontends":[{"id":id,"capabilities":capabilities}]});
            assert_eq!(
                mailbox_target(&catalog).map(|target| target.id).as_deref(),
                expected
            );
        }
        assert!(mailbox_target(&json!({"default":"missing","frontends":[]})).is_none());
    }
    fn catalog(default: &str) -> Value {
        json!({"default":default,"frontends":[
            {"id":"builtin-repair","capabilities":["background"]},
            {"id":maild_brand::BUNDLED_FRONTEND_ID,"version":"0.1.0","capabilities":["inbox","message","compose","settings","background"],"lifecycle":"on_demand"},
            {"id":"org.example.reader","capabilities":["inbox"],"lifecycle":"on_demand"},
        ]})
    }
    #[test]
    fn healthy_bundled_default_is_hosted_in_place() {
        let route = launch_route(&catalog(maild_brand::BUNDLED_FRONTEND_ID), || InPlace::Ready);
        assert!(matches!(route, LaunchRoute::InPlace));
    }
    #[test]
    fn bundled_default_without_in_place_ui_still_asks_the_agent() {
        let route = launch_route(&catalog(maild_brand::BUNDLED_FRONTEND_ID), || InPlace::Unavailable);
        assert!(matches!(route, LaunchRoute::Frontend(target) if target.id == maild_brand::BUNDLED_FRONTEND_ID));
    }
    #[test]
    fn third_party_default_uses_open_frontend_without_in_place_checks() {
        let route = launch_route(&catalog("org.example.reader"), || {
            panic!("third-party frontends are never hosted in place")
        });
        assert!(matches!(route, LaunchRoute::Frontend(target) if target.id == "org.example.reader"));
    }
    #[test]
    fn missing_or_damaged_bundled_frontend_shows_recovery() {
        // Removed/unloadable bundled frontend: the daemon reports the native repair default.
        let route = launch_route(&catalog("builtin-repair"), || panic!("no window to host"));
        assert!(matches!(route, LaunchRoute::Recovery));
        // A default that is not in the catalog any more.
        let route = launch_route(
            &json!({"default":maild_brand::BUNDLED_FRONTEND_ID,"frontends":[]}),
            || panic!("no window to host"),
        );
        assert!(matches!(route, LaunchRoute::Recovery));
        // Same-version bytes differ from the bundled checksum.
        let route = launch_route(&catalog(maild_brand::BUNDLED_FRONTEND_ID), || InPlace::Damaged);
        assert!(matches!(route, LaunchRoute::Recovery));
    }

    #[test]
    fn repair_confirmation_accepts_studio_and_legacy_bundled_frontends_only() {
        assert!(repair_response_confirmed(&json!({
            "id": maild::recovery::STUDIO_FRONTEND_ID
        })));
        assert!(repair_response_confirmed(&json!({
            "id": maild::recovery::BUNDLED_FRONTEND_ID
        })));
        assert!(!repair_response_confirmed(&json!({
            "id": "org.example.third-party"
        })));
        assert!(!repair_response_confirmed(&json!({})));
    }
}
