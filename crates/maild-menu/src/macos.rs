use std::cell::{Cell, OnceCell};
use std::time::Duration;

use maild_client::{
    discover_endpoint, DiscoveryOptions, Endpoint, OpenIntent, RpcClient,
};
use maild_l10n::Locale;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, ClassType, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSMenu, NSMenuDelegate, NSMenuItem,
    NSRunningApplication, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{
    MainThreadMarker, NSLocale, NSNotification, NSObject, NSObjectProtocol, NSString,
};
use tokio::runtime::{Builder, Handle};

use crate::locale::{resolve, LatestLocale};

#[used]
static MENU_ID_CONTRACT: &str = maild_brand::menu_id_contract!();

/// The RPC worker only ever runs a couple of short local requests, so it is a single
/// current-thread runtime driven by one small background thread instead of a multi-thread
/// scheduler (no idle worker pool, no blocking-pool threads kept warm).
const RPC_THREAD_STACK_BYTES: usize = 256 * 1024;

/// Upper bound for the `stopManagedAgent` round trip; a wedged daemon must not wedge "Quit".
const STOP_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// After asking the Agent to stop, it reaps this process ~350ms later during teardown. If we are
/// still alive after this grace period (e.g. the Agent was not running), exit ourselves.
const SELF_EXIT_GRACE: Duration = Duration::from_millis(600);
/// Upper bound for one `getLocalePreference` round trip on the RPC thread.
const LOCALE_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// How long opening the menu waits for that answer on the main thread. A local socket normally
/// answers in a few milliseconds; past this the menu opens in the last known language and a late
/// answer applies on the next opening.
const LOCALE_MENU_WAIT: Duration = Duration::from_millis(250);

struct MenuDelegateIvars {
    runtime: Handle,
    endpoint: Option<Endpoint>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    menu: OnceCell<Retained<NSMenu>>,
    quitting: Cell<bool>,
    /// Latest negotiated language (written by the RPC thread, read on the main thread).
    locale: LatestLocale,
    /// Language the current menu item titles were built in.
    shown: Cell<Option<Locale>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuDelegateIvars]
    struct MenuDelegate;

    unsafe impl NSObjectProtocol for MenuDelegate {}

    unsafe impl NSApplicationDelegate for MenuDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.install_menu();
        }
    }

    unsafe impl NSMenuDelegate for MenuDelegate {
        /// AppKit calls this right before the menu is displayed: re-read the language preference
        /// (ADR-0048 decision 6) and retitle the items if it changed since the last opening.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            let locale = self.refresh_locale(LOCALE_MENU_WAIT);
            if self.ivars().shown.get() != Some(locale) {
                menu.removeAllItems();
                self.populate(menu, locale);
            }
        }
    }

    impl MenuDelegate {
        #[unsafe(method(openInbox:))]
        fn open_inbox(&self, _sender: Option<&AnyObject>) {
            self.submit(OpenIntent::Inbox { account: None });
        }

        #[unsafe(method(syncMail:))]
        fn sync_mail(&self, _sender: Option<&AnyObject>) {
            self.sync_now();
        }

        #[unsafe(method(openCompose:))]
        fn open_compose(&self, _sender: Option<&AnyObject>) {
            self.submit(OpenIntent::Compose);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            self.submit(OpenIntent::Settings);
        }

        #[unsafe(method(openBackground:))]
        fn open_background(&self, _sender: Option<&AnyObject>) {
            self.submit(OpenIntent::Background);
        }

        #[unsafe(method(openBuiltinInbox:))]
        fn open_builtin_inbox(&self, _sender: Option<&AnyObject>) {
            // Direct fixed-bundle entry works even when the daemon/socket is unavailable.
            if maild_client::open_native_recovery().is_err() {
                eprintln!("[maild-menu] native recovery entry unavailable");
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: Option<&AnyObject>) {
            self.quit_all();
        }
    }
);

impl MenuDelegate {
    fn new(mtm: MainThreadMarker) -> Result<Retained<Self>, String> {
        let runtime = spawn_rpc_runtime()?;
        let endpoint = discover_endpoint(&DiscoveryOptions::default()).ok();
        let this = Self::alloc(mtm).set_ivars(MenuDelegateIvars {
            runtime,
            endpoint,
            status_item: OnceCell::new(),
            menu: OnceCell::new(),
            quitting: Cell::new(false),
            // Until the daemon answers, the OS preferred languages decide.
            locale: LatestLocale::new(resolve(None, system_languages)),
            shown: Cell::new(None),
        });
        // SAFETY: NSObject's init signature is correct and all ivars are initialized.
        Ok(unsafe { msg_send![super(this), init] })
    }

