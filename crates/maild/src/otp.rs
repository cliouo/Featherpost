//! otp：验证码实时收。
//!
//! 监听 `MailFiltered` → 查 store 是否已通知（持久化去重，重启有效）→ 正则提取
//! （先正文后主题）→ 标记 store → 发 `OtpDetected` + 系统通知。
//!
//! 去重依赖 store 的 `otp_notifications` 表（(account, uid) 主键）：
//! 多个 otp 消费者 / 重启重放都只通知一次。
//!
//! ADR-0043：默认 pattern（`\d{6}`）不再按裸正则匹配，而是走上下文感知的
//! [`extract_smart_otp`]——必须先出现验证码语境关键词，再取前后不紧邻字母数字的 4～8 位
//! 独立数字。用户显式自定义 pattern 时仍按其正则原样匹配。[`extract_code`] 是纯正则契约，
//! 行为不变。

use std::any::TypeId;
use std::time::{SystemTime, UNIX_EPOCH};

use cordis::{Context, Plugin, PluginError, Service};
use regex::Regex;
use serde::Deserialize;

use crate::events::{
    MailFiltered, MailMessage, NotifyRoute, NotifyRouteEvent, OtpDetected, OtpHit, QuietHoursQuery,
    QuietHoursRequest,
};
use crate::frontend::OpenIntent;
use crate::notify::Notifier;
use crate::rules::{RulesConfig, RulesSettings};
use crate::store::Store;

const MAX_OTP_CODE_BYTES: usize = 128;
/// 验证码通知的新鲜度窗口（1 小时）：容忍延迟投递，同时拦截冷启动或历史同步带回的过期验证码。
pub const OTP_STALE_AFTER_SECS: u64 = 60 * 60;

/// 判定一封邮件是否仍有资格触发实时验证码通知（跳过已读、已删与超过 [`OTP_STALE_AFTER_SECS`] 的历史信）。
pub fn otp_eligible(msg: &MailMessage, now_unix_secs: u64) -> bool {
    if msg.seen || msg.deleted {
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
    !effective_ts.is_some_and(|ts| ts.saturating_add(OTP_STALE_AFTER_SECS) < now_unix_secs)
}

#[derive(Clone, Debug, Deserialize)]
pub struct OtpConfig {
    /// 提取正则（默认 6 位数字，首字符即验证码）。
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// 用户自定义的通知标题。缺省（`None`）= 用当前语言的内核文案 `otp_title`（ADR-0048 决策 6），
    /// 见 [`OtpConfig::custom_title`]。
    #[serde(default)]
    pub title: Option<String>,
}

/// 出厂默认 pattern。等于它时 [`OtpPlugin`] 改用上下文感知的 [`extract_smart_otp`]。
pub const DEFAULT_PATTERN: &str = r"\d{6}";

fn default_pattern() -> String {
    DEFAULT_PATTERN.into()
}

/// 本地化之前的出厂标题。旧版启动器首启模板与旧配置升级会把它写进 `config.toml`，
/// 所以配置里恰好等于它时视为「未自定义」，同样跟随界面语言。
pub const LEGACY_DEFAULT_TITLE: &str = "验证码";

impl Default for OtpConfig {
    /// 与 serde 默认值一致（防 `Default`/反序列化两条路径漂移）。
    fn default() -> Self {
        OtpConfig {
            pattern: default_pattern(),
            title: None,
        }
    }
}

impl OtpConfig {
    /// 用户真正自定义过的标题；`None` 表示用本地化的 `otp_title`。
    ///
    /// 未配置，或配置值恰好等于 [`LEGACY_DEFAULT_TITLE`]（旧版写入的出厂值）都算未自定义；
    /// 其余用户写的标题原样使用。
    pub fn custom_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .filter(|title| *title != LEGACY_DEFAULT_TITLE)
    }

    /// 按 `messages` 成文的通知标题与正文（纯函数，不读偏好）。
    pub fn notification_text(
        &self,
        messages: &maild_l10n::Messages,
        hit: &OtpHit,
        account_label: Option<&str>,
    ) -> (String, String) {
        let notifications = &messages.notifications;
        let title = self
            .custom_title()
            .unwrap_or(notifications.otp_title)
            .to_string();
        let body = (notifications.otp_body)(&hit.code, account_label, &hit.from);
        (title, body)
    }

    /// 是否仍是出厂默认 pattern（→ 走 [`extract_smart_otp`]，ADR-0043）。
    pub fn uses_smart_default(&self) -> bool {
        self.pattern == DEFAULT_PATTERN
    }

    /// 按本配置的语义检测一封信是否会被识别为验证码（不触碰 store、不发通知）。
    ///
    /// 普适新邮件通知用它避免与 OTP 专属通知重复；pattern 非法时按「不是验证码」处理。
    pub fn detect(&self, body: &str, subject: &str) -> Option<String> {
        if self.uses_smart_default() {
            extract_smart_otp(body, subject)
        } else if self.pattern.trim().is_empty() {
            None
        } else {
            extract_code(&self.pattern, body, subject)
        }
    }
}

