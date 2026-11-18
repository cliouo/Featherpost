//! drafts：daemon 拥有的持久草稿（ADR-0034）。
//!
//! 这里是草稿的领域层——形状、上限与校验。持久化在 [`crate::store`]，RPC 在 [`crate::api`]。
//!
//! 两条贯穿本模块的规则：
//!
//! 1. **草稿是字段，不是报文**（决策 1）。`Date` 与 `Message-ID` 必须在投递那一刻生成，
//!    烤进草稿会让周一起草、周五发出的信带着周一的日期。
//! 2. **超限一律拒绝，绝不截断**（决策 5）。正文是用户刚敲进去的字，没有第二份副本；
//!    悄悄砍掉一半比拒绝保存恶劣得多。

use serde::{Deserialize, Serialize};

/// 纯文本正文上限。与 `body_html` 各一份，两者相加仍在 1 MiB 帧内留有充裕余量。
pub const MAX_DRAFT_BODY_BYTES: usize = 256 * 1024;
/// HTML 正文上限。
pub const MAX_DRAFT_HTML_BYTES: usize = 256 * 1024;
/// 主题上限。长主题在发信时会被 RFC 2047 折行，这里只防离谱输入。
pub const MAX_DRAFT_SUBJECT_BYTES: usize = 2 * 1024;
/// 单条收件人输入的上限（RFC 5321 的 path 上限是 256；留出显示名的余量）。
pub const MAX_DRAFT_ADDRESS_BYTES: usize = 320;
/// 发件地址上限。
pub const MAX_DRAFT_FROM_BYTES: usize = MAX_DRAFT_ADDRESS_BYTES;
/// `to + cc + bcc` 的总数上限，与 [`crate::compose::MAX_RECIPIENTS`] 同值。
pub const MAX_DRAFT_RECIPIENTS: usize = crate::compose::MAX_RECIPIENTS;
/// 单份草稿的附件引用条数上限。
pub const MAX_DRAFT_ATTACHMENTS: usize = 50;
/// 单个账户的草稿份数上限。到顶时拒绝新建——**不淘汰最旧的**：
/// 用户写了一半的信不该被我们替他删掉（决策 5 的同一条理由）。
pub const MAX_DRAFTS_PER_ACCOUNT: i64 = 200;
/// `in_reply_to` 上限（单个 Message-ID）。
pub const MAX_DRAFT_IN_REPLY_TO_BYTES: usize = 998;
/// `references` 上限，与 [`crate::compose::MAX_REFERENCES_BYTES`] 同值。
pub const MAX_DRAFT_REFERENCES_BYTES: usize = crate::compose::MAX_REFERENCES_BYTES;

/// 生成草稿行 id（对齐账户/地址行 id 形状：`draft_` + 26 位 base32）。
pub fn generate_draft_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut random = [0u8; 26];
    getrandom::fill(&mut random).expect("系统随机源不可用");
    let mut id = String::with_capacity(32);
    id.push_str("draft_");
    for byte in random {
        id.push(ALPHABET[(byte & 31) as usize] as char);
    }
    id
}

/// 草稿的一条附件引用：指向库里已有的某封信的某个 part，**字节不复制**（决策 6）。
///
/// `filename` / `mime_type` / `size` 是写入时的快照，只为显示；发信读的是引用本身。
/// `stored` 是**读取时**现查的当前状态——源邮件可能在草稿写完之后被删掉。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftAttachmentRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    pub account: String,
    pub folder: String,
    pub uid: String,
    pub part_index: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size: u64,
    /// 当前源 part 是否仍然可读（元数据齐全且字节已落库）。写入时恒为真。
    #[serde(default)]
    pub stored: bool,
}