    fn install_menu(&self) {
        let mtm = self.mtm();
        let status_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            button.setTitle(&NSString::from_str("✉"));
            button.setToolTip(Some(&NSString::from_str(maild_brand::PRODUCT_NAME)));
        }
        let menu = NSMenu::initWithTitle(
            NSMenu::alloc(mtm),
            &NSString::from_str(maild_brand::PRODUCT_NAME),
        );
        self.populate(&menu, self.ivars().locale.get());
        // The menu holds its delegate weakly; this delegate lives as long as the application.
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        status_item.setMenu(Some(&menu));
        let _ = self.ivars().menu.set(menu);
        let _ = self.ivars().status_item.set(status_item);
        // Ask the daemon now so the first opening usually already has its answer.
        let _ = self.request_locale();
    }

    /// Fill `menu` with every item titled in `locale`.
    fn populate(&self, menu: &NSMenu, locale: Locale) {
        let mtm = self.mtm();
        let m = &locale.messages().menu;
        self.add_item(menu, m.open_inbox, sel!(openInbox:));
        self.add_item(menu, m.sync_now, sel!(syncMail:));
        self.add_item(menu, m.compose, sel!(openCompose:));
        self.add_item(menu, m.settings, sel!(openSettings:));
        self.add_item(menu, m.background_and_notifications, sel!(openBackground:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(menu, m.repair_and_background, sel!(openBuiltinInbox:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(menu, &(m.quit)(maild_brand::PRODUCT_NAME), sel!(quitApp:));
        self.ivars().shown.set(Some(locale));
    }

    /// Re-negotiate the menu language and wait up to `wait` for the answer; past that, return
    /// the last known language (the late answer is still recorded for the next opening).
    fn refresh_locale(&self, wait: Duration) -> Locale {
        match self.request_locale() {
            Some(answer) => answer
                .recv_timeout(wait)
                .unwrap_or_else(|_| self.ivars().locale.get()),
            None => self.ivars().locale.get(),
        }
    }

    /// Start one language refresh: the daemon's `effective` list when it answers, otherwise the
    /// OS preferred languages. The result is recorded in `locale` and also sent on the returned
    /// channel. `None` when no RPC could be started (the OS languages were applied directly).
    fn request_locale(&self) -> Option<std::sync::mpsc::Receiver<Locale>> {
        let latest = self.ivars().locale.clone();
        let Some(endpoint) = self.ivars().endpoint.clone() else {
            latest.set(resolve(None, system_languages));
            return None;
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.ivars().runtime.spawn(async move {
            let effective = daemon_effective(endpoint).await;
            let locale = resolve(effective.as_deref(), system_languages);
            latest.set(locale);
            let _ = sender.try_send(locale);
        });
        Some(receiver)
    }

    fn add_item(&self, menu: &NSMenu, title: &str, action: objc2::runtime::Sel) {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                Some(action),
                &NSString::new(),
            )
        };
        // SAFETY: the delegate outlives the menu and implements every selector installed above.
        unsafe { item.setTarget(Some(self)) };
        menu.addItem(&item);
    }

    /// Ask the daemon to sync now. Failures are one short stderr line; the menu never crashes.
    fn sync_now(&self) {
        let Some(endpoint) = self.ivars().endpoint.clone() else {
            eprintln!("[maild-menu] local API endpoint unavailable");
            return;
        };
        self.ivars().runtime.spawn(async move {
            let result = match RpcClient::new(endpoint) {
                // `{}` params. maild-menu deliberately has no direct serde_json dependency;
                // `Value: FromIterator<(K, V)>` builds an empty JSON object from the inferred
                // parameter type instead.
                Ok(client) => client
                    .call("syncMessages", std::iter::empty::<(String, bool)>().collect())
                    .await
                    .map(|_| ()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                eprintln!("[maild-menu] sync request failed");
            }
        });
    }

    /// "Quit <PRODUCT_NAME>": stop the whole managed Agent (plugins, this menu, open frontend windows).
    ///
    /// Online path: `stopManagedAgent` makes the Agent push `agent/stopping` to frontends and
    /// tear everything down, including this process. Offline fallback: terminate a leftover main
    /// UI found through `.main-ui.lock`. Either way this process exits after a short grace period
    /// so the status item can never be left stranded.
    fn quit_all(&self) {
        if self.ivars().quitting.replace(true) {
            return;
        }
        let endpoint = self.ivars().endpoint.clone();
        self.ivars().runtime.spawn(async move {
            let stopped = match endpoint.clone() {
                Some(endpoint) => request_agent_stop(endpoint).await,
                None => false,
            };
            if !stopped {
                eprintln!("[maild-menu] Agent unreachable; closing leftover windows directly");
                terminate_leftover_main_ui(endpoint.as_ref());
            }
            tokio::time::sleep(SELF_EXIT_GRACE).await;
            std::process::exit(0);
        });
    }

    fn submit(&self, intent: OpenIntent) {
        self.submit_to(None, intent);
    }

    fn submit_to(&self, frontend: Option<&'static str>, intent: OpenIntent) {
        let Some(endpoint) = self.ivars().endpoint.clone() else {
            eprintln!("[maild-menu] local API endpoint unavailable");
            return;
        };
        self.ivars().runtime.spawn(async move {
            let result = match RpcClient::new(endpoint) {
                Ok(client) => client
                    .open_frontend_with(frontend, &intent)
                    .await
                    .map(|_| ()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                eprintln!("[maild-menu] frontend request failed");
            }
        });
    }
}

/// The daemon's `effective` language list, or `None` when it is unreachable, too slow, or an
/// older Agent without `getLocalePreference`.
async fn daemon_effective(endpoint: Endpoint) -> Option<Vec<String>> {
    let client = RpcClient::new(endpoint).ok()?;
    let answer = tokio::time::timeout(LOCALE_REQUEST_TIMEOUT, client.locale_preference()).await;
    Some(answer.ok()?.ok()?.effective)
}

/// This process's OS preferred languages (`+[NSLocale preferredLanguages]`, the same source the
/// daemon reads in `system` mode). Used only when the daemon cannot answer.
fn system_languages() -> Vec<String> {
    objc2::rc::autoreleasepool(|_| {
        let languages = NSLocale::preferredLanguages();
        (0..languages.count())
            .map(|index| languages.objectAtIndex(index).to_string())
            .collect()
    })
}

/// Ask the Agent for a managed graceful stop. `true` only when it acknowledged in time.
async fn request_agent_stop(endpoint: Endpoint) -> bool {
    let Ok(client) = RpcClient::new(endpoint) else {
        return false;
    };
    // `{}` params, built without a direct serde_json dependency (see `sync_now`).
    let call = client.call(
        "stopManagedAgent",
        std::iter::empty::<(String, bool)>().collect(),
    );
    matches!(
        tokio::time::timeout(STOP_REQUEST_TIMEOUT, call).await,
        Ok(Ok(_))
    )
}

/// Offline fallback: the Agent is gone but a main UI may still hold `.main-ui.lock` in the
/// runtime directory (the parent of the local socket). Ask that app to terminate normally so it
/// still gets to flush drafts.
fn terminate_leftover_main_ui(endpoint: Option<&Endpoint>) {
    let Some(Endpoint::Unix { path }) = endpoint else {
        return;
    };
    let Some(runtime_dir) = path.parent() else {
        return;
    };
    let Some(Some(pid)) = maild_client::running_main_ui(runtime_dir) else {
        return;
    };
    if pid <= 0 || u32::try_from(pid).is_ok_and(|pid| pid == std::process::id()) {
        return;
    }
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: `+[NSRunningApplication runningApplicationWithProcessIdentifier:]` takes a
        // `pid_t` (i32) and returns a nullable instance. Called via `msg_send!` because the typed
        // binding is gated behind objc2-app-kit's `libc` feature.
        let app: Option<Retained<NSRunningApplication>> = unsafe {
            msg_send![
                NSRunningApplication::class(),
                runningApplicationWithProcessIdentifier: pid
            ]
        };
        match app {
            Some(app) if app.terminate() => {}
            _ => eprintln!("[maild-menu] leftover main UI did not accept terminate"),
        }
    });
}

/// Build a current-thread runtime and park it on one named background thread for the life of
/// the process; callers only keep the [`Handle`] to spawn requests onto it.
fn spawn_rpc_runtime() -> Result<Handle, String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("menu runtime unavailable: {error}"))?;
    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name("maild-menu-rpc".into())
        .stack_size(RPC_THREAD_STACK_BYTES)
        .spawn(move || runtime.block_on(std::future::pending::<()>()))
        .map_err(|error| format!("menu runtime thread unavailable: {error}"))?;
    Ok(handle)
}

pub(super) fn run() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("{}: must start on the main thread", maild_brand::MENU_BIN);
        return;
    };
    let delegate = match MenuDelegate::new(mtm) {
        Ok(delegate) => delegate,
        Err(error) => {
            eprintln!("{}: {error}", maild_brand::MENU_BIN);
            return;
        }
    };
    let application = NSApplication::sharedApplication(mtm);
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
}
