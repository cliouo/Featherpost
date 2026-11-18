use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::{
    ActivationSink, DriverStatus, Notification, NotificationAuthorization,
    NotificationAvailability, OpenIntent, PlatformDriver,
};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
#[used]
static AGENT_ID_CONTRACT: &str = maild_brand::agent_id_contract!();

/// 让无窗口的 Agent 进程与 WindowServer 建立一次 GUI 会话连接。
///
/// headless 进程从不连接 WindowServer，LaunchServices 因此不会给它 ASN
/// （`lsappinfo info <pid>` 显示 `bundleID=[NULL]` 与 `!cgsConnection`），usernoted 无法把
/// 授权请求归属到任何已注册 app —— `requestAuthorization` 于是被静默丢弃：回调立刻返回、
/// 不带 NSError、系统不弹窗、授权状态原地停在 `not_determined`。这条路径经 `open` 由
/// LaunchServices 拉起也一样，因为缺的是 WindowServer 连接而不是启动方式。
///
/// `NSApplicationLoad` 只建立这条连接，不接管 run loop；Info.plist 的 `LSUIElement`
/// 继续保证没有 Dock 图标和菜单栏。必须在主线程、通知 worker 启动之前调用一次。
pub fn prepare_gui_session() -> bool {
    // objc2 只是把这个自由函数重命名到 `NSApplication::load`，而拿到那个类型要额外拉进
    // `NSResponder` 整棵生成代码。这里保留 AppKit 的 C 入口。
    #[allow(deprecated)]
    objc2_app_kit::NSApplicationLoad()
}

enum Center {
    Ready(objc2::rc::Retained<UNUserNotificationCenter>),
    IdentityMismatch,
}

struct NotificationDelegateIvars {
    activation: Arc<ActivationSink>,
    targets: Arc<Mutex<VecDeque<(String, OpenIntent)>>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = NotificationDelegateIvars]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            if let Some(intent) = take_target(&self.ivars().targets, &identifier) {
                self.ivars().activation.activate(intent);
            }
            completion_handler.call(());
        }
    }
);

impl NotificationDelegate {
    fn new(
        activation: Arc<ActivationSink>,
        targets: Arc<Mutex<VecDeque<(String, OpenIntent)>>>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(NotificationDelegateIvars {
            activation,
            targets,
        });
        // SAFETY: NSObject's init signature is correct and the ivars are initialized above.
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) struct MacosDriver {
    center: Center,
    _delegate: Option<Retained<NotificationDelegate>>,
    targets: Arc<Mutex<VecDeque<(String, OpenIntent)>>>,
    pending_authorization: Option<Receiver<Result<bool, ()>>>,
    worker_waker: std::thread::Thread,
}

impl MacosDriver {
    pub(super) fn new(activation: Arc<ActivationSink>) -> Self {
        let expected = expected_agent_identifier();
        let actual = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|identifier| identifier.to_string());
        let targets = Arc::new(Mutex::new(VecDeque::with_capacity(64)));
        let (center, delegate) = if actual.as_deref() == Some(expected) {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let delegate = NotificationDelegate::new(activation, targets.clone());
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            (Center::Ready(center), Some(delegate))
        } else {
            (Center::IdentityMismatch, None)
        };
        Self {
            center,
            _delegate: delegate,
            targets,
            pending_authorization: None,
            worker_waker: std::thread::current(),
        }
    }

    fn ready_center(&self) -> Result<&UNUserNotificationCenter, DriverStatus> {
        match &self.center {
            Center::Ready(center) => Ok(center),
            Center::IdentityMismatch => Err(DriverStatus {
                availability: NotificationAvailability::IdentityMismatch,
                authorization: NotificationAuthorization::Unknown,
                request_in_flight: false,
            }),
        }
    }

    fn query_settings(&self) -> Result<DriverStatus, &'static str> {
        let center = match self.ready_center() {
            Ok(center) => center,
            Err(status) => return Ok(status),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let callback = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let authorization =
                map_authorization(unsafe { settings.as_ref() }.authorizationStatus());
            let _ = tx.try_send(authorization);
        });
        center.getNotificationSettingsWithCompletionHandler(&callback);
        rx.recv_timeout(CALLBACK_TIMEOUT)
            .map(|authorization| DriverStatus {
                availability: NotificationAvailability::Available,
                authorization,
                request_in_flight: false,
            })
            .map_err(|_| "notification settings callback timed out")
    }

    fn pending_status() -> DriverStatus {
        DriverStatus {
            availability: NotificationAvailability::Available,
            authorization: NotificationAuthorization::Checking,
            request_in_flight: true,
        }
    }

    fn poll_pending_completion(&mut self) -> Option<Result<DriverStatus, &'static str>> {
        let completion = match self.pending_authorization.as_ref()?.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return None,
            // 发送端随 completion handler 一起被释放却从未调用：系统收下了请求又丢掉了它。
            // 这不是「授权失败」，是「根本没问」，两者对用户的下一步动作完全不同。
            Err(TryRecvError::Disconnected) => Err(()),
        };
        self.pending_authorization = None;
        Some(match completion {
            Ok(granted) => match self.query_settings() {
                // 系统既没授予，又没把状态推离 not_determined —— 也就是它根本没问用户。
                // 早期版本只看 NSError，于是把这种「静默丢弃」当成成功，UI 只能显示
                // 「未决定」，按钮永远可点而什么都不会发生。这里必须报错。
                Ok(status)
                    if !granted
                        && status.authorization == NotificationAuthorization::NotDetermined =>
                {
                    eprintln!(
                        "[maild/notify] 系统未询问用户就结束了授权请求（granted=false，状态仍为 not_determined）"
                    );
                    Err("system dropped the notification authorization request")
                }
                Ok(status) => {
                    if granted && status.authorization != NotificationAuthorization::Authorized {
                        eprintln!(
                            "[maild/notify] 授权回调 granted=true，但系统随后报告 {:?}",
                            status.authorization
                        );
                    }
                    Ok(status)
                }
                other => other,
            },
            Err(()) => {
                eprintln!(
                    "[maild/notify] 系统丢弃了通知授权请求：completion handler 从未被调用，\
                     未弹窗也未改变授权状态。常见原因是同一 bundle id 已存在旧签名的授权记录\
                     （ad-hoc 预览版每次重建都会换签名），可在「系统设置 → 通知」里检查 {} Agent。",
                    maild_brand::PRODUCT_NAME
                );
                Err("system discarded the notification authorization request")
            }
        })
    }
}

