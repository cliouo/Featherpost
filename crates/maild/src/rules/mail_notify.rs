//! 普适收件箱新邮件通知（ADR-0043）。
//!
//! 监听 `MailFiltered`（只有主文件夹/收件箱新邮件会发它；非收件箱同步直接落库不发），
//! 在当前生效的 [`RulesSettings`] 打开 `notify_new_mail` 时发系统通知：
//!
//! 1. 跳过：已读（`seen`，在别的设备读过）、静音账户、`Date` 早于 24 小时的积压信、
//!    同一 (account, folder, uid) 重复投递、会被 OTP 识别的验证码信（由 `OtpPlugin`
//!    发专属通知，不重复）；
//! 2. 走 `QuietHoursQuery` + `NotifyRouteEvent`（`sensitive: false`）：静默时段与
//!    `silent_keywords` 照常生效；
//! 3. 隐私：`show_sensitive = false` 时标题/正文不含发件人、主题与摘要；
//! 4. 防刷屏：同一账户 10 秒滑动窗口内超过 3 封时不再逐封弹窗，窗口内只发一条合并提示，
//!    指向该账户收件箱。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cordis::Context;

use super::{RulesConfig, RulesSettings};
use crate::events::{
    MailFiltered, MailMessage, NotifyRoute, NotifyRouteEvent, QuietHoursQuery, QuietHoursRequest,
};
use crate::frontend::OpenIntent;
use crate::notify::Notifier;
use crate::otp::OtpConfig;
use crate::store::Store;

/// 滑动窗口长度。
pub const BURST_WINDOW: Duration = Duration::from_secs(10);
/// 窗口内逐封通知的上限。
pub const BURST_LIMIT: usize = 3;
/// `Date` 头早于此的信视为积压（首次同步历史未读），不通知。
pub const STALE_AFTER_SECS: u64 = 24 * 60 * 60;
/// 通知去重记忆的 (account, folder, uid) 条数上限。
const DEDUP_CAPACITY: usize = 4096;
/// 限流器同时跟踪的账户数上限（超出时清掉已过窗口的账户）。
const MAX_TRACKED_ACCOUNTS: usize = 256;
/// 摘要预览字符上限。
const PREVIEW_CHARS: usize = 80;

/// 脱敏模式下的通知标题。
pub const MASKED_TITLE: &str = "新邮件";

/// 一封信在限流器里的去向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// 逐封通知。
    Single,
    /// 本窗口第一次超限：发一条合并提示。
    Summary,
    /// 已发过合并提示：本封抑制。
    Suppressed,
}

#[derive(Default)]
struct AccountWindow {
    /// 最近至多 `BURST_LIMIT + 1` 次到达时刻。
    arrivals: VecDeque<Instant>,
    summary_at: Option<Instant>,
}

/// 按账户的滑动窗口限流器（纯状态机，时刻由调用方给出，便于测试）。
#[derive(Default)]
pub struct BurstLimiter {
    accounts: HashMap<String, AccountWindow>,
}

impl BurstLimiter {
    pub fn admit(&mut self, account: &str, now: Instant) -> Admission {
        if self.accounts.len() >= MAX_TRACKED_ACCOUNTS && !self.accounts.contains_key(account) {
            self.accounts.retain(|_, window| {
                window
                    .arrivals
                    .back()
                    .is_some_and(|last| now.saturating_duration_since(*last) < BURST_WINDOW)
            });
        }
        let window = self.accounts.entry(account.to_string()).or_default();
        window.arrivals.push_back(now);
        while window.arrivals.len() > BURST_LIMIT + 1 {
            window.arrivals.pop_front();
        }
        let over_limit = window.arrivals.len() > BURST_LIMIT
            && window
                .arrivals
                .front()
                .is_some_and(|first| now.saturating_duration_since(*first) < BURST_WINDOW);
        if !over_limit {
            return Admission::Single;
        }
        let summary_due = window
            .summary_at
            .is_none_or(|at| now.saturating_duration_since(at) >= BURST_WINDOW);
        if summary_due {
            window.summary_at = Some(now);
            Admission::Summary
        } else {
            Admission::Suppressed
        }
    }
}