impl Service for OtpConfig {
    fn name(&self) -> &'static str {
        "otp.config"
    }
}

/// otp 插件：inject OtpConfig + Notifier + Store → 订阅 MailFiltered。
pub struct OtpPlugin;

impl Plugin for OtpPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![
            TypeId::of::<OtpConfig>(),
            TypeId::of::<Notifier>(),
            TypeId::of::<Store>(),
        ]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<OtpConfig>().expect("inject 就绪");
        if cfg.pattern.trim().is_empty() {
            return Err("otp pattern 为空".into());
        }
        let re = Regex::new(&cfg.pattern).map_err(|e| format!("otp pattern 无效: {e}"))?;
        let smart = cfg.uses_smart_default();

        let ctx2 = ctx.clone();
        ctx.on_emit_scoped::<MailFiltered>(move |msg| {
            let at_unix_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            if !otp_eligible(&msg, at_unix_secs) {
                return;
            }
            // 持久化去重：先查（emit 同步，「查→提取→标记」在单个监听器内无竞态）
            let store = match ctx2.get::<Store>() {
                Some(s) => s,
                None => return,
            };
            let already = store
                .is_notified(&msg.account, &msg.uid)
                .unwrap_or_else(|e| {
                    eprintln!("[maild/otp] 查询通知标记失败: {e}");
                    true // 查失败时宁可不通知
                });
            if already {
                return;
            }
            // 验证码通常在正文，其次主题
            let code = if smart {
                extract_smart_otp(&msg.body, &msg.subject)
            } else {
                let haystack = format!("{}\n{}", msg.body, msg.subject);
                re.find(&haystack).map(|m| m.as_str().to_string())
            };
            let Some(code) = code else {
                return;
            };
            if let Err(e) = store.mark_notified(&msg.account, &msg.uid) {
                eprintln!("[maild/otp] 标记通知失败: {e}");
                return;
            }
            if code.len() > MAX_OTP_CODE_BYTES {
                eprintln!("[maild/otp] 验证码匹配超过安全上限，通知已丢弃");
                return;
            }
            let hit = OtpHit {
                code,
                from: msg.sender.clone(),
                subject: msg.subject.clone(),
                account: msg.account.clone(),
                uid: msg.uid.clone(),
            };
            ctx2.emit::<OtpDetected>(hit.clone());
            // notify/route bail：rules 等策略可降级为 silent（静默时段/敏感开关）
            let mut default = NotifyRoute {
                channel: "system".into(),
                sensitive: true,
                subject: hit.subject.clone(),
                from: hit.from.clone(),
            };
            let quiet = ctx2
                .bail::<QuietHoursQuery>(QuietHoursRequest {
                    account: hit.account.clone(),
                    at_unix_secs,
                })
                .unwrap_or(false);
            if quiet {
                default.channel = "silent".into();
            }
            let final_route = ctx2
                .bail::<NotifyRouteEvent>(default.clone())
                .unwrap_or(default);
            if final_route.channel == "silent" {
                eprintln!("[maild/otp] 路由策略已静默系统通知");
                return;
            }
            // 用户配置了 rules 却没有成功挂载策略服务时必须 fail-closed，不能因配置
            // 或 scheduler 错误反而显示敏感验证码。
            if ctx2.get::<RulesConfig>().is_some() && ctx2.get::<RulesSettings>().is_none() {
                eprintln!("[maild/otp] rules 服务不可用，系统通知已静默");
                return;
            }
            if let Some(n) = ctx2.get::<Notifier>() {
                // 成文时读偏好：切换语言后下一条验证码通知即跟随（ADR-0048 决策 6）。
                let label = crate::accounts::notification_label(&ctx2, &hit.account);
                let (title, body) = cfg.notification_text(
                    crate::notify::kernel_messages(&ctx2),
                    &hit,
                    label.as_deref(),
                );
                n.notify_with_target(
                    &title,
                    &body,
                    Some(OpenIntent::Message {
                        folder: (!msg.folder.is_empty()).then(|| msg.folder.clone()),
                        account: hit.account.clone(),
                        uid: hit.uid.clone(),
                    }),
                );
            }
        });
        Ok(())
    }
}

