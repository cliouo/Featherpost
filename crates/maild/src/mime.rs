//! MIME 解析辅助（ADR-0025）：HTML → 纯文本、正文/附件分离、头部与时间钳制。
//!
//! 这里刻意**不引入 HTML 解析依赖**。本模块要的是「取纯文本」，不是构建 DOM；
//! 真正需要 DOM 语义的是正文渲染，由系统 webview 承担（ADR-0024）。内核侧再养一份
//! HTML 解析器是重复的、且是新的攻击面。

use mailparse::{DispositionType, MailHeaderMap, ParsedMail};

use crate::events::MailAttachment;

/// 纯文本正文存储上限（字节，UTF-8 边界截断）。
pub const MAX_STORED_TEXT_BYTES: usize = 128 * 1024;
/// HTML 原文存储上限（字节）。与上一项之和留足 `MAX_FRAME_BYTES` 余量。
pub const MAX_STORED_HTML_BYTES: usize = 256 * 1024;
/// HTML 被截断时追加的标记，使前端可如实告知用户。
pub const HTML_TRUNCATION_MARKER: &str = "<!-- maild: truncated -->";
/// 单个附件 part 内联落库上限。
pub const ATTACHMENT_INLINE_MAX_BYTES: usize = 2 * 1024 * 1024;
/// 单封邮件内联附件累计上限。
pub const ATTACHMENT_INLINE_TOTAL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// 列表预览字符数。
pub const SNIPPET_CHARS: usize = 200;

/// `Date` 头可信区间（决策 3）：早于 `received_at` 十年或晚于 24 小时即不可信。
const DATE_PAST_TOLERANCE_SECS: u64 = 10 * 365 * 24 * 60 * 60;
const DATE_FUTURE_TOLERANCE_SECS: u64 = 24 * 60 * 60;

/// 头部长度上限（字节）。`recipient` 沿用 ADR-0022 的 512，不在此改动。
pub const MAX_ADDRESS_LIST_BYTES: usize = 2048;
pub const MAX_MESSAGE_ID_BYTES: usize = 998;
pub const MAX_REFERENCES_BYTES: usize = 8192;

/// 剥离控制字符并按 UTF-8 字符边界截断。头部一律经此函数入库。
pub fn sanitize_header(value: &str, max_bytes: usize) -> String {
    let mut out: String = value.chars().filter(|ch| !ch.is_control()).collect();
    truncate_utf8_bytes(&mut out, max_bytes);
    out
}

/// 就地截断到 `max_bytes` 以内的最大 UTF-8 字符边界；返回是否发生截断。
pub fn truncate_utf8_bytes(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    true
}

/// 剥离 `Message-ID` 类头部的尖括号。
pub fn strip_angle_brackets(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_string()
}

/// 列表预览：正文前 [`SNIPPET_CHARS`] 个字符，换行折叠成空格。
pub fn snippet_of(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
        if out.chars().count() >= SNIPPET_CHARS {
            break;
        }
    }
    out
}

/// RFC 822 早于 1971 年不存在。宽松的日期解析器对无法识别的输入会静默返回
/// 纪元附近的值而不是报错（例如非英文的自由文本），因此下界比「非负」更可靠。
const EARLIEST_PLAUSIBLE_DATE_SECS: u64 = 31_536_000;

/// 解析 `Date` 头。不可解析、以及早于 [`EARLIEST_PLAUSIBLE_DATE_SECS`] 的结果
/// 一律返回 `None`——把垃圾当成 1970 年显示给用户比不显示更糟。
pub fn parse_date_header(value: &str) -> Option<u64> {
    if value.trim().is_empty() {
        return None;
    }
    mailparse::dateparse(value)
        .ok()
        .and_then(|secs| u64::try_from(secs).ok())
        .filter(|secs| *secs >= EARLIEST_PLAUSIBLE_DATE_SECS)
}

/// 排序键（决策 3）：可信的 `date_at`，否则回退投递观测时间。
///
/// 「可信」不等于「解析成功」：伪造远未来的 `Date` 是常见垃圾邮件手法，会永久
/// 占据列表顶部。`date_at` 本身原样保留——显示发信人声称的时间是诚实的，
/// 让它决定排序不是。
pub fn sort_key(date_at: Option<u64>, received_at: u64) -> u64 {
    match date_at {
        Some(date) if date <= received_at.saturating_add(DATE_FUTURE_TOLERANCE_SECS)
            && date >= received_at.saturating_sub(DATE_PAST_TOLERANCE_SECS) => date,
        _ => received_at,
    }
}

/// 从 MIME 树抽出的正文与附件。
#[derive(Debug, Default)]
pub struct MessageParts {
    pub text: String,
    pub html: String,
    pub attachments: Vec<MailAttachment>,
}

