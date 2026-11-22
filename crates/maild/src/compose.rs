//! 发信侧的 MIME 组装与回复/转发成型（ADR-0028）。
//!
//! 这里是发信的领域层：头部编码、multipart 结构、引用正文与主题归一都在内核完成，
//! 前端拿到的是草稿、提交的是字段。换前端不该换掉 `References` 的正确性。
//!
//! 组装结果**一定是纯 ASCII**（决策 6）：非 ASCII 头部走 RFC 2047 encoded-word，
//! 非 ASCII 正文整体 base64。因此报文可以原样放进 TEXT 列，不需要二进制通道。

use crate::transport::b64_encode;

/// 单个头部行的软上限（RFC 5322 建议 78，硬限 998）。
const HEADER_LINE_SOFT_LIMIT: usize = 76;
/// 正文行长安全上限：超过就不能声明 `7bit`。
const SAFE_LINE_BYTES: usize = 900;
/// `References` 的字节上限。超限时保留首条与最后若干条。
pub const MAX_REFERENCES_BYTES: usize = 4096;
/// 一封信的收件人总数上限（to + cc + bcc）。
pub const MAX_RECIPIENTS: usize = 100;

// —— RFC 5322 日期 ——

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// unix 秒 → RFC 5322 日期，恒用 `+0000`。
///
/// 外发 `Date:` 固定输出 `+0000`：保护发件人物理时区隐私，并保证 Sent `APPEND`
/// 与持久写队列载荷确定一致（ADR-0028 决策 4）。
pub fn rfc5322_date(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let seconds_of_day = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 是星期四，WEEKDAYS 因此从 Thu 起算。
    let weekday = WEEKDAYS[(days.rem_euclid(7)) as usize];
    let month_name = MONTHS[(month - 1) as usize];
    format!(
        "{weekday}, {day} {month_name} {year} {:02}:{:02}:{:02} +0000",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

/// 回复/转发引用行使用的人读日期：在 `unix_secs` 合理且本地时间可拆解时，按 `quote`
/// 的语言成文（中文如 `2026年11月18日 周三 19:35（UTC+08:00）`）；否则回退 [`rfc5322_date`]。
///
/// `quote` 来自 `maild-l10n` 的协商结果（ADR-0048 决策 7）。
pub fn attribution_date(quote: &maild_l10n::Quote, unix_secs: u64) -> String {
    if unix_secs >= crate::mime::EARLIEST_PLAUSIBLE_DATE_SECS {
        if let Some(parts) = crate::localtime::local_parts(unix_secs) {
            return format_attribution_parts(quote, &parts);
        }
    }
    rfc5322_date(unix_secs)
}

pub fn format_attribution_parts(
    quote: &maild_l10n::Quote,
    parts: &crate::localtime::LocalDateTime,
) -> String {
    (quote.attribution_date)(&maild_l10n::AttributionDate {
        year: parts.year,
        month: parts.month,
        day: parts.day,
        weekday: parts.weekday,
        hour: parts.hour,
        minute: parts.minute,
        utc_offset_minutes: parts.utc_offset_minutes,
    })
}

/// Howard Hinnant 的 civil-from-days：把 1970-01-01 起的天数换成 (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 生成 `Message-ID`（不含尖括号）。
///
/// 本地知道自己发出去那封信的 ID，是把它和会话对上的前提（ADR-0028 决策 4）。
pub fn new_message_id(from: &str, unix_secs: u64) -> String {
    let domain = from.rsplit('@').next().filter(|value| !value.is_empty()).unwrap_or("localhost");
    let mut entropy = [0u8; 10];
    if getrandom::fill(&mut entropy).is_err() {
        // RNG 不可用时退回时间戳；ID 的唯一性下降但报文仍然合法。
        entropy[..8].copy_from_slice(&unix_secs.to_be_bytes());
    }
    format!("{}.{unix_secs}@{domain}", base32_lower(&entropy))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((accumulator >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((accumulator << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

// —— RFC 2047 encoded-word ——

fn needs_encoding(value: &str) -> bool {
    !value.is_ascii() || value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

/// 头部值 → 可安全放进报文的形式。
///
/// 纯 ASCII 且无控制字符时原样返回——不给英文邮件平白加一层编码。
/// 否则按 RFC 2047 编成 `=?UTF-8?B?...?=`，并在**字符边界**切分折行。
pub fn encode_header_value(value: &str) -> String {
    if !needs_encoding(value) {
        return value.to_string();
    }
    // 每个 encoded-word 的 base64 载荷上限：留出 `=?UTF-8?B?` 与 `?=` 的开销。
    const CHUNK_BYTES: usize = 39;
    let mut words: Vec<String> = Vec::new();
    let mut chunk = String::new();
    for ch in value.chars() {
        if chunk.len() + ch.len_utf8() > CHUNK_BYTES && !chunk.is_empty() {
            words.push(format!("=?UTF-8?B?{}?=", b64_encode(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", b64_encode(chunk.as_bytes())));
    }
    // 折行：continuation 行以空白开头（RFC 5322 §2.2.3）。
    words.join("\r\n ")
}

/// 组装一行头部，必要时对超长的地址列表折行。
fn header_line(name: &str, value: &str) -> String {
    if name.len() + 2 + value.len() <= HEADER_LINE_SOFT_LIMIT || !value.contains(", ") {
        return format!("{name}: {value}\r\n");
    }
    let mut out = format!("{name}:");
    let mut column = name.len() + 1;
    for (index, part) in value.split(", ").enumerate() {
        let piece = if index == 0 {
            part.to_string()
        } else {
            format!(", {part}")
        };
        if column + piece.len() > HEADER_LINE_SOFT_LIMIT {
            out.push_str("\r\n ");
            column = 1;
        }
        out.push(' ');
        out.push_str(piece.trim_start_matches(' '));
        column += piece.len() + 1;
    }
    out.push_str("\r\n");
    out
}

// —— 正文与附件 ——

/// 一个待发送的附件。字节由调用方从库里取出（ADR-0028 决策 8：本片不接受新文件上传）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    pub content: Vec<u8>,
}

/// 一封待组装的邮件。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutgoingMessage {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// **只进信封，绝不进头**（ADR-0028 决策 2）。
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// 可选 HTML 备选正文。非空时组装成 `multipart/alternative`。
    pub body_html: String,
    pub in_reply_to: String,
    pub references: String,
    pub attachments: Vec<OutgoingAttachment>,
    /// 由 [`new_message_id`] 生成，不含尖括号。
    pub message_id: String,
    pub date_unix: u64,
}

impl OutgoingMessage {
    /// 信封收件人：to + cc + bcc 的并集，保序去重。
    pub fn envelope_recipients(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for address in self.to.iter().chain(&self.cc).chain(&self.bcc) {
            if !out.iter().any(|existing| existing == address) {
                out.push(address.clone());
            }
        }
        out
    }
}

fn is_seven_bit_safe(body: &str) -> bool {
    body.is_ascii()
        && !body.contains('\0')
        && body.split('\n').all(|line| line.len() <= SAFE_LINE_BYTES)
}

/// 正文分段：ASCII 且行长安全走 `7bit`，否则整体 base64（ADR-0028 决策 6）。
fn body_part(content_type: &str, body: &str) -> String {
    if is_seven_bit_safe(body) {
        let normalized = body.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n");
        return format!(
            "Content-Type: {content_type}; charset=utf-8\r\n\
             Content-Transfer-Encoding: 7bit\r\n\r\n{normalized}\r\n"
        );
    }
    format!(
        "Content-Type: {content_type}; charset=utf-8\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        wrap_base64(&b64_encode(body.as_bytes()))
    )
}

// RFC 2231 parameters, including continuations. Encoded words (RFC 2047) are for headers,
// not MIME parameter values; percent encoding also prevents quotes from injecting parameters.
fn filename_parameter(key: &str, value: &str) -> String {
    if value.len() <= 60
        && value
            .bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\')
    {
        return format!("{key}=\"{value}\"");
    }
    value
        .as_bytes()
        .chunks(18)
        .enumerate()
        .map(|(index, chunk)| {
            let encoded: String = chunk.iter().map(|b| format!("%{b:02X}")).collect();
            let charset = if index == 0 { "utf-8''" } else { "" };
            format!("{key}*{index}*={charset}{encoded}")
        })
        .collect::<Vec<_>>()
        .join(";\r\n ")
}

fn attachment_part(attachment: &OutgoingAttachment) -> String {
    let name = filename_parameter("name", &attachment.filename);
    let filename = filename_parameter("filename", &attachment.filename);
    let mime_type = if attachment.mime_type.is_empty() {
        "application/octet-stream"
    } else {
        &attachment.mime_type
    };
    format!(
        "Content-Type: {mime_type}; {name}\r\n\
         Content-Disposition: attachment; {filename}\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        wrap_base64(&b64_encode(&attachment.content))
    )
}

/// base64 按 76 列折行（RFC 2045）。
fn wrap_base64(encoded: &str) -> String {
    encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// 边界串：只用 base32 字母表，绝不会与 base64 正文冲突。
fn boundary(seed: &str, tag: &str) -> String {
    format!("--=_maild_{tag}_{seed}")
}

/// 组装完整的 RFC 5322 报文（不含结尾的 SMTP `.`）。
pub fn assemble(message: &OutgoingMessage) -> String {
    let mut out = String::new();
    out.push_str(&header_line("Date", &rfc5322_date(message.date_unix)));
    out.push_str(&header_line("From", &message.from));
    if !message.to.is_empty() {
        out.push_str(&header_line("To", &message.to.join(", ")));
    }
    if !message.cc.is_empty() {
        out.push_str(&header_line("Cc", &message.cc.join(", ")));
    }
    // Bcc 不在这里，也不在任何地方——它只出现在 SMTP 信封里。
    if !message.subject.is_empty() {
        out.push_str(&header_line("Subject", &encode_header_value(&message.subject)));
    }
    if !message.message_id.is_empty() {
        out.push_str(&header_line("Message-ID", &format!("<{}>", message.message_id)));
    }
    if !message.in_reply_to.is_empty() {
        out.push_str(&header_line(
            "In-Reply-To",
            &format!("<{}>", message.in_reply_to),
        ));
    }
    if !message.references.is_empty() {
        out.push_str(&header_line("References", &message.references));
    }
    out.push_str("MIME-Version: 1.0\r\n");

    let seed = if message.message_id.is_empty() {
        "0"
    } else {
        message.message_id.split('.').next().unwrap_or("0")
    };
    let alternative = !message.body_html.is_empty();
    let mixed = !message.attachments.is_empty();

    let text_block = if alternative {
        let inner = boundary(seed, "alt");
        let mut block = format!(
            "Content-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n"
        );
        block.push_str(&format!("--{inner}\r\n"));
        block.push_str(&body_part("text/plain", &message.body));
        block.push_str(&format!("--{inner}\r\n"));
        block.push_str(&body_part("text/html", &message.body_html));
        block.push_str(&format!("--{inner}--\r\n"));
        block
    } else {
        body_part("text/plain", &message.body)
    };

    if !mixed {
        out.push_str(&text_block);
        return out;
    }

    let outer = boundary(seed, "mix");
    out.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{outer}\"\r\n\r\n"
    ));
    out.push_str(&format!("--{outer}\r\n"));
    out.push_str(&text_block);
    for attachment in &message.attachments {
        out.push_str(&format!("--{outer}\r\n"));
        out.push_str(&attachment_part(attachment));
    }
    out.push_str(&format!("--{outer}--\r\n"));
    out
}

// —— 回复 / 转发成型 ——

/// 草稿模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DraftMode {
    Reply,
    ReplyAll,
    Forward,
}

impl DraftMode {
    pub fn parse(value: &str) -> Option<DraftMode> {
        match value {
            "reply" => Some(Self::Reply),
            "reply_all" => Some(Self::ReplyAll),
            "forward" => Some(Self::Forward),
            _ => None,
        }
    }
}

/// 主题的回复/转发前缀。反复剥离后只加一个，避免 `Re: Re: Re:` 累积。
const SUBJECT_PREFIXES: &[&str] = &["re:", "答复:", "回复:", "fwd:", "fw:", "转发:"];

pub fn normalize_subject(subject: &str) -> String {
    let mut current = subject.trim();
    loop {
        let lowered = current.to_ascii_lowercase();
        let Some(prefix) = SUBJECT_PREFIXES
            .iter()
            .find(|prefix| lowered.starts_with(*prefix))
        else {
            break;
        };
        current = current[prefix.len()..].trim_start();
    }
    current.to_string()
}

pub fn reply_subject(subject: &str) -> String {
    format!("Re: {}", normalize_subject(subject))
}

pub fn forward_subject(subject: &str) -> String {
    format!("Fwd: {}", normalize_subject(subject))
}

/// 继承 `References`：原信的 References 追加原信的 Message-ID。
///
/// 超限时保留**首条与最后若干条**——首条是会话根，丢了整条会话就断开，
/// 这比丢中间几条严重得多（ADR-0028 决策 7）。
pub fn inherited_references(references: &str, in_reply_to: &str, message_id: &str) -> String {
    let base = if references.trim().is_empty() {
        in_reply_to.trim()
    } else {
        references.trim()
    };
    let mut ids: Vec<String> = base
        .split_whitespace()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if !message_id.is_empty() {
        let formatted = format!("<{message_id}>");
        if !ids.contains(&formatted) {
            ids.push(formatted);
        }
    }
    let joined = ids.join(" ");
    if joined.len() <= MAX_REFERENCES_BYTES {
        return joined;
    }
    let root = ids.first().cloned().unwrap_or_default();
    let mut tail: Vec<String> = Vec::new();
    let mut budget = MAX_REFERENCES_BYTES.saturating_sub(root.len() + 1);
    for id in ids.iter().skip(1).rev() {
        if id.len() + 1 > budget {
            break;
        }
        budget -= id.len() + 1;
        tail.push(id.clone());
    }
    tail.reverse();
    let mut kept = vec![root];
    kept.extend(tail);
    kept.join(" ")
}

/// 回复引用块。引用行的语言由 `quote` 决定（ADR-0048 决策 7：`prepareReply` 的 `locale`
/// 或 `effective` 协商）；`> ` 前缀与前后空行与语言无关。
pub fn quote_reply(quote: &maild_l10n::Quote, sender: &str, date: &str, body: &str) -> String {
    let quoted: String = body
        .lines()
        .map(|line| if line.is_empty() { ">".to_string() } else { format!("> {line}") })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n\n{}\n{quoted}", (quote.reply_attribution)(date, sender))
}

/// 转发头 + 原文正文。转发头的语言由 `quote` 决定，同 [`quote_reply`]。
pub fn quote_forward(
    quote: &maild_l10n::Quote,
    sender: &str,
    date: &str,
    subject: &str,
    recipient: &str,
    body: &str,
) -> String {
    format!(
        "\n\n{}\n\n{body}",
        (quote.forward_header)(sender, date, subject, recipient)
    )
}

/// 清洗 `Message-ID` 类输入：剥尖括号、去掉空白与控制字符，有界。
/// 头部值来自客户端，不能让它带着 CRLF 走进报文。
pub fn sanitize_message_id(value: &str) -> String {
    let trimmed = value.trim().trim_start_matches('<').trim_end_matches('>');
    trimmed
        .chars()
        .filter(|ch| !ch.is_control() && !ch.is_whitespace() && *ch != '<' && *ch != '>')
        .take(400)
        .collect()
}

/// 清洗 `References`：逐项按 `Message-ID` 规则清洗后用单空格重组，并施加长度上限。
pub fn sanitize_references(value: &str) -> String {
    let ids: Vec<String> = value
        .split_whitespace()
        .map(sanitize_message_id)
        .filter(|id| !id.is_empty())
        .map(|id| format!("<{id}>"))
        .collect();
    let joined = ids.join(" ");
    if joined.len() <= MAX_REFERENCES_BYTES {
        return joined;
    }
    inherited_references(&joined, "", "")
}

/// 从一个头部值里抽出 addr-spec 列表（去掉显示名与尖括号）。
///
/// 只做发信需要的最小解析：逐项按逗号切，取尖括号内或整段，过滤掉不含 `@` 的碎片。
pub fn extract_addresses(header: &str) -> Vec<String> {
    header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let candidate = match (part.rfind('<'), part.rfind('>')) {
                (Some(start), Some(end)) if end > start + 1 => &part[start + 1..end],
                _ => part,
            };
            let candidate = candidate.trim();
            (candidate.contains('@') && !candidate.contains(' ')).then(|| candidate.to_string())
        })
        .collect()
}

/// 去掉「自己」并保序去重。全部回复不该把信抄送给自己。
pub fn without_self(addresses: Vec<String>, mine: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for address in addresses {
        if mine
            .iter()
            .any(|own| own.eq_ignore_ascii_case(&address))
        {
            continue;
        }
        if !out.iter().any(|existing| existing.eq_ignore_ascii_case(&address)) {
            out.push(address);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5322_dates_cover_leap_years_and_century_boundaries() {
        assert_eq!(rfc5322_date(0), "Thu, 1 Jan 1970 00:00:00 +0000");
        // 2000-02-29：能被 400 整除的世纪年是闰年。
        assert_eq!(rfc5322_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
        // 2100-03-01：2100 不是闰年，2 月只有 28 天。
        assert_eq!(rfc5322_date(4_107_542_400), "Mon, 1 Mar 2100 00:00:00 +0000");
        assert_eq!(rfc5322_date(1_646_128_800), "Tue, 1 Mar 2022 10:00:00 +0000");
    }

    #[test]
    fn attribution_date_formats_local_offsets_and_falls_back_for_synthetic_epochs() {
        use crate::localtime::LocalDateTime;
        let zh = &maild_l10n::ZH_HANS.quote;

        assert_eq!(
            format_attribution_parts(zh, &LocalDateTime {
                year: 2026,
                month: 11,
                day: 18,
                weekday: 3,
                hour: 19,
                minute: 35,
                second: 0,
                utc_offset_minutes: 480,
            }),
            "2026年11月18日 周三 19:35（UTC+08:00）"
        );
        assert_eq!(
            format_attribution_parts(zh, &LocalDateTime {
                year: 2026,
                month: 11,
                day: 18,
                weekday: 0,
                hour: 7,
                minute: 5,
                second: 0,
                utc_offset_minutes: -360,
            }),
            "2026年11月18日 周日 07:05（UTC-06:00）"
        );
        assert_eq!(
            format_attribution_parts(zh, &LocalDateTime {
                year: 2026,
                month: 3,
                day: 1,
                weekday: 6,
                hour: 0,
                minute: 0,
                second: 0,
                utc_offset_minutes: 330,
            }),
            "2026年3月1日 周六 00:00（UTC+05:30）"
        );
        assert_eq!(
            format_attribution_parts(zh, &LocalDateTime {
                year: 2026,
                month: 1,
                day: 2,
                weekday: 5,
                hour: 12,
                minute: 30,
                second: 0,
                utc_offset_minutes: 0,
            }),
            "2026年1月2日 周五 12:30（UTC+00:00）"
        );
        // Synthetic timestamps < 1971 fall back to RFC 5322 UTC.
        assert_eq!(attribution_date(zh, 0), "Thu, 1 Jan 1970 00:00:00 +0000");
        assert_eq!(attribution_date(zh, 1234), rfc5322_date(1234));
        // 回退的 RFC 5322 日期不是文案，与语言无关。
        let en = &maild_l10n::EN.quote;
        assert_eq!(attribution_date(en, 1234), rfc5322_date(1234));
        assert_eq!(
            format_attribution_parts(en, &LocalDateTime {
                year: 2026,
                month: 11,
                day: 18,
                weekday: 3,
                hour: 19,
                minute: 35,
                second: 0,
                utc_offset_minutes: 480,
            }),
            "Wed, Nov 18, 2026 at 19:35 (UTC+08:00)"
        );
    }

    /// ADR-0048 决策 7：中文引用块与引入 `maild-l10n` 之前逐字节一致；英文走同一拼接骨架。
    #[test]
    fn quote_blocks_follow_the_negotiated_catalog() {
        let zh = &maild_l10n::ZH_HANS.quote;
        let en = &maild_l10n::EN.quote;
        let date = "2026年11月18日 周三 19:35（UTC+08:00）";
        assert_eq!(
            quote_reply(zh, "Alice <a@x.test>", date, "第一行\n\n第二行"),
            "\n\n在 2026年11月18日 周三 19:35（UTC+08:00），Alice <a@x.test> 写道：\n\
             > 第一行\n>\n> 第二行"
        );
        assert_eq!(
            quote_forward(zh, "Alice <a@x.test>", date, "周会", "me@x.test", "正文"),
            "\n\n---------- 转发的邮件 ----------\n\
             发件人: Alice <a@x.test>\n日期: 2026年11月18日 周三 19:35（UTC+08:00）\n\
             主题: 周会\n收件人: me@x.test\n\n正文"
        );
        assert_eq!(
            quote_reply(en, "Alice <a@x.test>", "D", "hi"),
            "\n\nOn D, Alice <a@x.test> wrote:\n> hi"
        );
        assert_eq!(
            quote_forward(en, "Alice <a@x.test>", "D", "Weekly", "me@x.test", "body"),
            "\n\n---------- Forwarded message ----------\n\
             From: Alice <a@x.test>\nDate: D\nSubject: Weekly\nTo: me@x.test\n\nbody"
        );
    }

    #[test]
    fn message_ids_are_unique_and_carry_the_sender_domain() {
        let first = new_message_id("me@example.test", 1_700_000_000);
        let second = new_message_id("me@example.test", 1_700_000_000);
        assert_ne!(first, second);
        assert!(first.ends_with("@example.test"), "{first}");
        assert!(!first.contains('<') && !first.contains('>'));
    }

    #[test]
    fn ascii_headers_are_not_needlessly_encoded() {
        assert_eq!(encode_header_value("Hello world"), "Hello world");
        assert!(encode_header_value("周会纪要").starts_with("=?UTF-8?B?"));
    }

    #[test]
    fn subject_prefixes_never_accumulate() {
        assert_eq!(reply_subject("Re: Re: 回复: 周会"), "Re: 周会");
        assert_eq!(forward_subject("Fwd: FW: 转发: 周会"), "Fwd: 周会");
        assert_eq!(normalize_subject("  Re:   周会  "), "周会");
        assert_eq!(reply_subject("Reply guide"), "Re: Reply guide");
    }

    #[test]
    fn references_keep_the_thread_root_when_truncated() {
        let root = "<root@example.test>";
        let mut long = vec![root.to_string()];
        for index in 0..500 {
            long.push(format!("<msg{index:04}@example.test>"));
        }
        let inherited = inherited_references(&long.join(" "), "", "newest@example.test");
        assert!(inherited.len() <= MAX_REFERENCES_BYTES);
        assert!(inherited.starts_with(root), "会话根必须保留: {inherited}");
        assert!(inherited.ends_with("<newest@example.test>"));
    }

    #[test]
    fn references_fall_back_to_in_reply_to() {
        assert_eq!(
            inherited_references("", "<parent@example.test>", "child@example.test"),
            "<parent@example.test> <child@example.test>"
        );
    }

    #[test]
    fn addresses_are_extracted_from_display_name_forms() {
        assert_eq!(
            extract_addresses("Alice <a@example.test>, b@example.test, 乱码"),
            vec!["a@example.test".to_string(), "b@example.test".to_string()]
        );
    }

    #[test]
    fn reply_all_drops_our_own_addresses_case_insensitively() {
        let kept = without_self(
            vec![
                "Me@Example.test".into(),
                "alias@example.test".into(),
                "other@example.test".into(),
                "other@example.test".into(),
            ],
            &["me@example.test".into(), "ALIAS@example.test".into()],
        );
        assert_eq!(kept, vec!["other@example.test".to_string()]);
    }
}
