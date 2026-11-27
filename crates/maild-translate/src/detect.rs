//! Zero-dependency lightweight language detector (`<1ms` on 2,000-char email samples).
//!
//! Designed to fail quiet ([`DetectedLanguage::Unknown`]) whenever evidence is insufficient or
//! ambiguous (short subjects `< 16` effective chars, OTPs, GitHub diffs, kanji-only Japanese
//! ambiguity, or balanced mixed CJK/English technical emails) so auto-translate and the foreign
//! language banner never misfire. Users can still manually trigger translation at any time.

use serde::{Deserialize, Serialize};

/// Minimum number of effective non-noise characters required before returning a non-`Unknown`
/// language classification.
pub const MIN_EFFECTIVE_CHARS: usize = 16;

/// Maximum number of characters sampled from translatable body text + subject.
pub const MAX_SAMPLE_CHARS: usize = 2_000;

/// Supported lightweight language classifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DetectedLanguage {
    #[serde(rename = "zh-CN")]
    ZhHans,
    #[serde(rename = "zh-TW")]
    ZhHant,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "de")]
    De,
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "pt")]
    Pt,
    #[serde(rename = "it")]
    It,
    #[serde(rename = "nl")]
    Nl,
    #[serde(rename = "ru")]
    Ru,
    #[serde(rename = "uk")]
    Uk,
    #[serde(rename = "ar")]
    Ar,
    #[serde(rename = "th")]
    Th,
    #[serde(rename = "el")]
    El,
    #[serde(rename = "he")]
    He,
    #[serde(rename = "hi")]
    Hi,
    #[serde(rename = "unknown")]
    Unknown,
}

impl DetectedLanguage {
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::ZhHans => "zh-CN",
            Self::ZhHant => "zh-TW",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::En => "en",
            Self::Fr => "fr",
            Self::De => "de",
            Self::Es => "es",
            Self::Pt => "pt",
            Self::It => "it",
            Self::Nl => "nl",
            Self::Ru => "ru",
            Self::Uk => "uk",
            Self::Ar => "ar",
            Self::Th => "th",
            Self::El => "el",
            Self::He => "he",
            Self::Hi => "hi",
            Self::Unknown => "unknown",
        }
    }

    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Normalize a BCP-47 language tag (e.g. `"zh-CN"`, `"zh_TW"`, `"zh"`, `"en-US"`) for matching.
    pub fn matches_target(self, target_tag: &str) -> bool {
        if self.is_unknown() {
            return false;
        }
        let norm = normalize_lang_tag(target_tag);
        if norm.is_empty() {
            return false;
        }
        match self {
            Self::ZhHans => matches!(
                norm.as_str(),
                "zh" | "zh-cn" | "zh-sg" | "zh-hans" | "zh-hans-cn" | "zh-hans-sg"
            ),
            Self::ZhHant => matches!(
                norm.as_str(),
                "zh" | "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" | "zh-hant-tw" | "zh-hant-hk"
            ),
            _ => {
                let primary = norm.split('-').next().unwrap_or("");
                primary == self.as_tag()
            }
        }
    }

    /// Check whether `self` should be suppressed by any entry in a user's `ignored_languages` list.
    ///
    /// Rules:
    /// - `"zh"` ignores both `ZhHans` and `ZhHant`.
    /// - `"zh-CN"` / `"zh-Hans"` ignores `ZhHans` only; `"zh-TW"` / `"zh-Hant"` ignores `ZhHant` only.
    /// - Primary subtag matches regional tags (e.g. `"en"` matches `En`, `"en-US"` matches `En`).
    pub fn is_ignored(self, ignored_languages: &[String]) -> bool {
        if self.is_unknown() {
            return false;
        }
        ignored_languages.iter().any(|tag| self.matches_target(tag))
    }
}

/// Normalize a user or provider language tag into canonical lowercase hyphenated form.
pub fn normalize_lang_tag(raw: &str) -> String {
    raw.trim().replace('_', "-").to_ascii_lowercase()
}

/// Output of lightweight language detection.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectionResult {
    pub language: DetectedLanguage,
    pub confidence: f32,
    pub effective_chars: usize,
}

impl DetectionResult {
    fn unknown(effective_chars: usize) -> Self {
        Self {
            language: DetectedLanguage::Unknown,
            confidence: 0.0,
            effective_chars,
        }
    }
}

/// Detect the primary language of an email using translatable body text (sampled up to
/// [`MAX_SAMPLE_CHARS`]) and optional subject.
pub fn detect_language(body_text: &str, subject: Option<&str>) -> DetectionResult {
    let sampled = extract_clean_sample(body_text, subject, MAX_SAMPLE_CHARS);
    if sampled.diff_dominated {
        return DetectionResult::unknown(sampled.effective_chars);
    }
    if sampled.effective_chars < MIN_EFFECTIVE_CHARS {
        return DetectionResult::unknown(sampled.effective_chars);
    }
    if sampled.is_likely_otp_or_code() {
        return DetectionResult::unknown(sampled.effective_chars);
    }

    classify_sample(&sampled)
}