/// 一份完整草稿。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct Draft {
    #[serde(default)]
    pub send_state: String,
    #[serde(default)]
    pub sent_archive: Option<bool>,
    pub id: String,
    pub account: String,
    /// 空 = 用账户主地址，发信时解析（别名可能在起草之后才被停用）。
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub body_html: String,
    #[serde(default)]
    pub in_reply_to: String,
    #[serde(default)]
    pub references: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachmentRef>,
    /// 并发仲裁（决策 3）：创建时为 1，每次成功更新 +1。
    pub version: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// 列表项。正文不进列表——`listDrafts` 要能一帧装下一页。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftSummary {
    pub id: String,
    pub account: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub attachment_count: u32,
    pub version: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// `createDraft` / `updateDraft` 的字段补丁（决策 4）。
///
/// `None` = 该字段不动；`Some` = 整体替换。清空正文是 `Some("")` 而不是省略。
/// `attachments` 出现即替换整张列表，顺序即 `position`。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<Vec<String>>,
    #[serde(default)]
    pub cc: Option<Vec<String>>,
    #[serde(default)]
    pub bcc: Option<Vec<String>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_html: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<DraftAttachmentInput>>,
}

/// 写入侧的附件引用：只有四元组是必需的，显示用的三项由 daemon 在校验时从库里取。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftAttachmentInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    /// 缺省 = 草稿自己的账户。跨账户转发要显式写出来。
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub part_index: u32,
}

impl DraftPatch {
    /// 补丁是否一个字段都没带。空补丁不是错误（它是一次幂等的「确认还在」），
    /// 但调用方需要知道以免白白推进版本号。
    pub fn is_empty(&self) -> bool {
        self.account.is_none()
            && self.from.is_none()
            && self.to.is_none()
            && self.cc.is_none()
            && self.bcc.is_none()
            && self.subject.is_none()
            && self.body.is_none()
            && self.body_html.is_none()
            && self.in_reply_to.is_none()
            && self.references.is_none()
            && self.attachments.is_none()
    }

    /// 把补丁应用到一份草稿上（不含附件——附件需要查库校验，由 store 层处理）。
    pub fn apply_fields(&self, draft: &mut Draft) {
        if let Some(value) = &self.account {
            draft.account = value.clone();
        }
        if let Some(value) = &self.from {
            draft.from = value.clone();
        }
        if let Some(value) = &self.to {
            draft.to = value.clone();
        }
        if let Some(value) = &self.cc {
            draft.cc = value.clone();
        }
        if let Some(value) = &self.bcc {
            draft.bcc = value.clone();
        }
        if let Some(value) = &self.subject {
            draft.subject = value.clone();
        }
        if let Some(value) = &self.body {
            draft.body = value.clone();
        }
        if let Some(value) = &self.body_html {
            draft.body_html = value.clone();
        }
        if let Some(value) = &self.in_reply_to {
            draft.in_reply_to = value.clone();
        }
        if let Some(value) = &self.references {
            draft.references = value.clone();
        }
    }
}

/// 一份草稿在写入之前没通过的那条规则。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DraftInvalid {
    /// 头部字段里出现 CR/LF/NUL 等控制字符。头注入的最早一道门：
    /// 用户不会在收件人框里敲回车，而组装时再拦已经晚了一站。
    ControlCharacter { field: &'static str },
    /// 超过字节上限。
    TooLarge { field: &'static str, limit: usize },
    /// 超过条数上限。
    TooMany { field: &'static str, limit: usize },
}

impl DraftInvalid {
    pub fn field(&self) -> &'static str {
        match self {
            Self::ControlCharacter { field }
            | Self::TooLarge { field, .. }
            | Self::TooMany { field, .. } => field,
        }
    }

    pub fn limit(&self) -> Option<usize> {
        match self {
            Self::ControlCharacter { .. } => None,
            Self::TooLarge { limit, .. } | Self::TooMany { limit, .. } => Some(*limit),
        }
    }
}

impl std::fmt::Display for DraftInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlCharacter { field } => {
                write!(f, "`{field}` 不能包含换行或控制字符")
            }
            Self::TooLarge { field, limit } => {
                write!(f, "`{field}` 超过上限 {limit} 字节")
            }
            Self::TooMany { field, limit } => write!(f, "`{field}` 超过上限 {limit} 条"),
        }
    }
}

