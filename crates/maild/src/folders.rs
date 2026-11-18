//! 文件夹领域语义（ADR-0026）：role 判定与 IMAP mailbox 名的 modified UTF-7 解码。
//!
//! 这里刻意不依赖 IMAP 客户端类型：调用方把 RFC 6154 特殊用途属性翻译成 `\Xxx` 字符串再传进来，
//! 使判定逻辑可以脱离网络层直接测试。

use serde::{Deserialize, Serialize};

/// 文件夹的语义角色。空（`None`）表示用户自建文件夹——**不猜**。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Archive,
    Spam,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Archive => "archive",
            Self::Spam => "spam",
        }
    }

    /// 库内文本 → role。空串是合法的「无角色」，由调用方以 `None` 表达。
    pub fn parse(value: &str) -> Option<FolderRole> {
        match value {
            "inbox" => Some(Self::Inbox),
            "sent" => Some(Self::Sent),
            "drafts" => Some(Self::Drafts),
            "trash" => Some(Self::Trash),
            "archive" => Some(Self::Archive),
            "spam" => Some(Self::Spam),
            _ => None,
        }
    }

    /// `Option<FolderRole>` → 库内文本（`None` = 空串）。
    pub fn to_column(role: Option<FolderRole>) -> &'static str {
        match role {
            Some(role) => role.as_str(),
            None => "",
        }
    }
}

/// 一条文件夹记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderRecord {
    pub id: i64,
    pub account: String,
    /// 服务器线上的原名（可能是 modified UTF-7）。`SELECT` 的实参，也是唯一键。
    pub name: String,
    /// 解码后的展示名。
    pub display_name: String,
    pub role: Option<FolderRole>,
    pub uid_validity: Option<u32>,
    pub last_seen_uid: Option<u32>,
    pub synced_at: Option<u64>,
    /// 最近一次 `LIST` 是否仍列出它。为 false 的文件夹不再同步，但消息保留（ADR-0026 决策 8）。
    pub present: bool,
}

/// RFC 6154 特殊用途属性 → role。服务器的明示声明永远优先于名字启发式。
///
/// `\All`（Gmail 的「所有邮件」）按归档处理：它是一个包含全部历史邮件的虚拟信箱，
/// 语义上最接近归档。`\Flagged` 是视图而不是容器，不映射。
pub fn role_from_special_use(attribute: &str) -> Option<FolderRole> {
    let attribute = attribute.trim_start_matches('\\');
    if attribute.eq_ignore_ascii_case("Sent") {
        Some(FolderRole::Sent)
    } else if attribute.eq_ignore_ascii_case("Drafts") {
        Some(FolderRole::Drafts)
    } else if attribute.eq_ignore_ascii_case("Trash") {
        Some(FolderRole::Trash)
    } else if attribute.eq_ignore_ascii_case("Archive") || attribute.eq_ignore_ascii_case("All") {
        Some(FolderRole::Archive)
    } else if attribute.eq_ignore_ascii_case("Junk") {
        Some(FolderRole::Spam)
    } else {
        None
    }
}

/// 名字启发式**兜底**。相当多的服务器不宣告 SPECIAL-USE。
///
/// 只匹配明确无歧义的名字。`垃圾箱` 之类不同服务商指向不同角色（有的是废纸篓、有的是垃圾邮件）
/// 的名字一律不匹配——猜错比留空糟糕得多。
fn role_from_leaf_name(leaf: &str) -> Option<FolderRole> {
    let normalized = leaf.trim().to_ascii_lowercase();
    const TABLE: &[(FolderRole, &[&str])] = &[
        (FolderRole::Inbox, &["inbox", "收件箱"]),
        (
            FolderRole::Sent,
            &[
                "sent",
                "sent items",
                "sent mail",
                "sent messages",
                "已发送",
                "已发送邮件",
                "发件箱",
            ],
        ),
        (FolderRole::Drafts, &["draft", "drafts", "草稿", "草稿箱"]),
        (
            FolderRole::Trash,
            &[
                "trash",
                "bin",
                "deleted",
                "deleted items",
                "deleted messages",
                "已删除",
                "已删除邮件",
                "废纸篓",
            ],
        ),
        (
            FolderRole::Archive,
            &["archive", "archives", "all mail", "归档", "存档", "所有邮件"],
        ),
        (
            FolderRole::Spam,
            &["spam", "junk", "junk e-mail", "junk email", "bulk mail", "垃圾邮件"],
        ),
    ];
    TABLE
        .iter()
        .find(|(_, names)| names.iter().any(|name| *name == normalized))
        .map(|(role, _)| *role)
}

/// 取层级路径的末段。`[Gmail]/已发送邮件` 的角色由 `已发送邮件` 决定。
fn leaf_of<'a>(display_name: &'a str, delimiter: Option<&str>) -> &'a str {
    match delimiter.filter(|value| !value.is_empty()) {
        Some(delimiter) => display_name.rsplit(delimiter).next().unwrap_or(display_name),
        None => display_name,
    }
}