struct CleanedSample {
    text: String,
    effective_chars: usize,
    total_lines: usize,
    diff_or_code_lines: usize,
    diff_dominated: bool,
    digit_tokens: usize,
    word_tokens: usize,
}

impl CleanedSample {
    fn is_likely_otp_or_code(&self) -> bool {
        if self.word_tokens > 0 && self.word_tokens <= 8 && self.digit_tokens >= 1 {
            let lower = self.text.to_ascii_lowercase();
            if lower.contains("code")
                || lower.contains("otp")
                || lower.contains("pin")
                || lower.contains("verify")
                || lower.contains("verification")
                || lower.contains("passcode")
                || lower.contains("验证码")
                || lower.contains("認証コード")
            {
                return self.effective_chars < 48;
            }
        }
        if self.total_lines >= 4 && self.diff_or_code_lines * 2 >= self.total_lines {
            return true;
        }
        false
    }
}

/// Single-pass noise stripper for email text: strips fenced code blocks, diff headers/hunks,
/// quote headers, URLs, email addresses, HTML tags (defensive), hex/UUIDs, and pure numbers,
/// stopping once `max_chars` effective characters have been collected.
fn extract_clean_sample(body_text: &str, subject: Option<&str>, max_chars: usize) -> CleanedSample {
    let mut out = String::with_capacity(max_chars.min(2048));
    let mut effective_chars = 0usize;
    let mut total_lines = 0usize;
    let mut diff_or_code_lines = 0usize;
    let mut digit_tokens = 0usize;
    let mut word_tokens = 0usize;

    let mut feed_source = |src: &str, is_body: bool| {
        let mut in_fenced_code = false;
        let mut in_diff_block = false;
        for raw_line in src.lines() {
            if effective_chars >= max_chars {
                break;
            }
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_body {
                total_lines += 1;
            }

            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fenced_code = !in_fenced_code;
                if is_body {
                    diff_or_code_lines += 1;
                }
                continue;
            }
            if in_fenced_code {
                if is_body {
                    diff_or_code_lines += 1;
                }
                continue;
            }

            // Detect unified / GitHub diff markers.
            if trimmed.starts_with("diff --git ")
                || trimmed.starts_with("index ")
                || trimmed.starts_with("--- a/")
                || trimmed.starts_with("+++ b/")
                || trimmed.starts_with("@@ ")
                || trimmed.starts_with("commit ")
            {
                in_diff_block = true;
                if is_body {
                    diff_or_code_lines += 1;
                }
                continue;
            }
            if in_diff_block
                && (raw_line.starts_with('+')
                    || raw_line.starts_with('-')
                    || raw_line.starts_with(' ')
                    || raw_line.starts_with('\\'))
            {
                if is_body {
                    diff_or_code_lines += 1;
                }
                continue;
            }
            if is_body && looks_like_code_line(raw_line, trimmed) {
                diff_or_code_lines += 1;
                continue;
            }
            if is_body && looks_like_quote_or_header_line(trimmed) {
                continue;
            }

            // Strip any residual HTML tags defensively (e.g. if HTML-only text wasn't pre-stripped).
            let stripped_tags = strip_html_tags(trimmed);
            let mut appended_any = false;
            for token in stripped_tags.split_whitespace() {
                if effective_chars >= max_chars {
                    break;
                }
                let cleaned_tok = token.trim_matches(|c: char| {
                    matches!(
                        c,
                        '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ',' | ';' | ':'
                    )
                });
                if cleaned_tok.is_empty() {
                    continue;
                }
                if is_digit_or_otp_token(cleaned_tok) {
                    digit_tokens += 1;
                    continue;
                }
                if is_noise_token(cleaned_tok) {
                    continue;
                }
                let tok_eff = cleaned_tok.chars().filter(|c| c.is_alphabetic()).count();
                if tok_eff == 0 {
                    continue;
                }
                word_tokens += 1;
                if appended_any || !out.is_empty() {
                    out.push(' ');
                }
                for ch in cleaned_tok.chars() {
                    if effective_chars >= max_chars {
                        break;
                    }
                    out.push(ch);
                    if ch.is_alphabetic() {
                        effective_chars += 1;
                    }
                }
                appended_any = true;
            }
        }
    };

    // Sample translatable body first; supplement with subject if body is short or empty.
    feed_source(body_text, true);
    if let Some(subj) = subject {
        let cleaned_subj = strip_reply_prefixes(subj);
        feed_source(cleaned_subj, false);
    }

    let diff_dominated = total_lines >= 3 && diff_or_code_lines * 10 >= total_lines * 6;
    CleanedSample {
        text: out,
        effective_chars,
        total_lines,
        diff_or_code_lines,
        diff_dominated,
        digit_tokens,
        word_tokens,
    }
}