/// 头部类字段只允许可打印字符与 Tab：CR/LF 会让一个收件人变成一行新头部。
fn reject_control_characters(value: &str, field: &'static str) -> Result<(), DraftInvalid> {
    if value
        .chars()
        .any(|ch| (ch.is_control() && ch != '\t') || ch == '\u{feff}')
    {
        return Err(DraftInvalid::ControlCharacter { field });
    }
    Ok(())
}

fn check_len(value: &str, field: &'static str, limit: usize) -> Result<(), DraftInvalid> {
    if value.len() > limit {
        return Err(DraftInvalid::TooLarge { field, limit });
    }
    Ok(())
}

/// 校验一份**补丁**：只看它带来的那些字段，加上收件人总数这条跨字段的约束。
///
/// 刻意不校验地址形状：草稿必须能装下「用户正打字打到一半」的收件人（`alice@`）。
/// 真正的 addr-spec 校验在投递那一刻（ADR-0028 决策 2 的逐项校验），那里拦得住同样的东西。
pub fn validate_patch(patch: &DraftPatch, current: &Draft) -> Result<(), DraftInvalid> {
    if let Some(account) = &patch.account {
        reject_control_characters(account, "account")?;
        check_len(account, "account", 128)?;
    }
    if let Some(from) = &patch.from {
        reject_control_characters(from, "from")?;
        check_len(from, "from", MAX_DRAFT_FROM_BYTES)?;
    }
    for (field, values) in [
        ("to", &patch.to),
        ("cc", &patch.cc),
        ("bcc", &patch.bcc),
    ] {
        let Some(values) = values else { continue };
        for value in values {
            reject_control_characters(value, field)?;
            check_len(value, field, MAX_DRAFT_ADDRESS_BYTES)?;
        }
    }
    // 收件人总数跨字段：只补 cc 时也要把既有的 to/bcc 算进去，否则分两次就能绕过上限。
    let count = |patched: &Option<Vec<String>>, existing: &[String]| {
        patched.as_ref().map_or(existing.len(), Vec::len)
    };
    let recipients = count(&patch.to, &current.to)
        + count(&patch.cc, &current.cc)
        + count(&patch.bcc, &current.bcc);
    if recipients > MAX_DRAFT_RECIPIENTS {
        return Err(DraftInvalid::TooMany {
            field: "recipients",
            limit: MAX_DRAFT_RECIPIENTS,
        });
    }
    if let Some(subject) = &patch.subject {
        reject_control_characters(subject, "subject")?;
        check_len(subject, "subject", MAX_DRAFT_SUBJECT_BYTES)?;
    }
    if let Some(body) = &patch.body {
        check_len(body, "body", MAX_DRAFT_BODY_BYTES)?;
    }
    if let Some(html) = &patch.body_html {
        check_len(html, "body_html", MAX_DRAFT_HTML_BYTES)?;
    }
    if let Some(value) = &patch.in_reply_to {
        reject_control_characters(value, "in_reply_to")?;
        check_len(value, "in_reply_to", MAX_DRAFT_IN_REPLY_TO_BYTES)?;
    }
    if let Some(value) = &patch.references {
        reject_control_characters(value, "references")?;
        check_len(value, "references", MAX_DRAFT_REFERENCES_BYTES)?;
    }
    if let Some(attachments) = &patch.attachments {
        if attachments.len() > MAX_DRAFT_ATTACHMENTS {
            return Err(DraftInvalid::TooMany {
                field: "attachments",
                limit: MAX_DRAFT_ATTACHMENTS,
            });
        }
    }
    Ok(())
}

