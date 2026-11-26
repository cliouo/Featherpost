//! 联系人自动补全的纯解析、过滤与排名层（ADR-0034 合规，零 schema 变更）。
//!
//! 本模块不持有数据库连接也不触碰 `drafts` 表：[`crate::store::Store`] 仅在锁内拉取
//! 有界原始消息头行，随后立即释放锁，由本模块在锁外完成 `mailparse` 解析、截断尾保护、
//! `validate_send_field` 逐地址过滤、RFC 5321 规范化、己方别名降权与加权排名。

use std::collections::{HashMap, HashSet};

use maild_client::{validate_send_field, SendField};
use mailparse::{addrparse, MailAddr, SingleInfo};
use serde::{Deserialize, Serialize};

use crate::mime;
use crate::search::segment;

/// 默认返回建议条数。
pub const DEFAULT_CONTACT_LIMIT: usize = 8;
/// 单次 `searchContacts` 返回建议上限。
pub const MAX_CONTACT_LIMIT: usize = 20;
/// `query` 参数字节上限。
pub const MAX_CONTACT_QUERY_BYTES: usize = 256;
/// `account` 参数字节上限。
pub const MAX_CONTACT_ACCOUNT_BYTES: usize = 256;
/// 回退 FTS5 前缀查询最多取用的词数。
pub const MAX_CONTACT_FTS_TERMS: usize = 6;
/// 近期全账户样本扫描条数（走 `idx_messages_account_sort`）。
pub const RECENT_CONTACT_SAMPLE_LIMIT: usize = 800;
/// 近期已发送文件夹专项样本条数（走 `idx_messages_folder_sort`），防高频收件箱淹没常联人。
pub const SENT_CONTACT_SAMPLE_LIMIT: usize = 200;
/// 近期样本不足时，按 `sender`/`recipients` 列的 FTS5 前缀回退最多扫描条数。
pub const FALLBACK_PREFIX_ROW_LIMIT: usize = 250;
/// 并发 `searchContacts` 槽位上限，防止快速击键风暴争抢 `Store` 锁与阻塞线程池。
pub const MAX_CONCURRENT_CONTACT_SEARCHES: usize = 4;

/// `messages.recipient` 列落库截断上限（`receive::sanitize_detail_owned`）。
const RECIPIENT_COLUMN_CAP: usize = 512;
/// 显示名纯文本字节上限。
const MAX_DISPLAY_NAME_BYTES: usize = 128;

/// 锁内拉取的原始消息行：仅含地址头、排序时间与文件夹角色，不含正文或草稿。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContactMessageRow {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub cc: String,
    pub sort_at: u64,
    pub folder_role: String,
}

/// 单条联系人建议：裸 ASCII `addr-spec` + 可选纯文本显示名。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct ContactSuggestion {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 从单个头部解析出的已校验联系人条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedContact {
    pub address: String,
    pub name: Option<String>,
}

/// RFC 5321 规范化：保留 local-part 原始大小写，仅小写化 domain。
pub fn canonical_addr_spec(addr: &str) -> Option<String> {
    let trimmed = addr.trim().trim_start_matches('<').trim_end_matches('>').trim();
    validate_send_field(SendField::To, trimmed).ok()?;
    let (local, domain) = trimmed.split_once('@')?;
    Some(format!("{local}@{}", domain.to_ascii_lowercase()))
}

/// 剥离截断或语法未闭合的末尾条目，避免截断在 `.com` 中间的 `bob@example.co` 被当成有效地址。
fn trim_truncated_tail(raw: &str, column_cap: Option<usize>) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "";
    }
    let mut in_quotes = false;
    let mut escaped = false;
    let mut angle_depth: usize = 0;
    let mut last_top_comma: Option<usize> = None;

    for (idx, ch) in trimmed.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes {
            match ch {
                '\\' => escaped = true,
                '"' => in_quotes = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '<' => angle_depth = angle_depth.saturating_add(1),
            '>' => angle_depth = angle_depth.saturating_sub(1),
            ',' if angle_depth == 0 => last_top_comma = Some(idx),
            _ => {}
        }
    }

    let unbalanced = in_quotes || escaped || angle_depth > 0;
    let hit_cap = column_cap.is_some_and(|cap| {
        // `recipient` 按 `chars().take(512)` 截断；`cc` 按 UTF-8 字节边界截断在 2048，
        // 4 字节字符可能提前最多 3 字节收尾。
        raw.len() >= cap.saturating_sub(3) || raw.chars().count() >= cap
    });
    let ends_cleanly_delimited =
        !unbalanced && matches!(trimmed.as_bytes().last(), Some(b'>' | b';'));

    if unbalanced || (hit_cap && !ends_cleanly_delimited) {
        match last_top_comma {
            Some(comma_idx) => trimmed[..comma_idx].trim(),
            None => "",
        }
    } else {
        trimmed
    }
}