fn strip_reply_prefixes(mut s: &str) -> &str {
    loop {
        let trimmed = s.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = ["re:", "fwd:", "fw:", "回复:", "转发:", "答复:"]
            .iter()
            .find_map(|p| {
                if lower.starts_with(p) {
                    Some(&trimmed[p.len()..])
                } else {
                    None
                }
            })
        {
            s = rest;
        } else {
            return trimmed;
        }
    }
}

fn looks_like_quote_or_header_line(trimmed: &str) -> bool {
    if trimmed.starts_with('>') {
        return true;
    }
    if trimmed.starts_with("-----Original Message-----")
        || trimmed.starts_with("---------- Forwarded message")
        || trimmed == "--"
    {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if (lower.starts_with("on ") && lower.ends_with("wrote:"))
        || lower.starts_with("from:")
        || lower.starts_with("sent:")
        || lower.starts_with("to:")
        || lower.starts_with("cc:")
        || lower.starts_with("date:")
        || lower.starts_with("subject:")
        || lower.starts_with("reply-to:")
        || lower.starts_with("message-id:")
    {
        return true;
    }
    false
}

fn looks_like_code_line(raw_line: &str, trimmed: &str) -> bool {
    if (raw_line.starts_with("    ") || raw_line.starts_with('\t'))
        && (trimmed.contains('{')
            || trimmed.contains('}')
            || trimmed.contains("();")
            || trimmed.contains("::")
            || trimmed.contains("=>")
            || trimmed.contains("->"))
    {
        return true;
    }
    if trimmed.starts_with("fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("impl ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("export ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("#include")
        || trimmed.starts_with("at ") && trimmed.contains(".rs:")
    {
        return true;
    }
    // High density of code punctuation relative to letters.
    let mut alpha = 0usize;
    let mut code_punct = 0usize;
    for ch in trimmed.chars() {
        if ch.is_alphabetic() {
            alpha += 1;
        } else if matches!(
            ch,
            '{' | '}' | '[' | ']' | ';' | '=' | '|' | '&' | '^' | '~' | '<' | '>' | '\\' | '/' | '_'
        ) {
            code_punct += 1;
        }
    }
    alpha > 0 && code_punct >= 4 && code_punct * 3 >= alpha
}

fn strip_html_tags(input: &str) -> String {
    if !input.contains('<') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut in_style_or_script = false;
    let mut tag_buf = String::new();

    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag_lower = tag_buf.trim().to_ascii_lowercase();
                if tag_lower.starts_with("style") || tag_lower.starts_with("script") {
                    in_style_or_script = true;
                } else if tag_lower.starts_with("/style") || tag_lower.starts_with("/script") {
                    in_style_or_script = false;
                }
                out.push(' ');
            } else if tag_buf.len() < 16 {
                tag_buf.push(ch);
            }
            continue;
        }
        if !in_style_or_script {
            out.push(ch);
        }
    }
    out
}

fn is_digit_or_otp_token(tok: &str) -> bool {
    let mut digits = 0usize;
    let mut non_sep = 0usize;
    for ch in tok.chars() {
        if matches!(ch, '-' | '_' | '.') {
            continue;
        }
        non_sep += 1;
        if ch.is_ascii_digit() {
            digits += 1;
        }
    }
    non_sep > 0 && digits == non_sep
}

fn is_noise_token(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("www.")
        || lower.starts_with("ftp://")
    {
        return true;
    }
    // Email address.
    if let Some((user, domain)) = tok.split_once('@') {
        if !user.is_empty() && domain.contains('.') {
            return true;
        }
    }
    // File paths, module paths, or code identifiers (`foo_bar`, `crate::x`, `src/main.rs`).
    if tok.contains("::") || tok.contains('/') || tok.contains('\\') || tok.contains('_') {
        return true;
    }
    // Hex hash / UUID (>= 7 hex chars with digits).
    if tok.len() >= 7
        && tok.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && tok.chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }
    // CamelCase identifier like `TranslationProvider` or `getProxyConfig`.
    if tok.is_ascii() && tok.len() >= 5 {
        let bytes = tok.as_bytes();
        let has_internal_upper = bytes[1..].iter().any(|b| b.is_ascii_uppercase());
        let has_lower = bytes.iter().any(|b| b.is_ascii_lowercase());
        if has_internal_upper && has_lower {
            return true;
        }
    }
    // Dotted domain or property chain like `api.openai.com` or `config.base_url`.
    if tok.contains('.') && tok.is_ascii() {
        let parts: Vec<&str> = tok.split('.').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
            return true;
        }
    }
    false
}

#[derive(Default)]
struct ScriptCounts {
    han: usize,
    kana: usize,
    hangul: usize,
    latin: usize,
    cyrillic: usize,
    arabic: usize,
    thai: usize,
    greek: usize,
    hebrew: usize,
    devanagari: usize,
    other: usize,
}