/// 列表项的摘要文本：正文压成单行短句（与邮件摘要同一个函数，措辞一致）。
pub fn snippet_of(draft: &Draft) -> String {
    crate::mime::snippet_of(&draft.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_with(to: Vec<&str>) -> Draft {
        Draft {
            to: to.into_iter().map(str::to_string).collect(),
            ..Draft::default()
        }
    }

    #[test]
    fn generated_ids_have_the_house_shape_and_do_not_repeat() {
        let first = generate_draft_id();
        assert!(first.starts_with("draft_"));
        assert_eq!(first.len(), 32);
        assert!(first[6..].chars().all(|ch| ch.is_ascii_lowercase() || ('2'..='7').contains(&ch)));
        assert_ne!(first, generate_draft_id());
    }

    #[test]
    fn half_typed_recipients_are_accepted_but_newlines_are_not() {
        // 草稿必须装得下打到一半的地址，否则自动保存会在用户打字时报错。
        let patch = DraftPatch {
            to: Some(vec!["alice@".into(), "".into()]),
            ..DraftPatch::default()
        };
        assert_eq!(validate_patch(&patch, &Draft::default()), Ok(()));

        let injected = DraftPatch {
            to: Some(vec!["a@b.example\r\nBcc: evil@x.example".into()]),
            ..DraftPatch::default()
        };
        assert_eq!(
            validate_patch(&injected, &Draft::default()),
            Err(DraftInvalid::ControlCharacter { field: "to" })
        );
        let subject = DraftPatch {
            subject: Some("行一\n行二".into()),
            ..DraftPatch::default()
        };
        assert_eq!(
            validate_patch(&subject, &Draft::default()),
            Err(DraftInvalid::ControlCharacter { field: "subject" })
        );
        // 正文里的换行当然是正文。
        let body = DraftPatch {
            body: Some("行一\n行二\r\n".into()),
            ..DraftPatch::default()
        };
        assert_eq!(validate_patch(&body, &Draft::default()), Ok(()));
    }

    #[test]
    fn recipient_total_counts_unpatched_fields_too() {
        let current = draft_with(vec!["a@b.example"; MAX_DRAFT_RECIPIENTS - 1]);
        // 只补 cc 也要把既有的 to 算进总数，否则分两次就能绕过上限。
        let patch = DraftPatch {
            cc: Some(vec!["c@d.example".into(), "e@f.example".into()]),
            ..DraftPatch::default()
        };
        assert_eq!(
            validate_patch(&patch, &current),
            Err(DraftInvalid::TooMany {
                field: "recipients",
                limit: MAX_DRAFT_RECIPIENTS
            })
        );
        // 同一次请求里把 to 清空就又装得下了。
        let with_clear = DraftPatch {
            to: Some(Vec::new()),
            ..patch
        };
        assert_eq!(validate_patch(&with_clear, &current), Ok(()));
    }

    #[test]
    fn oversized_fields_are_refused_by_name_and_limit() {
        let patch = DraftPatch {
            body: Some("汉".repeat(MAX_DRAFT_BODY_BYTES)),
            ..DraftPatch::default()
        };
        assert_eq!(
            validate_patch(&patch, &Draft::default()),
            Err(DraftInvalid::TooLarge {
                field: "body",
                limit: MAX_DRAFT_BODY_BYTES
            })
        );
        // 恰好压线的正文必须过——上限是拒绝的门槛，不是建议值。
        let exact = DraftPatch {
            body: Some("a".repeat(MAX_DRAFT_BODY_BYTES)),
            ..DraftPatch::default()
        };
        assert_eq!(validate_patch(&exact, &Draft::default()), Ok(()));
    }

    #[test]
    fn an_empty_patch_is_recognisable() {
        assert!(DraftPatch::default().is_empty());
        assert!(!DraftPatch {
            body: Some(String::new()),
            ..DraftPatch::default()
        }
        .is_empty());
    }

    #[test]
    fn apply_fields_replaces_only_present_fields() {
        let mut draft = Draft {
            subject: "原主题".into(),
            body: "原正文".into(),
            to: vec!["a@b.example".into()],
            ..Draft::default()
        };
        DraftPatch {
            body: Some(String::new()),
            ..DraftPatch::default()
        }
        .apply_fields(&mut draft);
        assert_eq!(draft.subject, "原主题");
        assert_eq!(draft.body, "");
        assert_eq!(draft.to, vec!["a@b.example".to_string()]);
    }
}