/// 纯函数提取（单元测试用）：返回首个匹配。
pub fn extract_code(pattern: &str, body: &str, subject: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.find(&format!("{body}\n{subject}"))
        .map(|m| m.as_str().to_string())
}

// —— ADR-0043：上下文感知的验证码识别 ——

/// 中文验证码语境关键词（子串匹配）。
const OTP_KEYWORDS_ZH: &[&str] = &[
    "验证码",
    "校验码",
    "动态码",
    "确认码",
    "安全码",
    "认证码",
    "登录码",
    "提取码",
    "授权码",
    "激活码",
    "一次性密码",
    "口令",
];

/// 英文关键词（ASCII 大小写不敏感）与是否要求右侧词边界。
/// 左侧一律要求词边界：`hotpot` 不能因为含 `otp` 被当作验证码语境。
const OTP_KEYWORDS_EN: &[(&str, bool)] = &[
    ("verification", false),
    ("verify", false),
    ("security code", false),
    ("confirmation code", false),
    ("login code", false),
    ("auth code", false),
    ("one-time", false),
    ("passcode", false),
    ("otp", true),
    ("pin code", false),
    ("2fa", true),
];

const MIN_CODE_DIGITS: usize = 4;
const MAX_CODE_DIGITS: usize = 8;
/// 关键词附近窗口（字符数）：窗口内的候选按距离打分。
const NEAR_WINDOW_CHARS: usize = 64;
/// 非 6 位候选的距离惩罚：同等距离下优先 6 位，但不让远处的 6 位电话片段压过紧邻的 4 位码。
const NON_SIX_PENALTY: usize = 8;
/// 候选出现在关键词之前的距离惩罚（模板通常是「验证码：123456」）。
const BEFORE_KEYWORD_PENALTY: usize = 4;

#[derive(Debug)]
struct Candidate {
    /// 起止字符序号（[start, end)）。
    start: usize,
    end: usize,
    digits: String,
}