impl ScriptCounts {
    fn total(&self) -> usize {
        self.han
            + self.kana
            + self.hangul
            + self.latin
            + self.cyrillic
            + self.arabic
            + self.thai
            + self.greek
            + self.hebrew
            + self.devanagari
            + self.other
    }
}

fn classify_sample(sample: &CleanedSample) -> DetectionResult {
    let mut counts = ScriptCounts::default();
    for ch in sample.text.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        let cp = ch as u32;
        if is_kana(cp) {
            counts.kana += 1;
        } else if is_han(cp) {
            counts.han += 1;
        } else if is_hangul(cp) {
            counts.hangul += 1;
        } else if is_latin(ch) {
            counts.latin += 1;
        } else if is_cyrillic(cp) {
            counts.cyrillic += 1;
        } else if is_arabic(cp) {
            counts.arabic += 1;
        } else if (0x0E00..=0x0E7F).contains(&cp) {
            counts.thai += 1;
        } else if (0x0370..=0x03FF).contains(&cp) || (0x1F00..=0x1FFF).contains(&cp) {
            counts.greek += 1;
        } else if (0x0590..=0x05FF).contains(&cp) {
            counts.hebrew += 1;
        } else if (0x0900..=0x097F).contains(&cp) {
            counts.devanagari += 1;
        } else {
            counts.other += 1;
        }
    }

    let total = counts.total();
    if total < MIN_EFFECTIVE_CHARS {
        return DetectionResult::unknown(total);
    }

    // Weighted totals: CJK/Kana/Hangul characters carry roughly 2x information density vs Latin letters.
    let cjk_total = counts.han + counts.kana;
    let cjk_weight = cjk_total * 2;
    let latin_weight = counts.latin;

    // 1. Japanese with Kana (Hiragana/Katakana) present.
    if counts.kana >= 4 && (cjk_total * 100 / total) >= 25 {
        // Mixed JA + substantial English prose? Fail quiet if balanced.
        if latin_weight > 0 && is_ambiguous_mixed_cjk_latin(cjk_weight, latin_weight, &sample.text) {
            return DetectionResult::unknown(total);
        }
        let conf = ((counts.kana + counts.han) as f32 / total as f32).clamp(0.65, 0.99);
        return DetectionResult {
            language: DetectedLanguage::Ja,
            confidence: conf,
            effective_chars: total,
        };
    }

    // 2. Korean (Hangul).
    if counts.hangul >= 8 && counts.hangul * 100 / total >= 40 {
        let conf = (counts.hangul as f32 / total as f32).clamp(0.7, 0.99);
        return DetectionResult {
            language: DetectedLanguage::Ko,
            confidence: conf,
            effective_chars: total,
        };
    }

    // 3. Han-only (Chinese vs kanji-only Japanese ambiguity vs mixed CJK/English).
    if counts.han >= 8 && counts.kana == 0 && (counts.han * 2) * 100 / (counts.han * 2 + counts.latin + 1) >= 35 {
        if latin_weight > 0 && is_ambiguous_mixed_cjk_latin(counts.han * 2, latin_weight, &sample.text) {
            return DetectionResult::unknown(total);
        }
        return classify_han(&sample.text, total);
    }

    // 4. Cyrillic (distinguish Russian vs Ukrainian vs other Cyrillic languages).
    if counts.cyrillic >= 16 && counts.cyrillic * 100 / total >= 60 {
        return classify_cyrillic(&sample.text, counts.cyrillic, total);
    }

    // 5. Arabic script (guard against Persian/Urdu).
    if counts.arabic >= 16 && counts.arabic * 100 / total >= 60 {
        return classify_arabic(&sample.text, counts.arabic, total);
    }

    // 6. Single-language scripts: Thai, Greek, Hebrew, Devanagari.
    if counts.thai >= 16 && counts.thai * 100 / total >= 60 {
        return DetectionResult {
            language: DetectedLanguage::Th,
            confidence: (counts.thai as f32 / total as f32).clamp(0.75, 0.99),
            effective_chars: total,
        };
    }
    if counts.greek >= 16 && counts.greek * 100 / total >= 60 {
        return DetectionResult {
            language: DetectedLanguage::El,
            confidence: (counts.greek as f32 / total as f32).clamp(0.75, 0.99),
            effective_chars: total,
        };
    }
    if counts.hebrew >= 16 && counts.hebrew * 100 / total >= 60 {
        return DetectionResult {
            language: DetectedLanguage::He,
            confidence: (counts.hebrew as f32 / total as f32).clamp(0.75, 0.99),
            effective_chars: total,
        };
    }
    if counts.devanagari >= 16 && counts.devanagari * 100 / total >= 60 {
        return classify_devanagari(&sample.text, counts.devanagari, total);
    }

    // 7. Latin languages (en, fr, de, es, pt, it, nl).
    if counts.latin >= MIN_EFFECTIVE_CHARS && counts.latin * 100 / total >= 70 {
        // Guard against mixed CJK/English when Han characters are also significant.
        if counts.han >= 6 && is_ambiguous_mixed_cjk_latin(counts.han * 2, latin_weight, &sample.text)
        {
            return DetectionResult::unknown(total);
        }
        return classify_latin(&sample.text, total);
    }

    DetectionResult::unknown(total)
}