/// 深度优先遍历 MIME 树，分离正文候选与附件。
///
/// `part_index` 对每个访问到的节点递增（含容器节点），因此在 `(account, uid)`
/// 内稳定，可作为附件的自然键。
pub fn collect_parts(mail: &ParsedMail) -> MessageParts {
    let mut parts = MessageParts::default();
    let mut texts: Vec<String> = Vec::new();
    let mut htmls: Vec<String> = Vec::new();
    let mut index: u32 = 0;
    let mut inlined_total: usize = 0;
    walk(mail, &mut index, &mut texts, &mut htmls, &mut parts.attachments, &mut inlined_total);
    parts.text = texts.join("\n");
    parts.html = htmls.join("\n");
    parts
}

fn walk(
    node: &ParsedMail,
    index: &mut u32,
    texts: &mut Vec<String>,
    htmls: &mut Vec<String>,
    attachments: &mut Vec<MailAttachment>,
    inlined_total: &mut usize,
) {
    let part_index = *index;
    *index = index.saturating_add(1);

    if !node.subparts.is_empty() {
        for sub in &node.subparts {
            walk(sub, index, texts, htmls, attachments, inlined_total);
        }
        return;
    }

    let disposition = node.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .or_else(|| node.ctype.params.get("name"))
        .map(|name| sanitize_header(name, 255))
        .unwrap_or_default();
    let content_id = node
        .get_headers()
        .get_first_value("Content-ID")
        .map(|value| sanitize_header(&strip_angle_brackets(&value), MAX_MESSAGE_ID_BYTES))
        .unwrap_or_default();
    let is_attachment_disposition = disposition.disposition == DispositionType::Attachment;
    let mime = node.ctype.mimetype.to_ascii_lowercase();
    let is_body_candidate = (mime == "text/plain" || mime == "text/html")
        && !is_attachment_disposition
        && filename.is_empty();

    if is_body_candidate {
        let body = node.get_body().unwrap_or_default();
        let body = body.trim_end().to_string();
        if body.is_empty() {
            return;
        }
        if mime == "text/plain" {
            texts.push(body);
        } else {
            htmls.push(body);
        }
        return;
    }

    let Ok(bytes) = node.get_body_raw() else {
        return;
    };
    let size = bytes.len();
    // 「内联」的判据是**正文引用了它**（有 Content-ID），不是发信方声明了
    // `Content-Disposition: inline`。缺省 disposition 就是 inline，若照它判断，
    // 只带 `name=` 参数的真附件会被误判成内联而不点亮回形针。
    let inline = !is_attachment_disposition && !content_id.is_empty();
    let content = if size <= ATTACHMENT_INLINE_MAX_BYTES
        && inlined_total.saturating_add(size) <= ATTACHMENT_INLINE_TOTAL_MAX_BYTES
    {
        *inlined_total = inlined_total.saturating_add(size);
        Some(bytes)
    } else {
        None
    };
    attachments.push(MailAttachment {
        part_index,
        filename,
        mime_type: sanitize_header(&mime, 255),
        content_id,
        inline,
        size: size as u64,
        stored: content.is_some(),
        content,
    });
}

// —— HTML → 纯文本 ——

/// 元素内容整体丢弃的标签。此前的 `<[^>]*>` 正则只删标签本身，
/// `<style>` / `<script>` 的**内容**会作为正文泄漏进摘要、规则匹配和通知。
const RAW_TEXT_TAGS: &[&str] = &["script", "style", "head", "title", "textarea", "noscript"];

/// 产生换行的块级标签。
const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "tr", "li", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "table",
    "ul", "ol", "dl", "dd", "dt", "section", "article", "header", "footer", "hr", "pre", "form",
    "figure", "figcaption", "address", "fieldset", "main", "nav", "aside",
];

/// 产生空格的单元格标签。
const CELL_TAGS: &[&str] = &["td", "th"];

/// HTML → 纯文本。单遍字节扫描，正确处理注释、CDATA、属性值内的 `>`
/// 与 raw-text 元素。
pub fn html_to_text(html: &str) -> String {
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len / 2);
    let mut cursor = 0usize;
    let mut text_start = 0usize;

    while cursor < len {
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        push_decoded(&mut out, &html[text_start..cursor]);

        let after = cursor + 1;
        if bytes[after..].starts_with(b"!--") {
            cursor = find_from(bytes, after + 3, b"-->")
                .map(|position| position + 3)
                .unwrap_or(len);
        } else if bytes[after..].starts_with(b"![CDATA[") {
            cursor = find_from(bytes, after + 8, b"]]>")
                .map(|position| position + 3)
                .unwrap_or(len);
        } else if matches!(bytes.get(after), Some(b'!') | Some(b'?')) {
            cursor = scan_tag_end(bytes, after);
        } else {
            let closing = bytes.get(after) == Some(&b'/');
            let name_start = after + usize::from(closing);
            let mut name_end = name_start;
            while name_end < len && bytes[name_end].is_ascii_alphanumeric() {
                name_end += 1;
            }
            if name_end == name_start {
                // 不是标签起始，`<` 是普通字符。
                out.push('<');
                cursor = after;
                text_start = cursor;
                continue;
            }
            let name = html[name_start..name_end].to_ascii_lowercase();
            let tag_end = scan_tag_end(bytes, name_end);
            cursor = tag_end;
            if !closing && RAW_TEXT_TAGS.contains(&name.as_str()) {
                // 找不到匹配的闭标签时**不跳过**：宁可多一点噪音，
                // 也不要因为一个畸形 `<head>` 丢掉整封正文。
                if let Some(end) = skip_raw_text(bytes, tag_end, &name) {
                    cursor = end;
                }
            } else if BLOCK_TAGS.contains(&name.as_str()) {
                out.push('\n');
            } else if CELL_TAGS.contains(&name.as_str()) {
                out.push(' ');
            }
        }
        text_start = cursor;
    }
    push_decoded(&mut out, &html[text_start.min(len)..]);
    normalize_text(&out)
}