impl PlatformDriver for MacosDriver {
    fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        if let Some(result) = self.poll_pending_completion() {
            return result;
        }
        if self.pending_authorization.is_some() {
            return Ok(Self::pending_status());
        }
        self.query_settings()
    }

    fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        if let Some(result) = self.poll_pending_completion() {
            return result;
        }
        if self.pending_authorization.is_some() {
            return Ok(Self::pending_status());
        }
        let center = match self.ready_center() {
            Ok(center) => center,
            Err(status) => return Ok(status),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let worker_waker = self.worker_waker.clone();
        let callback = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            let result = if error.is_null() {
                Ok(granted.as_bool())
            } else {
                Err(())
            };
            let _ = tx.try_send(result);
            worker_waker.unpark();
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &callback,
        );
        self.pending_authorization = Some(rx);
        Ok(Self::pending_status())
    }

    fn deliver(&mut self, notification: &Notification) -> Result<(), &'static str> {
        let center = self
            .ready_center()
            .map_err(|_| "notification identity mismatch")?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        let identifier_text = format!("maild-{}-{}", std::process::id(), notification.id);
        if let Some(target) = notification.target.clone() {
            record_target(&self.targets, identifier_text.clone(), target);
        }
        let identifier = NSString::from_str(&identifier_text);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );
        let (tx, rx) = mpsc::sync_channel(1);
        let callback = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.try_send(error.is_null());
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
        match rx.recv_timeout(CALLBACK_TIMEOUT) {
            Ok(true) => Ok(()),
            Ok(false) => {
                take_target(&self.targets, &identifier_text);
                Err("notification delivery failed")
            }
            Err(_) => {
                take_target(&self.targets, &identifier_text);
                Err("notification delivery callback timed out")
            }
        }
    }

    fn poll_authorization(&mut self) -> Option<Result<DriverStatus, &'static str>> {
        self.poll_pending_completion()
    }
}

fn record_target(
    targets: &Mutex<VecDeque<(String, OpenIntent)>>,
    identifier: String,
    intent: OpenIntent,
) {
    let mut targets = targets.lock().unwrap();
    if targets.len() == 64 {
        targets.pop_front();
    }
    targets.push_back((identifier, intent));
}

fn take_target(
    targets: &Mutex<VecDeque<(String, OpenIntent)>>,
    identifier: &str,
) -> Option<OpenIntent> {
    let mut targets = targets.lock().unwrap();
    let index = targets
        .iter()
        .position(|(candidate, _)| candidate == identifier)?;
    targets.remove(index).map(|(_, intent)| intent)
}

fn expected_agent_identifier() -> &'static str {
    maild_brand::AGENT_BUNDLE_ID
}

fn map_authorization(status: UNAuthorizationStatus) -> NotificationAuthorization {
    match status {
        UNAuthorizationStatus::NotDetermined => NotificationAuthorization::NotDetermined,
        UNAuthorizationStatus::Denied => NotificationAuthorization::Denied,
        UNAuthorizationStatus::Authorized => NotificationAuthorization::Authorized,
        UNAuthorizationStatus::Provisional => NotificationAuthorization::Provisional,
        UNAuthorizationStatus::Ephemeral => NotificationAuthorization::Ephemeral,
        _ => NotificationAuthorization::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_identity_comes_from_brand() {
        assert_eq!(expected_agent_identifier(), maild_brand::AGENT_BUNDLE_ID);
        let contract: &str = AGENT_ID_CONTRACT;
        assert!(contract.ends_with(&format!("{}\0", maild_brand::AGENT_BUNDLE_ID)));
    }

    #[test]
    fn unknown_authorization_values_remain_forward_compatible() {
        assert_eq!(
            map_authorization(UNAuthorizationStatus(99)),
            NotificationAuthorization::Unknown
        );
    }

    #[test]
    fn notification_targets_are_bounded_and_one_shot() {
        let targets = Mutex::new(VecDeque::new());
        for index in 0..65 {
            record_target(
                &targets,
                format!("maild-{index}"),
                OpenIntent::Message {
                    folder: None,
                    account: "main".into(),
                    uid: index.to_string(),
                },
            );
        }
        assert_eq!(targets.lock().unwrap().len(), 64);
        assert_eq!(take_target(&targets, "maild-0"), None);
        assert!(matches!(
            take_target(&targets, "maild-64"),
            Some(OpenIntent::Message { uid, .. }) if uid == "64"
        ));
        assert_eq!(take_target(&targets, "maild-64"), None);
    }
}