/// Detect balanced mixed CJK + English prose where neither language is clearly dominant.
fn is_ambiguous_mixed_cjk_latin(cjk_weight: usize, latin_weight: usize, text: &str) -> bool {
    let combined = cjk_weight + latin_weight;
    if combined == 0 {
        return false;
    }
    let cjk_pct = cjk_weight * 100 / combined;
    // If CJK accounts for 10%..=72% of weighted text AND there are multiple English stopwords
    // (real English sentences, not just product names like "GitHub" or "API"), treat as mixed/Unknown.
    if (10..=72).contains(&cjk_pct) {
        let en_stop_hits = count_latin_stopwords(text, EN_STOPWORDS);
        if en_stop_hits >= 3 {
            return true;
        }
    }
    false
}

fn classify_han(text: &str, total: usize) -> DetectionResult {
    let mut simp_hits = 0usize;
    let mut trad_hits = 0usize;
    let mut zh_func_hits = 0usize;
    let mut ja_kanji_hits = 0usize;

    for ch in text.chars() {
        if SIMP_ONLY_CHARS.contains(&ch) {
            simp_hits += 1;
        }
        if TRAD_ONLY_CHARS.contains(&ch) {
            trad_hits += 1;
        }
        if ZH_FUNCTION_CHARS.contains(&ch) {
            zh_func_hits += 1;
        }
        if JA_SPECIFIC_KANJI.contains(&ch) {
            ja_kanji_hits += 1;
        }
    }

    // Kanji-only Japanese ambiguity guard:
    // 1. If Japanese-specific Shinjitai/kokuji appear without strong Chinese grammatical particles,
    //    fail quiet (`Unknown`).
    // 2. If there are NO Chinese grammatical particles at all (e.g., pure Sino-Japanese noun
    //    compounds like "会議日程変更通知書"), fail quiet (`Unknown`) rather than guessing Chinese.
    if ja_kanji_hits > 0 && zh_func_hits < 2 {
        return DetectionResult::unknown(total);
    }
    if zh_func_hits == 0 {
        return DetectionResult::unknown(total);
    }

    let lang = if trad_hits > simp_hits {
        DetectedLanguage::ZhHant
    } else if simp_hits > 0 || zh_func_hits > 0 {
        DetectedLanguage::ZhHans
    } else {
        return DetectionResult::unknown(total);
    };

    let conf = (0.65 + (zh_func_hits + simp_hits + trad_hits).min(10) as f32 * 0.03).min(0.98);
    DetectionResult {
        language: lang,
        confidence: conf,
        effective_chars: total,
    }
}

fn classify_cyrillic(text: &str, cyrillic: usize, total: usize) -> DetectionResult {
    let mut uk_chars = 0usize;
    let mut non_ru_uk_chars = 0usize;
    let mut ru_specific_chars = 0usize;

    for ch in text.chars() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        match lower {
            'ї' | 'є' | 'ґ' | 'і' => uk_chars += 1,
            'ы' | 'э' | 'ё' | 'ъ' => ru_specific_chars += 1,
            // Serbian, Macedonian, Belarusian, Kazakh, Bulgarian/other Cyrillic markers:
            'љ' | 'њ' | 'ћ' | 'ђ' | 'џ' | 'ј' | 'ў' | 'қ' | 'ң' | 'ғ' | 'ү' | 'ұ' | 'һ' | 'ө'
            | 'ә' => non_ru_uk_chars += 1,
            _ => {}
        }
    }

    if non_ru_uk_chars > 0 {
        return DetectionResult::unknown(total);
    }

    let words: Vec<String> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();

    let uk_stops = words.iter().filter(|w| UK_STOPWORDS.contains(&w.as_str())).count();
    let ru_stops = words.iter().filter(|w| RU_STOPWORDS.contains(&w.as_str())).count();

    if uk_chars > 0 && (uk_stops >= 1 || uk_chars >= 2) && uk_stops >= ru_stops {
        return DetectionResult {
            language: DetectedLanguage::Uk,
            confidence: (cyrillic as f32 / total as f32).clamp(0.75, 0.98),
            effective_chars: total,
        };
    }

    // Require positive Russian stopword evidence (not just any Cyrillic text).
    if ru_stops >= 2 || (ru_stops >= 1 && ru_specific_chars >= 2) {
        return DetectionResult {
            language: DetectedLanguage::Ru,
            confidence: (cyrillic as f32 / total as f32).clamp(0.75, 0.98),
            effective_chars: total,
        };
    }

    DetectionResult::unknown(total)
}