/// 按顶层逗号切分（尊重双引号、转义与尖括号），用于畸形头部的分段容错恢复。
fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut angle_depth: usize = 0;
    let mut start = 0usize;

    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes {
            match ch {
                '\\' => escaped = true,
                '"' => in_quotes = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '<' => angle_depth = angle_depth.saturating_add(1),
            '>' => angle_depth = angle_depth.saturating_sub(1),
            ',' if angle_depth == 0 => {
                parts.push(input[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

/// 找到组头 `Group Name: member1, ...` 中顶层冒号后的部分，用于截断组头的容错恢复。
fn strip_top_level_group_prefix(segment: &str) -> Option<&str> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut angle_depth: usize = 0;
    for (idx, ch) in segment.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes {
            match ch {
                '\\' => escaped = true,
                '"' => in_quotes = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '<' => angle_depth = angle_depth.saturating_add(1),
            '>' => angle_depth = angle_depth.saturating_sub(1),
            ':' if angle_depth == 0 => {
                let rest = segment[idx + 1..].trim();
                return (!rest.is_empty()).then_some(rest);
            }
            _ => {}
        }
    }
    None
}

fn clean_display_name(raw: Option<String>, canonical_address: &str) -> Option<String> {
    let raw = raw?;
    let mut cleaned = String::with_capacity(raw.len().min(MAX_DISPLAY_NAME_BYTES));
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = !cleaned.is_empty();
            continue;
        }
        if pending_space {
            if cleaned.len() + 1 > MAX_DISPLAY_NAME_BYTES {
                break;
            }
            cleaned.push(' ');
            pending_space = false;
        }
        if cleaned.len() + ch.len_utf8() > MAX_DISPLAY_NAME_BYTES {
            break;
        }
        cleaned.push(ch);
    }
    let trimmed = cleaned.trim().trim_matches('"').trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(canonical_address) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn push_single_info(out: &mut Vec<ParsedContact>, info: &SingleInfo) {
    let Some(address) = canonical_addr_spec(&info.addr) else {
        return;
    };
    let name = clean_display_name(info.display_name.clone(), &address);
    out.push(ParsedContact { address, name });
}

fn collect_mail_addrs(out: &mut Vec<ParsedContact>, list: &mailparse::MailAddrList) {
    for entry in list.iter() {
        match entry {
            MailAddr::Single(info) => push_single_info(out, info),
            MailAddr::Group(group) => {
                for member in &group.addrs {
                    push_single_info(out, member);
                }
            }
        }
    }
}

/// 从一个 RFC 5322 地址头解析并校验联系人（含组地址与带逗号引号显示名），自动跳过截断尾与非法地址。
pub fn extract_contacts_from_header(raw: &str, column_cap: Option<usize>) -> Vec<ParsedContact> {
    let cleaned = trim_truncated_tail(raw, column_cap);
    if cleaned.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(list) = addrparse(cleaned) {
        collect_mail_addrs(&mut out, &list);
        return out;
    }
    // 整行解析失败（例如被截断的组头缺少分号，或中间夹杂畸形片段）时，按顶层逗号逐段安全恢复。
    for part in split_top_level_commas(cleaned) {
        if part.is_empty() {
            continue;
        }
        if let Ok(list) = addrparse(part) {
            collect_mail_addrs(&mut out, &list);
            continue;
        }
        if let Some(after_colon) = strip_top_level_group_prefix(part) {
            if let Ok(list) = addrparse(after_colon) {
                collect_mail_addrs(&mut out, &list);
            }
        }
    }
    out
}

/// 构造严格限定在 `{sender recipients}` 列的转义 FTS5 前缀查询表达式。
///
/// 所有 FTS5 运算符与标点均被剥离或包进引号，只保留字母/数字/表意文字 token；
/// 词数与字节数双重有界。无有效 token 时返回 `None`。
pub fn build_contact_fts_prefix_expression(query: &str) -> Option<String> {
    let query = if query.len() > MAX_CONTACT_QUERY_BYTES {
        let mut end = MAX_CONTACT_QUERY_BYTES;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        &query[..end]
    } else {
        query
    };

    let mut clauses: Vec<String> = Vec::new();
    let flush_run = |buf: &mut String, clauses: &mut Vec<String>| -> bool {
        if buf.is_empty() || clauses.len() >= MAX_CONTACT_FTS_TERMS {
            buf.clear();
            return clauses.len() >= MAX_CONTACT_FTS_TERMS;
        }
        let segmented = segment(buf);
        buf.clear();
        if segmented.is_empty() {
            return false;
        }
        let escaped = segmented.replace('"', "\"\"");
        clauses.push(format!("\"{escaped}\"*"));
        clauses.len() >= MAX_CONTACT_FTS_TERMS
    };

    for word in query.split_whitespace() {
        // 按非字母数字切段，既中和 `"`, `*`, `:`, `-`, `(`, `)`, `@`, `.` 等符号，
        // 又让 `ali@ex` 拆为 `"ali"* AND "ex"*` 命中 `alice@example.com`。
        let mut run = String::new();
        for ch in word.chars() {
            if ch.is_alphanumeric() {
                run.push(ch);
            } else if flush_run(&mut run, &mut clauses) {
                break;
            }
        }
        if flush_run(&mut run, &mut clauses) {
            break;
        }
    }

    if clauses.is_empty() {
        return None;
    }
    Some(format!("{{sender recipients}} : {}", clauses.join(" AND ")))
}

struct ContactAccumulator {
    address: String,
    name: Option<String>,
    name_sort_at: u64,
    score: f64,
    last_seen: u64,
    is_self: bool,
}

fn is_own_address(address: &str, own_addresses: &[String]) -> bool {
    own_addresses
        .iter()
        .any(|own| !own.is_empty() && own.eq_ignore_ascii_case(address))
}

/// 判断单个联系人是否命中查询词并返回匹配质量档位（0..=3）。
///
/// 仅在每个已提取联系人自身 `(address, name)` 上判定，确保正文或同封邮件中的无关同收件人绝不误命中。
fn match_tier_for_contact(address: &str, name: Option<&str>, terms: &[String], full_query: &str) -> Option<u8> {
    if terms.is_empty() {
        return Some(0);
    }
    let addr_lower = address.to_lowercase();
    let name_lower = name.map(str::to_lowercase);

    for term in terms {
        let in_addr = addr_lower.contains(term.as_str());
        let in_name = name_lower
            .as_deref()
            .is_some_and(|n| n.contains(term.as_str()));
        if !in_addr && !in_name {
            return None;
        }
    }

    let (local_lower, domain_lower) = addr_lower.split_once('@').unwrap_or((&addr_lower, ""));
    let exact = addr_lower == full_query
        || local_lower == full_query
        || name_lower.as_deref() == Some(full_query);
    if exact {
        return Some(3);
    }
    let prefix = addr_lower.starts_with(full_query)
        || name_lower.as_deref().is_some_and(|n| {
            n.starts_with(full_query)
                || n.split_whitespace().any(|word| word.starts_with(full_query))
        });
    if prefix {
        return Some(2);
    }
    if domain_lower.starts_with(full_query) {
        return Some(1);
    }
    Some(0)
}

/// 对锁外已取出的消息行执行解析、去重、加权、己方降权与确定性排名。
///
/// - `Sent` 文件夹收件人加权更高（To: 3×，Cc: 2×，From: 0.5×）；其余可见文件夹（From: 1×，To: 0.8×，Cc: 0.5×）。
/// - 同一封邮件内重复出现的地址只取最高角色权重，避免单封群发虚高。
/// - 己方主地址/别名严格降权排在所有非己方联系人之后。
pub fn rank_contacts(
    rows: &[ContactMessageRow],
    query: &str,
    own_addresses: &[String],
    limit: usize,
    now_secs: Option<u64>,
) -> Vec<ContactSuggestion> {
    let limit = limit.clamp(1, MAX_CONTACT_LIMIT);
    let max_sort_at = rows.iter().map(|row| row.sort_at).max().unwrap_or(0);
    let reference_now = now_secs.unwrap_or(max_sort_at).max(max_sort_at);

    let mut seen_row_ids: HashSet<i64> = HashSet::with_capacity(rows.len());
    let mut contacts: HashMap<String, ContactAccumulator> = HashMap::new();

    for row in rows {
        if !seen_row_ids.insert(row.id) {
            continue;
        }
        let is_sent = row.folder_role == "sent";
        let (sender_w, to_w, cc_w) = if is_sent {
            (0.5_f64, 3.0_f64, 2.0_f64)
        } else {
            (1.0_f64, 0.8_f64, 0.5_f64)
        };

        let age_days = reference_now.saturating_sub(row.sort_at) as f64 / 86_400.0;
        let recency = 1.0 / (1.0 + age_days / 30.0);

        // 单封消息内先按地址合并，取最高权重与非空显示名。
        let mut per_message: HashMap<String, (f64, Option<String>)> = HashMap::new();
        let headers: [(&str, Option<usize>, f64); 3] = [
            (&row.recipient, Some(RECIPIENT_COLUMN_CAP), to_w),
            (&row.cc, Some(mime::MAX_ADDRESS_LIST_BYTES), cc_w),
            (&row.sender, None, sender_w),
        ];

        for (raw, cap, weight) in headers {
            if raw.trim().is_empty() {
                continue;
            }
            for parsed in extract_contacts_from_header(raw, cap) {
                per_message
                    .entry(parsed.address)
                    .and_modify(|(best_w, best_name)| {
                        if weight > *best_w {
                            *best_w = weight;
                        }
                        if best_name.is_none() && parsed.name.is_some() {
                            *best_name = parsed.name.clone();
                        }
                    })
                    .or_insert((weight, parsed.name));
            }
        }

        for (address, (weight, name)) in per_message {
            let delta = weight * recency;
            contacts
                .entry(address.clone())
                .and_modify(|acc| {
                    acc.score += delta;
                    if row.sort_at > acc.last_seen {
                        acc.last_seen = row.sort_at;
                    }
                    if let Some(candidate_name) = &name {
                        if acc.name.is_none() || row.sort_at > acc.name_sort_at {
                            acc.name = Some(candidate_name.clone());
                            acc.name_sort_at = row.sort_at;
                        }
                    }
                })
                .or_insert_with(|| {
                    let is_self = is_own_address(&address, own_addresses);
                    let name_sort_at = if name.is_some() { row.sort_at } else { 0 };
                    ContactAccumulator {
                        address,
                        name,
                        name_sort_at,
                        score: delta,
                        last_seen: row.sort_at,
                        is_self,
                    }
                });
        }
    }

    let trimmed_query = query.trim().to_lowercase();
    let terms: Vec<String> = trimmed_query
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect();

    let mut ranked: Vec<(bool, u8, f64, u64, ContactSuggestion)> = contacts
        .into_values()
        .filter_map(|acc| {
            let tier = match_tier_for_contact(
                &acc.address,
                acc.name.as_deref(),
                &terms,
                &trimmed_query,
            )?;
            Some((
                acc.is_self,
                tier,
                acc.score,
                acc.last_seen,
                ContactSuggestion {
                    address: acc.address,
                    name: acc.name,
                },
            ))
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.4.address.cmp(&b.4.address))
    });

    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, _, _, suggestion)| suggestion)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_names_groups_and_preserves_local_part_case() {
        let header = r#""Doe, John" <John.Doe@Example.COM>, Engineering: Alice <Alice@ORG.TEST>, "Bob, Jr." <bob@org.test>;"#;
        let parsed = extract_contacts_from_header(header, None);
        assert_eq!(
            parsed,
            vec![
                ParsedContact {
                    address: "John.Doe@example.com".into(),
                    name: Some("Doe, John".into()),
                },
                ParsedContact {
                    address: "Alice@org.test".into(),
                    name: Some("Alice".into()),
                },
                ParsedContact {
                    address: "bob@org.test".into(),
                    name: Some("Bob, Jr.".into()),
                },
            ]
        );
    }

    #[test]
    fn filters_invalid_addresses_with_validate_send_field() {
        let header = "valid@example.com, bad..dots@example.com, \"No At\" <not-an-email>, \
                      space in@example.com, nonascii@例え.jp, ok.two@sub.example.org";
        let parsed = extract_contacts_from_header(header, None);
        let addresses: Vec<&str> = parsed.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addresses, vec!["valid@example.com", "ok.two@sub.example.org"]);
    }

    #[test]
    fn skips_truncated_trailing_tokens_from_capped_columns() {
        // 模拟 512 字节截断在 `carol@example.com` 中间变成看似合法的 `carol@example.co`。
        let prefix = (0..18)
            .map(|i| format!("user{i:02}@example.com"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut capped = format!("{prefix}, ");
        let pad_len = RECIPIENT_COLUMN_CAP.saturating_sub(capped.len() + "carol@example.co".len());
        if pad_len > 0 {
            capped.push_str(&format!("{}@example.com, ", "a".repeat(pad_len.saturating_sub(14).max(1))));
        }
        capped.push_str("carol@example.co");
        // 精确补齐到 512 字节且以裸地址尾段收尾。
        let mut exact_512 = format!("{prefix}, ");
        let filler_local_len = 512 - exact_512.len() - "@example.com, carol@example.co".len();
        exact_512.push_str(&"x".repeat(filler_local_len));
        exact_512.push_str("@example.com, carol@example.co");
        assert_eq!(exact_512.len(), 512);

        let parsed = extract_contacts_from_header(&exact_512, Some(RECIPIENT_COLUMN_CAP));
        assert!(parsed.iter().any(|c| c.address == "user00@example.com"));
        assert!(
            !parsed.iter().any(|c| c.address == "carol@example.co"),
            "截断尾段 carol@example.co 不得作为有效地址泄漏: {parsed:?}"
        );

        // 未闭合尖括号或引号同样被安全跳过，不影响前面的完整联系人。
        let unclosed = r#"Alice <alice@example.com>, "Bob" <bob@exam"#;
        let parsed_unclosed = extract_contacts_from_header(unclosed, Some(RECIPIENT_COLUMN_CAP));
        assert_eq!(
            parsed_unclosed,
            vec![ParsedContact {
                address: "alice@example.com".into(),
                name: Some("Alice".into()),
            }]
        );
    }

    #[test]
    fn keeps_cleanly_terminated_angle_addr_at_column_cap() {
        let prefix = (0..18)
            .map(|i| format!("user{i:02}@example.com"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut exact_512 = format!("{prefix}, ");
        let suffix = "<final@example.com>";
        let filler_local_len = 512 - exact_512.len() - "@example.com, ".len() - suffix.len();
        exact_512.push_str(&"z".repeat(filler_local_len));
        exact_512.push_str("@example.com, ");
        exact_512.push_str(suffix);
        assert_eq!(exact_512.len(), 512);

        let parsed = extract_contacts_from_header(&exact_512, Some(RECIPIENT_COLUMN_CAP));
        assert!(parsed.iter().any(|c| c.address == "final@example.com"));
    }

    #[test]
    fn sent_weighting_recency_and_self_alias_demotion_rank_deterministically() {
        let base = 1_750_000_000_u64;
        let rows = vec![
            // 收件箱收到 2 封来自 inboxer 的信（权重 1.0 * 2 = 2.0）
            ContactMessageRow {
                id: 1,
                sender: "Inboxer <inboxer@example.com>".into(),
                recipient: "Me <me@example.com>".into(),
                cc: "alias@example.com".into(),
                sort_at: base,
                folder_role: "inbox".into(),
            },
            ContactMessageRow {
                id: 2,
                sender: "Inboxer <inboxer@example.com>".into(),
                recipient: "me@example.com".into(),
                cc: String::new(),
                sort_at: base,
                folder_role: "inbox".into(),
            },
            // 已发送 1 封给 sent_target（权重 3.0 > 2.0）
            ContactMessageRow {
                id: 3,
                sender: "me@example.com".into(),
                recipient: "Sent Target <sent@example.com>".into(),
                cc: String::new(),
                sort_at: base,
                folder_role: "sent".into(),
            },
            // 重复的己方高权重记录：仍须被降权到所有非己方之后
            ContactMessageRow {
                id: 4,
                sender: "me@example.com".into(),
                recipient: "alias@example.com".into(),
                cc: String::new(),
                sort_at: base,
                folder_role: "sent".into(),
            },
        ];

        let own = vec!["me@example.com".into(), "ALIAS@example.com".into()];
        let ranked = rank_contacts(&rows, "", &own, 8, Some(base));
        let addresses: Vec<&str> = ranked.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(
            addresses,
            vec![
                "sent@example.com",
                "inboxer@example.com",
                "alias@example.com",
                "me@example.com",
            ]
        );
    }

    #[test]
    fn fts_prefix_expression_escapes_syntax_and_scopes_to_address_columns() {
        assert_eq!(
            build_contact_fts_prefix_expression("ali").as_deref(),
            Some("{sender recipients} : \"ali\"*")
        );
        assert_eq!(
            build_contact_fts_prefix_expression("ali@ex").as_deref(),
            Some("{sender recipients} : \"ali\"* AND \"ex\"*")
        );
        assert_eq!(
            build_contact_fts_prefix_expression("张三").as_deref(),
            Some("{sender recipients} : \"张 三\"*")
        );
        assert_eq!(
            build_contact_fts_prefix_expression("a\"b NEAR* -c (d)").as_deref(),
            Some("{sender recipients} : \"a\"* AND \"b\"* AND \"NEAR\"* AND \"c\"* AND \"d\"*")
        );
        assert_eq!(build_contact_fts_prefix_expression("   @@@   "), None);
    }
}
