//! search：本地全文检索的分析与查询构造（ADR-0029）。
//!
//! 这个模块只做纯函数：把文本切成 FTS5 能索引的形状、把用户输入变成一条安全的
//! `MATCH` 表达式、把会话头拆成线程图的边。库交互在 [`crate::store`]。

use crate::compose::sanitize_message_id;

/// 一次查询最多取用的词数。超出部分丢弃——继续 `AND` 下去只会更慢且更空。
pub const MAX_QUERY_TERMS: usize = 16;

/// 用户查询串的字节上限。
pub const MAX_QUERY_BYTES: usize = 512;

/// 一封消息贡献的线程边数上限（ADR-0029 决策 7）。
pub const MAX_THREAD_REFS: usize = 32;

/// 服务器侧 `SEARCH` 单次返回的 UID 上限（ADR-0029 决策 11）。
pub const MAX_SERVER_SEARCH_HITS: usize = 200;

/// 该字符是否需要作为**独立 token** 索引。
///
/// `unicode61` 把连续的中日文当成一个 token，「发票已开具」于是搜不到「发票」；
/// 逐字切开后每个汉字自成 token，查询侧发短语即可还原子串语义（ADR-0029 决策 3）。
///
/// 韩文谚文**不在此列**：韩语本来就用空格分词节，逐字切会把词素拆碎。
fn is_ideographic(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF   // 平假名、片假名
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 基本区
        | 0xF900..=0xFAFF // CJK 兼容表意文字
        | 0x20000..=0x2FA1F // CJK 扩展 B–F 与兼容补充
    )
}

/// 把文本转成索引用的分析结果：表意文字逐字空格分隔，其余原样。
///
/// 连续空白折叠成一个空格——索引里留着原始排版没有意义，只是体积。
pub fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // 只在真正要写字符时才补空格，避免尾随空白与连续空格。
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if is_ideographic(ch) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push(ch);
            pending_space = true;
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// 把一个用户词变成 FTS5 短语。切分后为空返回 `None`。
///
/// 整个词放进双引号里：引号内除 `"` 本身外全部按字面处理，因此 `*`、`-`、`NEAR`、
/// `AND` 这些语法元素都失去特殊含义（ADR-0029 决策 4）。多个 token 的短语要求位置
/// 相邻，这正是「原文里是一个连续子串」。
fn phrase_of(term: &str) -> Option<String> {
    let segmented = segment(term);
    if segmented.is_empty() {
        return None;
    }
    // FTS5 的字符串字面量用重复引号转义。
    let escaped = segmented.replace('"', "\"\"");
    Some(format!("\"{escaped}\""))
}

/// 用户输入 → 一条完整的 FTS5 `MATCH` 表达式。无可用词时返回 `None`
/// （调用方退化为纯结构化过滤，而不是报错）。
pub fn build_match_expression(query: &str) -> Option<String> {
    let query = if query.len() > MAX_QUERY_BYTES {
        // 按字符边界截断，不切碎多字节序列。
        let mut end = MAX_QUERY_BYTES;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        &query[..end]
    } else {
        query
    };
    let phrases: Vec<String> = query
        .split_whitespace()
        .filter_map(phrase_of)
        .take(MAX_QUERY_TERMS)
        .collect();
    if phrases.is_empty() {
        return None;
    }
    Some(phrases.join(" AND "))
}