/// 上下文感知的验证码提取（ADR-0043）。
///
/// 1. 正文或主题必须含验证码语境关键词（中文子串；英文 ASCII 大小写不敏感、带词边界），
///    否则直接 `None`——账单、物流、通知类邮件里的日期、金额、电话号码不再被误报；
/// 2. 候选是**前后都不紧邻 ASCII 字母数字**的 4～8 位纯数字串：`20261117`、
///    `13800138000`、`202611170001`、URL/十六进制串中间截不出 6 位；
/// 3. 排除明显的非验证码数字：日期/时间/小数/分段电话（两侧以 `-` `/` `.` `:` `,`
///    连着数字）、`1990..=2099` 后接 `年`/`-`/`/`、8 位紧凑日期 `YYYYMMDD`、
///    金额（前接 `¥` `￥` `$`，或后接 `元`，中间可隔空白）；
/// 4. 关键词附近（64 字符内）的候选按「距离 + 非 6 位惩罚 + 位于关键词之前惩罚」取最小；
///    附近没有候选时退回首个 6 位候选，再不行仅在全文只有唯一候选时采用它。
///
/// 正文在前、主题在后拼接，与 [`extract_code`] 的扫描顺序一致。
pub fn extract_smart_otp(body: &str, subject: &str) -> Option<String> {
    let text: Vec<char> = format!("{body}\n{subject}").chars().collect();
    let lowered: Vec<char> = text.iter().map(|ch| ch.to_ascii_lowercase()).collect();
    let keywords = keyword_spans(&lowered);
    if keywords.is_empty() {
        return None;
    }
    let candidates = candidates(&text);
    if candidates.is_empty() {
        return None;
    }
    let near = candidates
        .iter()
        .filter_map(|candidate| {
            let (distance, before) = keywords
                .iter()
                .map(|&(start, end)| span_distance(candidate, start, end))
                .min()?;
            (distance <= NEAR_WINDOW_CHARS).then(|| {
                let mut score = distance;
                if candidate.digits.len() != 6 {
                    score += NON_SIX_PENALTY;
                }
                if before {
                    score += BEFORE_KEYWORD_PENALTY;
                }
                (score, candidate.start, candidate)
            })
        })
        .min_by_key(|(score, start, _)| (*score, *start));
    if let Some((_, _, candidate)) = near {
        return Some(candidate.digits.clone());
    }
    if let Some(six) = candidates
        .iter()
        .find(|candidate| candidate.digits.len() == 6)
    {
        return Some(six.digits.clone());
    }
    (candidates.len() == 1).then(|| candidates[0].digits.clone())
}

fn looks_like_compact_date(digits: &str) -> bool {
    let part = |range: std::ops::Range<usize>| digits[range].parse::<u16>().unwrap_or(0);
    (1990..=2099).contains(&part(0..4))
        && (1..=12).contains(&part(4..6))
        && (1..=31).contains(&part(6..8))
}

/// 候选与关键词 span 的字符距离，以及候选是否位于关键词之前。
fn span_distance(candidate: &Candidate, start: usize, end: usize) -> (usize, bool) {
    if candidate.end <= start {
        (start - candidate.end, true)
    } else if candidate.start >= end {
        (candidate.start - end, false)
    } else {
        (0, false)
    }
}

fn keyword_spans(lowered: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for keyword in OTP_KEYWORDS_ZH {
        let needle: Vec<char> = keyword.chars().collect();
        for start in find_all(lowered, &needle) {
            spans.push((start, start + needle.len()));
        }
    }
    for &(keyword, right_boundary) in OTP_KEYWORDS_EN {
        let needle: Vec<char> = keyword.chars().collect();
        for start in find_all(lowered, &needle) {
            let end = start + needle.len();
            let left_ok = start == 0 || !lowered[start - 1].is_ascii_alphanumeric();
            let right_ok =
                !right_boundary || end == lowered.len() || !lowered[end].is_ascii_alphanumeric();
            if left_ok && right_ok {
                spans.push((start, end));
            }
        }
    }
    spans
}

fn find_all(haystack: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    (0..=haystack.len() - needle.len())
        .filter(|&start| haystack[start..start + needle.len()] == *needle)
        .collect()
}