fn classify_arabic(text: &str, arabic: usize, total: usize) -> DetectionResult {
    // Reject Persian / Urdu-specific characters so we don't mislabel them as Arabic.
    if text
        .chars()
        .any(|c| matches!(c, 'پ' | 'چ' | 'ژ' | 'گ' | 'ک' | 'ی' | 'ٹ' | 'ڈ' | 'ڑ' | 'ے' | 'ں'))
    {
        return DetectionResult::unknown(total);
    }
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    let ar_stops = words
        .iter()
        .filter(|w| AR_STOPWORDS.contains(w) || w.starts_with("ال"))
        .count();
    if ar_stops >= 2 {
        DetectionResult {
            language: DetectedLanguage::Ar,
            confidence: (arabic as f32 / total as f32).clamp(0.75, 0.98),
            effective_chars: total,
        }
    } else {
        DetectionResult::unknown(total)
    }
}

fn classify_devanagari(text: &str, devanagari: usize, total: usize) -> DetectionResult {
    let hi_stops = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| HI_STOPWORDS.contains(w))
        .count();
    if hi_stops >= 2 {
        DetectionResult {
            language: DetectedLanguage::Hi,
            confidence: (devanagari as f32 / total as f32).clamp(0.75, 0.98),
            effective_chars: total,
        }
    } else {
        DetectionResult::unknown(total)
    }
}

fn classify_latin(text: &str, total: usize) -> DetectionResult {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();

    if words.len() < 3 {
        return DetectionResult::unknown(total);
    }

    let candidates: [(DetectedLanguage, &[&str]); 7] = [
        (DetectedLanguage::En, EN_STOPWORDS),
        (DetectedLanguage::Fr, FR_STOPWORDS),
        (DetectedLanguage::De, DE_STOPWORDS),
        (DetectedLanguage::Es, ES_STOPWORDS),
        (DetectedLanguage::Pt, PT_STOPWORDS),
        (DetectedLanguage::It, IT_STOPWORDS),
        (DetectedLanguage::Nl, NL_STOPWORDS),
    ];

    let mut scores: [(DetectedLanguage, u32, u32); 7] = [
        (DetectedLanguage::En, 0, 0),
        (DetectedLanguage::Fr, 0, 0),
        (DetectedLanguage::De, 0, 0),
        (DetectedLanguage::Es, 0, 0),
        (DetectedLanguage::Pt, 0, 0),
        (DetectedLanguage::It, 0, 0),
        (DetectedLanguage::Nl, 0, 0),
    ];

    for (idx, (_, stops)) in candidates.iter().enumerate() {
        let mut stop_hits = 0u32;
        for w in &words {
            if stops.contains(&w.as_str()) {
                stop_hits += 1;
            }
        }
        scores[idx].1 = stop_hits * 4;
        scores[idx].2 = stop_hits;
    }

    // Diacritic and distinctive character bonuses.
    for ch in text.chars() {
        match ch.to_lowercase().next().unwrap_or(ch) {
            'ß' | 'ä' | 'ö' | 'ü' => scores[2].1 += 3,
            'ñ' | '¿' | '¡' => scores[3].1 += 4,
            'ã' | 'õ' => scores[4].1 += 4,
            'ç' => {
                scores[1].1 += 2;
                scores[4].1 += 2;
            }
            'œ' | 'ê' | 'â' | 'î' | 'û' | 'è' => scores[1].1 += 2,
            'ò' | 'ì' | 'ù' => scores[5].1 += 2,
            _ => {}
        }
    }

    // Sort descending by score.
    scores.sort_by_key(|b| std::cmp::Reverse(b.1));
    let (best_lang, best_score, best_stops) = scores[0];
    let (_, second_score, _) = scores[1];

    // Require enough stopword hits and a clear margin over the runner-up.
    if best_stops < 2 || best_score < 8 {
        return DetectionResult::unknown(total);
    }
    if second_score > 0 && best_score * 10 < second_score * 13 {
        return DetectionResult::unknown(total);
    }

    let ratio = best_stops as f32 / words.len().max(1) as f32;
    let conf = (0.65 + ratio * 0.5).clamp(0.65, 0.99);
    DetectionResult {
        language: best_lang,
        confidence: conf,
        effective_chars: total,
    }
}

fn count_latin_stopwords(text: &str, stops: &[&str]) -> usize {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .filter(|w| stops.contains(&w.to_ascii_lowercase().as_str()))
        .count()
}

fn is_kana(cp: u32) -> bool {
    (0x3040..=0x309F).contains(&cp)
        || (0x30A0..=0x30FF).contains(&cp)
        || (0x31F0..=0x31FF).contains(&cp)
        || (0xFF65..=0xFF9F).contains(&cp)
}

fn is_han(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0x20000..=0x2A6DF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
}

fn is_hangul(cp: u32) -> bool {
    (0xAC00..=0xD7AF).contains(&cp)
        || (0x1100..=0x11FF).contains(&cp)
        || (0x3130..=0x318F).contains(&cp)
}