#[derive(Default)]
struct Dedup {
    seen: HashSet<(String, String, String)>,
    order: VecDeque<(String, String, String)>,
}

impl Dedup {
    /// 首次见到返回 true。
    fn insert(&mut self, key: (String, String, String)) -> bool {
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > DEDUP_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

#[derive(Default)]
struct NotifyState {
    dedup: Dedup,
    limiter: BurstLimiter,
}

/// 一条待发的系统通知。
#[derive(Clone, Debug, PartialEq)]
pub struct MailNotification {
    pub title: String,
    pub body: String,
    pub target: OpenIntent,
}

/// 在 `ctx` 上挂普适新邮件通知监听器（由 `RulesPlugin::apply` 调用）。
pub(crate) fn mount(ctx: &Context, settings: RulesSettings) {
    let state = Arc::new(Mutex::new(NotifyState::default()));
    let ctx2 = ctx.clone();
    ctx.on_emit_scoped::<MailFiltered>(move |msg| {
        let cfg = settings.snapshot();
        if !cfg.notify_new_mail {
            return;
        }
        handle(&ctx2, &cfg, &state, &msg);
    });
}

fn handle(ctx: &Context, cfg: &RulesConfig, state: &Mutex<NotifyState>, msg: &MailMessage) {
    if !eligible(cfg, msg, unix_now_secs()) {
        return;
    }
    // 验证码信交给 OtpPlugin（专属通知）；无论监听器先后，两道检查都能挡住重复。
    if ctx
        .get::<OtpConfig>()
        .is_some_and(|otp| otp.detect(&msg.body, &msg.subject).is_some())
    {
        return;
    }
    if let Some(store) = ctx.get::<Store>() {
        if store.is_notified(&msg.account, &msg.uid).unwrap_or(false) {
            return;
        }
    }
    let quiet = ctx
        .bail::<QuietHoursQuery>(QuietHoursRequest {
            account: msg.account.clone(),
            at_unix_secs: unix_now_secs(),
        })
        .unwrap_or(false);
    let default = NotifyRoute {
        channel: if quiet { "silent" } else { "system" }.into(),
        sensitive: false,
        subject: msg.subject.clone(),
        from: msg.sender.clone(),
    };
    let route = ctx
        .bail::<NotifyRouteEvent>(default.clone())
        .unwrap_or(default);
    if route.channel == "silent" {
        return;
    }
    let admission = {
        let mut state = state.lock().unwrap();
        if !state
            .dedup
            .insert((msg.account.clone(), msg.folder.clone(), msg.uid.clone()))
        {
            return;
        }
        state.limiter.admit(&msg.account, Instant::now())
    };
    let Some(notification) = compose(cfg, msg, admission) else {
        return;
    };
    if let Some(notifier) = ctx.get::<Notifier>() {
        notifier.notify_with_target(
            &notification.title,
            &notification.body,
            Some(notification.target),
        );
    }
}

/// 与上下文无关的资格检查：开关、已读、静音、积压。
pub fn eligible(cfg: &RulesConfig, msg: &MailMessage, now_unix_secs: u64) -> bool {
    if !cfg.notify_new_mail || msg.seen || msg.deleted {
        return false;
    }
    if cfg.is_account_muted(&msg.account) {
        return false;
    }
    let effective_ts = msg
        .date_at
        .or_else(|| {
            (msg.sort_at >= crate::mime::EARLIEST_PLAUSIBLE_DATE_SECS).then_some(msg.sort_at)
        })
        .or_else(|| {
            (msg.received_at >= crate::mime::EARLIEST_PLAUSIBLE_DATE_SECS)
                .then_some(msg.received_at)
        });
    if effective_ts.is_some_and(|ts| ts.saturating_add(STALE_AFTER_SECS) < now_unix_secs) {
        return false;
    }
    true
}

/// 按隐私开关与限流结论组织通知文案（纯函数）。
pub fn compose(
    cfg: &RulesConfig,
    msg: &MailMessage,
    admission: Admission,
) -> Option<MailNotification> {
    match admission {
        Admission::Suppressed => None,
        Admission::Summary => Some(MailNotification {
            title: MASKED_TITLE.into(),
            body: format!("账户 {} 收到多封新邮件", msg.account),
            target: OpenIntent::Inbox {
                account: Some(msg.account.clone()),
            },
        }),
        Admission::Single => {
            let target = OpenIntent::Message {
                folder: (!msg.folder.is_empty()).then(|| msg.folder.clone()),
                account: msg.account.clone(),
                uid: msg.uid.clone(),
            };
            if !cfg.show_sensitive {
                return Some(MailNotification {
                    title: MASKED_TITLE.into(),
                    body: format!("账户 {} 收到一封新邮件", msg.account),
                    target,
                });
            }
            let subject = msg.subject.trim();
            let subject = if subject.is_empty() {
                "（无主题）"
            } else {
                subject
            };
            let preview = preview_of(msg);
            let body = if preview.is_empty() {
                subject.to_string()
            } else {
                format!("{subject}\n{preview}")
            };
            Some(MailNotification {
                title: sender_label(&msg.sender, &msg.account),
                body,
                target,
            })
        }
    }
}

/// `From` 头的显示名；没有显示名时用地址；都没有时退回账户名。
pub fn sender_label(sender: &str, account: &str) -> String {
    let sender = sender.trim();
    let label = match sender.rfind('<') {
        Some(open) => {
            let name = sender[..open].trim().trim_matches('"').trim();
            if name.is_empty() {
                sender[open + 1..].trim_end_matches('>').trim()
            } else {
                name
            }
        }
        None => sender,
    };
    if label.is_empty() {
        account.to_string()
    } else {
        label.to_string()
    }
}

fn preview_of(msg: &MailMessage) -> String {
    let source = if msg.snippet.trim().is_empty() {
        msg.body.as_str()
    } else {
        msg.snippet.as_str()
    };
    let collapsed = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = collapsed.chars().take(PREVIEW_CHARS).collect();
    if collapsed.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_three_then_one_summary_per_window() {
        let mut limiter = BurstLimiter::default();
        let start = Instant::now();
        let outcomes: Vec<_> = (0..10)
            .map(|i| limiter.admit("a", start + Duration::from_millis(i * 100)))
            .collect();
        assert_eq!(&outcomes[..3], &[Admission::Single; 3]);
        assert_eq!(outcomes[3], Admission::Summary);
        assert!(outcomes[4..].iter().all(|o| *o == Admission::Suppressed));
        // 另一个账户独立计数。
        assert_eq!(limiter.admit("b", start), Admission::Single);
        // 窗口过去后恢复逐封通知。
        assert_eq!(
            limiter.admit("a", start + Duration::from_secs(30)),
            Admission::Single
        );
    }

    #[test]
    fn sender_label_prefers_display_name() {
        assert_eq!(sender_label("\"张三\" <z@s.cn>", "acc"), "张三");
        assert_eq!(sender_label("<z@s.cn>", "acc"), "z@s.cn");
        assert_eq!(sender_label("z@s.cn", "acc"), "z@s.cn");
        assert_eq!(sender_label("  ", "acc"), "acc");
    }

    #[test]
    fn eligible_blocks_stale_arrival_without_date_while_keeping_synthetic_timestamps() {
        let cfg = RulesConfig {
            notify_new_mail: true,
            ..RulesConfig::default()
        };
        let now = 1_795_000_000_u64;
        for synthetic in [0_u64, 1, 42] {
            let msg = MailMessage {
                account: "acc".into(),
                uid: "1".into(),
                received_at: synthetic,
                ..Default::default()
            };
            assert!(eligible(&cfg, &msg, now), "synthetic={synthetic}");
        }
        let stale_arrival = MailMessage {
            account: "acc".into(),
            uid: "2".into(),
            received_at: now - STALE_AFTER_SECS - 1,
            sort_at: now - STALE_AFTER_SECS - 1,
            ..Default::default()
        };
        assert!(!eligible(&cfg, &stale_arrival, now));
        let fresh_arrival = MailMessage {
            account: "acc".into(),
            uid: "3".into(),
            received_at: now - 60,
            sort_at: now - 60,
            ..Default::default()
        };
        assert!(eligible(&cfg, &fresh_arrival, now));
    }
}