fn candidates(text: &[char]) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < text.len() {
        if !text[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < text.len() && text[index].is_ascii_digit() {
            index += 1;
        }
        let end = index;
        let len = end - start;
        if !(MIN_CODE_DIGITS..=MAX_CODE_DIGITS).contains(&len) {
            continue;
        }
        let prev = start.checked_sub(1).map(|i| text[i]);
        let next = text.get(end).copied();
        // 前后紧邻 ASCII 字母：属于更长的标识符/十六进制串/URL 片段。
        if prev.is_some_and(|ch| ch.is_ascii_alphanumeric())
            || next.is_some_and(|ch| ch.is_ascii_alphanumeric())
        {
            continue;
        }
        // 日期、时间、小数、千分位、分段电话：分隔符另一侧还是数字。
        let joined = |ch: Option<char>, beyond: Option<char>| {
            matches!(ch, Some('-' | '/' | '.' | ':' | ','))
                && beyond.is_some_and(|ch| ch.is_ascii_digit())
        };
        let before_prev = start.checked_sub(2).map(|i| text[i]);
        let after_next = text.get(end + 1).copied();
        if joined(prev, before_prev) || joined(next, after_next) {
            continue;
        }
        let digits: String = text[start..end].iter().collect();
        let next_inline_solid = text[end..]
            .iter()
            .take_while(|ch| !matches!(ch, '\n' | '\r'))
            .find(|ch| !ch.is_whitespace())
            .copied();
        // 年份：1990..=2099 后接 `年`（允许同行空格如 `2026 年`）/ `-` / `/`，
        // 或同行前约 20 字符内有英文月份名/缩写、`©`、`copyright`。
        if len == 4
            && digits
                .parse::<u16>()
                .is_ok_and(|year| (1990..=2099).contains(&year))
            && (matches!(next, Some('-' | '/'))
                || next_inline_solid == Some('年')
                || preceded_by_western_date_or_copyright(text, start))
        {
            continue;
        }
        // 紧凑日期 YYYYMMDD（1990..=2099 年、合法月日）。
        if len == 8 && looks_like_compact_date(&digits) {
            continue;
        }
        // 金额（允许同行隔空白：`100000 元`、`¥ 1999`、`€ 1234`、`£1234`，不跨行误伤如 `\n元旦`）。
        let prev_inline_solid = text[..start]
            .iter()
            .rev()
            .take_while(|ch| !matches!(ch, '\n' | '\r'))
            .find(|ch| !ch.is_whitespace());
        if matches!(prev_inline_solid, Some('¥' | '￥' | '$' | '€' | '£'))
            || next_inline_solid == Some('元')
        {
            continue;
        }
        // URL / 查询参数里的数字（仅在连续 ASCII URL token 内识别，避免中文无空格标点误杀）。
        if inside_url_token(text, start, end) {
            continue;
        }
        found.push(Candidate { start, end, digits });
    }
    found
}

fn inside_url_token(text: &[char], start: usize, end: usize) -> bool {
    let is_url_char = |ch: char| {
        ch.is_ascii_graphic()
            && !matches!(ch, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']')
    };
    let mut token_start = start;
    while token_start > 0 && is_url_char(text[token_start - 1]) {
        token_start -= 1;
    }
    let token: String = text[token_start..end]
        .iter()
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    token.contains("://") || token.starts_with("www.")
}

const WESTERN_YEAR_PREFIX_WORDS: &[&str] = &[
    "jan",
    "january",
    "feb",
    "february",
    "mar",
    "march",
    "apr",
    "april",
    "may",
    "jun",
    "june",
    "jul",
    "july",
    "aug",
    "august",
    "sep",
    "sept",
    "september",
    "oct",
    "october",
    "nov",
    "november",
    "dec",
    "december",
    "copyright",
];