/// 判定顺序：`INBOX` 保留名 → RFC 6154 属性 → 名字启发式 → 无角色。
pub fn resolve_role(
    special_use: &[String],
    raw_name: &str,
    display_name: &str,
    delimiter: Option<&str>,
) -> Option<FolderRole> {
    // RFC 3501：INBOX 是大小写不敏感的保留名，永远是收件箱。
    if raw_name.eq_ignore_ascii_case("INBOX") {
        return Some(FolderRole::Inbox);
    }
    if let Some(role) = special_use.iter().find_map(|value| role_from_special_use(value)) {
        return Some(role);
    }
    role_from_leaf_name(leaf_of(display_name, delimiter))
}

// —— modified UTF-7（RFC 3501 §5.1.3）——

/// IMAP mailbox 名 → 展示名。
///
/// 只解不编：线上原名原样保存并用于 `SELECT`，回编码是 bug 的温床（ADR-0026 决策 7）。
/// 解码失败时**回退原名**——一个显示成乱码的文件夹，比一个因为名字编码不合规就整账户
/// 同步失败的客户端好。
pub fn decode_modified_utf7(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    try_decode_modified_utf7(raw).unwrap_or_else(|| raw.to_string())
}

fn try_decode_modified_utf7(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let mut encoded = String::new();
        let mut terminated = false;
        for next in chars.by_ref() {
            if next == '-' {
                terminated = true;
                break;
            }
            encoded.push(next);
        }
        if !terminated {
            // 孤立的 `&`：不是合法的 modified UTF-7。
            return None;
        }
        if encoded.is_empty() {
            // `&-` 是字面量 `&`。
            out.push('&');
            continue;
        }
        out.push_str(&decode_utf16_base64(&encoded)?);
    }
    Some(out)
}

/// modified BASE64：字母表与标准 BASE64 相同，但 `/` 换成 `,`，且不带 `=` 填充。
/// 解出的字节流是 UTF-16BE。
fn decode_utf16_base64(encoded: &str) -> Option<String> {
    let mut units: Vec<u16> = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for ch in encoded.chars() {
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            ',' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 16 {
            bits -= 16;
            units.push(((accumulator >> bits) & 0xffff) as u16);
            accumulator &= (1u32 << bits).wrapping_sub(1);
        }
    }
    // RFC 3501：剩余位不足一个编码单元，且必须全为零。
    if bits >= 6 || accumulator != 0 {
        return None;
    }
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_utf7_decodes_chinese_folder_names() {
        assert_eq!(decode_modified_utf7("&XfJT0ZAB-"), "已发送");
        assert_eq!(decode_modified_utf7("&XfJT0ZABkK5O9g-"), "已发送邮件");
        assert_eq!(decode_modified_utf7("&g0l6P3ux-"), "草稿箱");
        assert_eq!(decode_modified_utf7("&V4NXPpCuTvY-"), "垃圾邮件");
        assert_eq!(decode_modified_utf7("INBOX"), "INBOX");
        assert_eq!(decode_modified_utf7("INBOX&-Sent"), "INBOX&Sent");
        assert_eq!(decode_modified_utf7("&ZYdO9lk5-/2024"), "文件夹/2024");
    }

    #[test]
    fn malformed_names_fall_back_to_the_raw_value() {
        // 孤立 `&`、非法字母表、非零填充位：一律回退原名而不是让整账户同步失败。
        for raw in ["&XfJT0ZAB", "&$$$-", "&AQ-"] {
            assert_eq!(decode_modified_utf7(raw), raw, "{raw}");
        }
    }

    #[test]
    fn special_use_attributes_beat_name_heuristics() {
        // 名字看着像归档，服务器却明示这是垃圾邮件——以服务器为准。
        assert_eq!(
            resolve_role(&["\\Junk".into()], "Archive", "Archive", None),
            Some(FolderRole::Spam)
        );
        assert_eq!(
            resolve_role(&[], "Archive", "Archive", None),
            Some(FolderRole::Archive)
        );
    }

    #[test]
    fn inbox_is_always_the_inbox_regardless_of_case_or_attributes() {
        assert_eq!(
            resolve_role(&["\\Trash".into()], "inbox", "inbox", None),
            Some(FolderRole::Inbox)
        );
    }

    #[test]
    fn role_is_matched_on_the_leaf_of_the_hierarchy() {
        assert_eq!(
            resolve_role(&[], "[Gmail]/&XfJT0ZABkK5O9g-", "[Gmail]/已发送邮件", Some("/")),
            Some(FolderRole::Sent)
        );
        assert_eq!(
            resolve_role(&[], "INBOX.Drafts", "INBOX.Drafts", Some(".")),
            Some(FolderRole::Drafts)
        );
    }

    #[test]
    fn ambiguous_and_custom_names_get_no_role() {
        // 「垃圾箱」不同服务商指向不同角色；用户自建文件夹更不该被猜。
        for name in ["垃圾箱", "客户", "Projects", "2024 年报销"] {
            assert_eq!(resolve_role(&[], name, name, None), None, "{name}");
        }
    }
}