/// 一封消息在线程图里贡献的边：自己的 `Message-ID`、`In-Reply-To`、
/// 以及 `References` 里的每一个 id。
///
/// 超过 [`MAX_THREAD_REFS`] 时保留**首条与最近若干条**：首条是会话根，丢了整条线程
/// 会断成两截（与 ADR-0028 决策 6 的 `References` 截断同源）。自身的 `Message-ID`
/// 永远保留——它是别人挂到这封信上的唯一锚点。
pub fn thread_ref_ids(message_id: &str, in_reply_to: &str, references: &str) -> Vec<String> {
    let own = sanitize_message_id(message_id);
    let mut ids: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !value.is_empty() && !ids.contains(&value) {
            ids.push(value);
        }
    };
    for token in references.split_whitespace() {
        push(sanitize_message_id(token));
    }
    push(sanitize_message_id(in_reply_to));
    if ids.len() > MAX_THREAD_REFS {
        // 首条 + 最近的若干条。
        let root = ids[0].clone();
        let tail_start = ids.len() - (MAX_THREAD_REFS - 1);
        let mut kept = vec![root];
        kept.extend_from_slice(&ids[tail_start..]);
        ids = kept;
    }
    if !own.is_empty() && !ids.contains(&own) {
        ids.push(own);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ideographs_become_single_character_tokens() {
        assert_eq!(segment("发票已开具"), "发 票 已 开 具");
        // 拉丁词整体保留，交给 unicode61 自己切。
        assert_eq!(segment("invoice attached"), "invoice attached");
        assert_eq!(segment("发票invoice"), "发 票 invoice");
        assert_eq!(segment("invoice发票"), "invoice 发 票");
    }

    #[test]
    fn whitespace_collapses_and_never_trails() {
        assert_eq!(segment("  hello   world  "), "hello world");
        assert_eq!(segment("发票\n\n已开具"), "发 票 已 开 具");
        assert_eq!(segment("   "), "");
        assert_eq!(segment(""), "");
    }

    #[test]
    fn hangul_is_not_split_because_korean_uses_spaces() {
        assert_eq!(segment("안녕하세요 세계"), "안녕하세요 세계");
    }

    #[test]
    fn kana_is_split_like_han() {
        assert_eq!(segment("こんにちは"), "こ ん に ち は");
    }

    #[test]
    fn a_two_character_chinese_word_becomes_an_adjacency_phrase() {
        // trigram 分词器要求 ≥3 字符，正好排除中文里最常用的两字词；
        // 逐字切分 + 短语没有这个下限（ADR-0029 决策 3）。
        assert_eq!(build_match_expression("发票").unwrap(), "\"发 票\"");
    }

    #[test]
    fn fts5_syntax_in_user_input_is_neutralised() {
        // 引号加倍转义；`*` `-` `NEAR` 在短语内部都失去语法含义。
        let expression = build_match_expression("a\"b NEAR* -c").unwrap();
        assert_eq!(expression, "\"a\"\"b\" AND \"NEAR*\" AND \"-c\"");
    }

    #[test]
    fn punctuation_only_terms_are_dropped_not_errors() {
        // 全是分隔符的词切完为空——丢弃而不是产生 `""` 这种 FTS5 语法错误。
        assert_eq!(build_match_expression("发票 ??? 单").unwrap(), "\"发 票\" AND \"???\" AND \"单\"");
        assert!(build_match_expression("   ").is_none());
        assert!(build_match_expression("").is_none());
    }

    #[test]
    fn term_count_and_length_are_bounded() {
        let many = (0..40).map(|i| format!("t{i}")).collect::<Vec<_>>().join(" ");
        let expression = build_match_expression(&many).unwrap();
        assert_eq!(expression.matches(" AND ").count(), MAX_QUERY_TERMS - 1);

        // 超长输入按字符边界截断，不会切碎多字节序列。
        let long = "发".repeat(MAX_QUERY_BYTES);
        assert!(build_match_expression(&long).is_some());
    }

    #[test]
    fn thread_edges_cover_self_and_every_ancestor() {
        let ids = thread_ref_ids("<c@x>", "<b@x>", "<a@x> <b@x>");
        assert_eq!(ids, vec!["a@x", "b@x", "c@x"]);
    }

    #[test]
    fn thread_edges_keep_the_root_when_truncated() {
        let references = (0..80).map(|i| format!("<r{i}@x>")).collect::<Vec<_>>().join(" ");
        let ids = thread_ref_ids("<self@x>", "", &references);
        assert_eq!(ids.len(), MAX_THREAD_REFS + 1);
        // 会话根必须活下来，否则整条线程断成两截。
        assert_eq!(ids[0], "r0@x");
        assert_eq!(ids[1], "r49@x");
        assert_eq!(ids[MAX_THREAD_REFS], "self@x");
    }

    #[test]
    fn thread_edges_survive_a_message_without_any_header() {
        assert!(thread_ref_ids("", "", "").is_empty());
        assert_eq!(thread_ref_ids("<only@x>", "", ""), vec!["only@x"]);
    }
}