fn is_latin(ch: char) -> bool {
    let cp = ch as u32;
    ch.is_ascii_alphabetic()
        || (0x00C0..=0x00FF).contains(&cp) && cp != 0x00D7 && cp != 0x00F7
        || (0x0100..=0x024F).contains(&cp)
}

fn is_cyrillic(cp: u32) -> bool {
    (0x0400..=0x04FF).contains(&cp) || (0x0500..=0x052F).contains(&cp)
}

fn is_arabic(cp: u32) -> bool {
    (0x0600..=0x06FF).contains(&cp) || (0x0750..=0x077F).contains(&cp)
}

/// Uniquely Chinese grammatical particles and pronouns rarely or never appearing in Japanese
/// kanji-only compounds.
const ZH_FUNCTION_CHARS: &[char] = &[
    '的', '了', '是', '我', '这', '這', '们', '們', '个', '個', '请', '請', '您', '谢', '謝', '把',
    '让', '讓', '给', '給', '还', '還', '没', '沒', '很', '也', '就', '吗', '嗎', '呢', '吧', '啊',
    '着', '过', '過', '里', '裡', '么', '麼', '谁', '誰', '哪', '那', '您', '将', '將', '经', '經',
];

const SIMP_ONLY_CHARS: &[char] = &[
    '这', '个', '们', '为', '发', '会', '时', '国', '说', '请', '进', '经', '对', '开', '关', '问',
    '题', '体', '机', '电', '视', '网', '页', '邮', '账', '号', '码', '点', '击', '链', '确', '认',
    '谢', '欢', '统', '误', '处', '当', '应', '该', '显', '译', '语', '设', '检', '测', '缓', '无',
    '从', '与', '让', '给', '还', '没', '过', '来', '后', '里', '么', '样', '实', '业', '务', '资',
    '内', '删', '选', '择', '输', '报', '计', '划', '项', '团', '队', '简', '录', '访', '复', '件',
    '单', '订', '购', '货', '运', '费', '银', '证', '签', '帮', '助', '联', '系', '客', '户',
];

const TRAD_ONLY_CHARS: &[char] = &[
    '這', '個', '們', '為', '發', '會', '時', '國', '說', '請', '進', '經', '對', '開', '關', '問',
    '題', '體', '機', '電', '視', '網', '頁', '郵', '帳', '號', '碼', '點', '擊', '鏈', '確', '認',
    '謝', '歡', '統', '誤', '處', '當', '應', '該', '顯', '譯', '語', '設', '檢', '測', '緩', '無',
    '從', '與', '讓', '給', '還', '沒', '過', '來', '後', '裡', '麼', '樣', '實', '業', '務', '資',
    '內', '刪', '選', '擇', '輸', '報', '計', '劃', '項', '團', '隊', '繁', '錄', '訪', '復', '單',
    '訂', '購', '貨', '運', '費', '銀', '證', '簽', '幫', '聯', '係', '臺', '灣',
];

/// Japanese Shinjitai / kokuji characters rarely or never used in standard Chinese prose.
const JA_SPECIFIC_KANJI: &[char] = &[
    '込', '働', '畑', '辻', '枠', '栃', '匂', '腺', '雫', '駅', '図', '広', '転', '売', '読', '変',
    '払', '届', '済', '円', '桜', '剣', '訳', '薬', '黒', '歩', '歳', '毎', '昼', '乗', '仮', '価',
    '発', '渋', '仏', '児', '剰', '労', '効', '勅', '励', '勧', '巻', '圧', '塁', '壊', '壌', '壱',
    '壮', '声', '奨', '嬢', '学', '宝', '実', '寛', '専', '対', '寿', '将', '尭', '尽', '属', '層',
    '岩', '岳', '峡', '島', '巣', '帯', '帰', '庁', '廃', '弁', '弐', '弥', '弾', '当', '径', '従',
    '徳', '徴', '応', '恋', '恒', '恵', '悩', '悪', '惨', '慎', '戦', '戯', '戻', '抜', '択', '担',
    '拠', '拡', '挙', '挟', '挿', '捜', '掲', '揺', '摂', '撃', '撮', '撲',
];

const EN_STOPWORDS: &[&str] = &[
    "the", "and", "that", "for", "with", "this", "from", "have", "your", "are", "was", "were",
    "will", "would", "could", "should", "been", "being", "about", "into", "please", "thank",
    "thanks", "hello", "dear", "best", "regards", "you", "our", "their", "they", "them", "what",
    "when", "where", "which", "who", "how", "can", "not", "but", "all", "any", "has", "had",
    "more", "some", "there", "here", "just", "only", "also", "very", "email", "message", "account",
];

const FR_STOPWORDS: &[&str] = &[
    "le", "la", "les", "des", "une", "un", "est", "sont", "pour", "avec", "dans", "sur", "que",
    "qui", "nous", "vous", "votre", "notre", "mais", "pas", "plus", "tout", "tous", "cette",
    "ces", "aux", "par", "comme", "être", "avoir", "faire", "bonjour", "merci", "cordialement",
    "bien", "aussi", "très", "sans", "chez", "entre", "après", "avant", "depuis", "toujours",
    "compte", "message", "veuillez",
];