/// 从 `from` 起扫描到标签结束的 `>`（跳过引号内的 `>`），返回其后一位。
fn scan_tag_end(bytes: &[u8], from: usize) -> usize {
    let mut cursor = from;
    let mut quote = 0u8;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if quote != 0 {
            if byte == quote {
                quote = 0;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = byte;
        } else if byte == b'>' {
            return cursor + 1;
        }
        cursor += 1;
    }
    bytes.len()
}

/// 跳到 `</name ...>` 之后；找不到返回 `None`。
fn skip_raw_text(bytes: &[u8], from: usize, name: &str) -> Option<usize> {
    let needle = format!("</{name}");
    let needle = needle.as_bytes();
    let mut cursor = from;
    while cursor + needle.len() <= bytes.len() {
        if bytes[cursor..cursor + needle.len()].eq_ignore_ascii_case(needle) {
            let next = bytes.get(cursor + needle.len());
            if !matches!(next, Some(byte) if byte.is_ascii_alphanumeric()) {
                return Some(scan_tag_end(bytes, cursor + needle.len()));
            }
        }
        cursor += 1;
    }
    None
}

fn find_from(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| from + position)
}

const NAMED_ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    // 不断行空格解码成普通空格，否则后续空白折叠对它无效。
    ("nbsp", " "),
    ("ensp", " "),
    ("emsp", " "),
    ("thinsp", " "),
    ("shy", ""),
    ("zwnj", ""),
    ("mdash", "—"),
    ("ndash", "–"),
    ("hellip", "…"),
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("laquo", "«"),
    ("raquo", "»"),
    ("ldquo", "“"),
    ("rdquo", "”"),
    ("lsquo", "‘"),
    ("rsquo", "’"),
    ("bull", "•"),
    ("middot", "·"),
];

/// 解码实体后追加。未知实体原样保留。
fn push_decoded(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    let bytes = text.as_bytes();
    let mut cursor = 0usize;
    let mut chunk_start = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'&' {
            cursor += 1;
            continue;
        }
        let Some(offset) = bytes[cursor..].iter().take(12).position(|byte| *byte == b';') else {
            cursor += 1;
            continue;
        };
        let entity = &text[cursor + 1..cursor + offset];
        let Some(decoded) = decode_entity(entity) else {
            cursor += 1;
            continue;
        };
        out.push_str(&text[chunk_start..cursor]);
        out.push_str(&decoded);
        cursor += offset + 1;
        chunk_start = cursor;
    }
    out.push_str(&text[chunk_start..]);
}

fn decode_entity(entity: &str) -> Option<String> {
    if entity.is_empty() {
        return None;
    }
    if let Some(digits) = entity.strip_prefix('#') {
        let code = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => digits.parse::<u32>().ok()?,
        };
        let ch = char::from_u32(code)?;
        // `&#0;` 之类会把控制字符（NUL 尤甚）送进正文文本和 SQLite TEXT 列。
        if ch.is_control() {
            return Some(String::new());
        }
        return Some(if ch == '\u{a0}' { ' '.to_string() } else { ch.to_string() });
    }
    let lowered = entity.to_ascii_lowercase();
    NAMED_ENTITIES
        .iter()
        .find(|(name, _)| *name == lowered)
        .map(|(_, value)| (*value).to_string())
}

/// 折叠空白：行内空白成单空格，行首行尾去空格，最多保留一个空行。
fn normalize_text(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let mut collapsed = String::with_capacity(line.len());
        let mut pending_space = false;
        for ch in line.chars() {
            if ch.is_whitespace() {
                pending_space = !collapsed.is_empty();
                continue;
            }
            if pending_space {
                collapsed.push(' ');
                pending_space = false;
            }
            collapsed.push(ch);
        }
        if collapsed.is_empty() && lines.last().is_some_and(|last| last.is_empty()) {
            continue;
        }
        lines.push(collapsed);
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