/// 检查 `start` 处的 4 位年份在同行前约 20 个字符内是否紧跟英文月份名/缩写、`©` 或 `copyright`。
fn preceded_by_western_date_or_copyright(text: &[char], start: usize) -> bool {
    const WINDOW_CHARS: usize = 20;
    let line_start = text[..start]
        .iter()
        .rposition(|ch| matches!(ch, '\n' | '\r'))
        .map(|pos| pos + 1)
        .unwrap_or(0);
    let window_start = start.saturating_sub(WINDOW_CHARS).max(line_start);
    if text[window_start..start].contains(&'©') {
        return true;
    }
    let mut cursor = line_start;
    while cursor < start {
        if !text[cursor].is_ascii_alphabetic() {
            cursor += 1;
            continue;
        }
        let word_start = cursor;
        while cursor < start && text[cursor].is_ascii_alphabetic() {
            cursor += 1;
        }
        let word_end = cursor;
        if start - word_start <= WINDOW_CHARS {
            let left_ok = word_start == 0 || !text[word_start - 1].is_ascii_alphanumeric();
            let right_ok = word_end == text.len() || !text[word_end].is_ascii_alphanumeric();
            if left_ok && right_ok {
                let word: String = text[word_start..word_end]
                    .iter()
                    .map(|ch| ch.to_ascii_lowercase())
                    .collect();
                if WESTERN_YEAR_PREFIX_WORDS.contains(&word.as_str()) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit() -> OtpHit {
        OtpHit {
            code: "246810".into(),
            from: "noreply@example.com".into(),
            subject: "登录验证码".into(),
            account: "personal".into(),
            uid: "9".into(),
        }
    }

    fn config(toml_text: &str) -> OtpConfig {
        toml::from_str(toml_text).expect("合法 otp 配置")
    }

    #[test]
    fn notification_text_follows_locale_when_title_is_not_customised() {
        // 未配置 title，以及旧版写进 config.toml 的出厂值「验证码」，都跟随界面语言。
        for cfg in [
            OtpConfig::default(),
            config(""),
            config("title = \"验证码\"\n"),
        ] {
            assert_eq!(cfg.custom_title(), None, "{:?}", cfg.title);
            assert_eq!(
                cfg.notification_text(&maild_l10n::ZH_HANS, &hit(), Some("personal")),
                (
                    "验证码".to_string(),
                    "246810（personal · 来自 noreply@example.com）".to_string()
                )
            );
            assert_eq!(
                cfg.notification_text(&maild_l10n::EN, &hit(), Some("personal")),
                (
                    maild_l10n::EN.notifications.otp_title.to_string(),
                    "246810 (personal · from noreply@example.com)".to_string()
                )
            );
        }
    }

    #[test]
    fn user_written_otp_title_is_kept_verbatim_in_every_locale() {
        let cfg = config("title = \"Login code\"\n");
        assert_eq!(cfg.custom_title(), Some("Login code"));
        for messages in [&maild_l10n::ZH_HANS, &maild_l10n::EN] {
            assert_eq!(
                cfg.notification_text(messages, &hit(), None).0,
                "Login code"
            );
        }
        // 只有逐字节等于旧出厂值才算未自定义。
        assert_eq!(
            config("title = \" 验证码\"\n").custom_title(),
            Some(" 验证码")
        );
    }

    #[test]
    fn excludes_western_dates_copyright_and_euro_pound_amounts_but_keeps_mmddyyyy() {
        assert_eq!(
            extract_smart_otp("Sent on Nov 18, 2026. Your OTP is 849201.", ""),
            Some("849201".into())
        );
        assert_eq!(
            extract_smart_otp("Verification notice — Nov 18, 2026", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Verification notice — 18 November 2026", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Verification statement for Nov 2026", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Your verification receipt\n© 2026 Acme Corp", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Your verification receipt\nCopyright 2026 Acme Corp", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Verification payment €123456 confirmed", ""),
            None
        );
        assert_eq!(
            extract_smart_otp("Verification payment £ 4821 confirmed", ""),
            None
        );
        // MMDDYYYY / DDMMYYYY must NOT be excluded.
        assert_eq!(
            extract_smart_otp("Your verification code is 11182026", ""),
            Some("11182026".into())
        );
        assert_eq!(
            extract_smart_otp("Your verification code is 18112026", ""),
            Some("18112026".into())
        );
    }

    #[test]
    fn otp_eligibility_skips_seen_deleted_and_stale_messages() {
        let now = 1_795_000_000_u64;
        let fresh = MailMessage {
            account: "a".into(),
            uid: "1".into(),
            date_at: Some(now - 300),
            received_at: now,
            sort_at: now - 300,
            ..Default::default()
        };
        assert!(otp_eligible(&fresh, now));
        assert!(!otp_eligible(
            &MailMessage {
                seen: true,
                ..fresh.clone()
            },
            now
        ));
        assert!(!otp_eligible(
            &MailMessage {
                deleted: true,
                ..fresh.clone()
            },
            now
        ));
        assert!(!otp_eligible(
            &MailMessage {
                date_at: Some(now - OTP_STALE_AFTER_SECS - 1),
                ..fresh.clone()
            },
            now
        ));
        assert!(!otp_eligible(
            &MailMessage {
                date_at: None,
                sort_at: now - OTP_STALE_AFTER_SECS - 1,
                received_at: now - OTP_STALE_AFTER_SECS - 1,
                ..fresh
            },
            now
        ));
        // Synthetic unit-test timestamps (< EARLIEST_PLAUSIBLE_DATE_SECS) stay eligible.
        assert!(otp_eligible(
            &MailMessage {
                received_at: 42,
                ..Default::default()
            },
            now
        ));
    }
}