const DE_STOPWORDS: &[&str] = &[
    "der", "die", "das", "und", "ist", "sind", "ein", "eine", "einen", "einem", "einer", "nicht",
    "mit", "auf", "für", "von", "den", "dem", "des", "sich", "auch", "noch", "wie", "aber",
    "oder", "wenn", "wir", "ihr", "ihre", "ihren", "ihrem", "uns", "unser", "bitte", "danke",
    "hallo", "sehr", "geehrte", "grüße", "freundlichen", "haben", "wird", "werden", "kann",
    "können", "über", "nach", "bei", "aus", "zum", "zur", "dieser", "diese", "dieses",
];

const ES_STOPWORDS: &[&str] = &[
    "el", "la", "los", "las", "un", "una", "unos", "unas", "que", "por", "para", "con", "del",
    "como", "pero", "más", "este", "esta", "estos", "estas", "todo", "todos", "sobre", "entre",
    "cuando", "donde", "también", "puede", "pueden", "hola", "gracias", "saludos", "favor",
    "cuenta", "mensaje", "nuestro", "nuestra", "su", "sus", "nos", "son", "está", "están",
    "tiene", "tienen", "desde", "hasta", "sin", "muy", "porque",
];

const PT_STOPWORDS: &[&str] = &[
    "os", "as", "um", "uma", "uns", "umas", "que", "por", "para", "com", "dos", "das", "como",
    "mas", "mais", "este", "esta", "estes", "estas", "isso", "isto", "tudo", "todos", "sobre",
    "entre", "quando", "onde", "também", "pode", "podem", "olá", "obrigado", "obrigada", "favor",
    "conta", "mensagem", "nosso", "nossa", "você", "vocês", "são", "está", "estão", "tem", "têm",
    "não", "pela", "pelo", "muito", "sem", "até", "aqui", "seu", "sua", "seus", "suas",
];

const IT_STOPWORDS: &[&str] = &[
    "il", "lo", "la", "gli", "le", "un", "uno", "una", "che", "per", "con", "del", "della",
    "delle", "degli", "dei", "nel", "nella", "nelle", "sul", "sulla", "come", "anche", "questo",
    "questa", "questi", "queste", "tutto", "tutti", "sono", "siamo", "essere", "avere", "ciao",
    "salve", "gentile", "grazie", "cordiali", "saluti", "non", "più", "molto", "senza", "dopo",
    "prima", "sempre", "vostro", "nostro", "messaggio", "account", "suo", "sua",
];

const NL_STOPWORDS: &[&str] = &[
    "de", "het", "een", "van", "en", "in", "op", "te", "dat", "die", "voor", "met", "zijn",
    "niet", "aan", "ook", "als", "maar", "om", "dan", "nog", "wel", "uit", "bij", "naar", "over",
    "tot", "door", "heeft", "hebben", "worden", "wordt", "kan", "kunnen", "wij", "ons", "onze",
    "uw", "jullie", "deze", "dit", "hallo", "beste", "bedankt", "groeten", "vriendelijke",
    "bericht", "graag", "meer", "geen",
];

const RU_STOPWORDS: &[&str] = &[
    "и", "в", "не", "на", "что", "с", "по", "это", "как", "для", "вы", "мы", "он", "она", "они",
    "но", "или", "из", "от", "до", "при", "так", "все", "уже", "если", "быть", "может", "ваш",
    "вашего", "пожалуйста", "спасибо", "здравствуйте", "привет", "уважением", "письмо",
];

const UK_STOPWORDS: &[&str] = &[
    "і", "та", "що", "не", "на", "це", "як", "для", "ви", "ми", "він", "вона", "вони", "але",
    "або", "від", "до", "при", "так", "всі", "вже", "якщо", "бути", "може", "ваш", "вашого",
    "будь", "ласка", "дякую", "вітаю", "привіт", "повагою", "лист", "є", "їх", "цього",
];

const AR_STOPWORDS: &[&str] = &[
    "في", "من", "على", "إلى", "الى", "عن", "مع", "هذا", "هذه", "التي", "الذي", "أن", "ان", "لا",
    "ما", "هو", "هي", "نحن", "أنت", "انتم", "تم", "يرجى", "شكرا", "مرحبا", "تحياتي", "رسالة",
    "حساب", "الرجاء", "كل", "بعد", "قبل",
];

const HI_STOPWORDS: &[&str] = &[
    "है", "हैं", "के", "की", "को", "में", "से", "पर", "और", "यह", "वह", "एक", "नहीं", "कि", "तो",
    "ही", "भी", "आप", "हम", "इस", "उस", "लिए", "साथ", "किया", "करने", "कृपया", "धन्यवाद", "नमस्ते",
];
