//! reading_policy：远程内容默认策略与可信发件人名单（ADR-0032 决策 5/6）。
//!
//! 这是 daemon 拥有的**持久偏好**，不是邮箱数据：`reading_policy.toml` 与活动配置同目录，
//! 纪律逐条对齐 `plugin_state.toml`（0600、单硬链接、拒符号链接、64 KiB 上限、未知
//! `format_version` fail-closed、临时文件 + fsync + 原子 rename）。不进 SQLite 的理由写在
//! ADR 里：恢复一份旧邮件备份不该静默恢复旧的信任名单。
//!
//! 内核只给**决定**（[`ReadingPolicy::decide`]），不代取、不代理任何远程资源；执行决定是
//! 渲染隔离层（M4.5-04）的责任。这不是沙箱。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cordis::Service;
use serde::{Deserialize, Serialize};

/// 策略文件名（活动配置同目录）。
pub const POLICY_FILE: &str = "reading_policy.toml";
const FORMAT_VERSION: u64 = 1;
const MAX_POLICY_BYTES: u64 = 64 * 1024;
/// 可信发件人条目上限：应答要落在一帧内，且没人会手动信任五百个发件人。
pub const MAX_TRUSTED_SENDERS: usize = 500;
const MAX_DOMAIN_BYTES: usize = 253;

/// 远程内容默认策略。缺省 `Block`：原文中的远程图片、样式与字体默认不联网（UX-SPEC §4）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteContent {
    #[default]
    Block,
    Allow,
}

impl RemoteContent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Allow => "allow",
        }
    }
}

/// 一条可信条目的种类，由 `value` 是否含 `@` 判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustKind {
    Address,
    Domain,
}

/// 一条可信发件人。`value` 一律小写存储。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedSender {
    pub value: String,
    pub kind: TrustKind,
    pub added_at: u64,
}

/// 对外可见的策略快照（`getReadingPolicy` / `setReadingPolicy` 应答里的 `policy`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    pub remote_content: RemoteContent,
    pub trusted_senders: Vec<TrustedSender>,
}

/// 磁盘形态。与 [`PolicySnapshot`] 的差别只是 `format_version`。
#[derive(Serialize, Deserialize)]
struct PolicyFile {
    format_version: u64,
    #[serde(default)]
    remote_content: RemoteContent,
    #[serde(default)]
    trusted_senders: Vec<TrustedSender>,
}

/// `setReadingPolicy` 的原子 patch。未知键拒绝——一个拼错的字段名不该被静默忽略成「成功」。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    #[serde(default)]
    pub remote_content: Option<RemoteContent>,
    #[serde(default)]
    pub trust: Vec<String>,
    #[serde(default)]
    pub untrust: Vec<String>,
}

/// 决定的依据。`Default` = 没有任何条目命中，采用默认策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Default,
    TrustedAddress,
    TrustedDomain,
    AllowAll,
}

/// `getMessage` 应答里 `remote_content` 的形状。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteContentDecision {
    pub decision: RemoteContent,
    pub reason: DecisionReason,
}

impl RemoteContentDecision {
    /// 服务未装配时的结论：与缺省策略一致（阻止），理由 `default`。
    pub const FALLBACK: Self = Self {
        decision: RemoteContent::Block,
        reason: DecisionReason::Default,
    };
}

/// patch 失败的三种原因，API 层分别映射 `-32602` / `-32001` / `-32603`。
#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    Invalid(String),
    Unavailable(String),
    Io(String),
}

enum State {
    Ready(PolicySnapshot),
    /// 文件损坏/未来版本：读返回缺省、写一律拒绝且**绝不覆盖**磁盘上的那份。
    Unavailable(String),
}

/// 阅读策略服务。一个进程一份，`Mutex` 保护内存副本，写入先落盘再更新内存。
pub struct ReadingPolicy {
    path: PathBuf,
    state: Mutex<State>,
}

impl Service for ReadingPolicy {
    fn name(&self) -> &'static str {
        "reading.policy"
    }
}

impl ReadingPolicy {
    /// 从磁盘加载；文件不存在 → 缺省策略 Ready（首次安装不制造状态文件）。
    pub fn open(path: PathBuf) -> Self {
        let state = match load_file(&path) {
            Ok(snapshot) => State::Ready(snapshot),
            Err(reason) => {
                eprintln!(
                    "[maild/reading] {} 无法加载，阅读策略停摆（读返回缺省、写拒绝、文件不覆盖）: {reason}",
                    path.display()
                );
                State::Unavailable(reason)
            }
        };
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 状态文件是否可用（= 写入会持久）。
    pub fn is_ready(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), State::Ready(_))
    }

    /// 当前策略。停摆时返回缺省策略——渲染层据此阻止远程内容，与「没设过」一致。
    pub fn snapshot(&self) -> PolicySnapshot {
        match &*self.state.lock().unwrap() {
            State::Ready(snapshot) => snapshot.clone(),
            State::Unavailable(_) => PolicySnapshot::default(),
        }
    }

    /// 原子应用一个 patch：任一项非法整包拒绝、不落盘；落盘成功后才更新内存副本。
    pub fn apply(&self, patch: &PolicyPatch, now: u64) -> Result<PolicySnapshot, PatchError> {
        if patch.remote_content.is_none() && patch.trust.is_empty() && patch.untrust.is_empty() {
            return Err(PatchError::Invalid(
                "patch 为空：至少给出 `remote_content`、`trust` 或 `untrust` 之一".into(),
            ));
        }
        let mut trust = Vec::with_capacity(patch.trust.len());
        for raw in &patch.trust {
            let (value, kind) = normalize_value(raw).map_err(PatchError::Invalid)?;
            if !trust
                .iter()
                .any(|(existing, _): &(String, TrustKind)| existing == &value)
            {
                trust.push((value, kind));
            }
        }
        let mut untrust = Vec::with_capacity(patch.untrust.len());
        for raw in &patch.untrust {
            let (value, _) = normalize_value(raw).map_err(PatchError::Invalid)?;
            if trust.iter().any(|(trusted, _)| trusted == &value) {
                return Err(PatchError::Invalid(format!(
                    "`{value}` 同时出现在 trust 与 untrust"
                )));
            }
            untrust.push(value);
        }

        let mut state = self.state.lock().unwrap();
        let current = match &*state {
            State::Ready(snapshot) => snapshot,
            State::Unavailable(reason) => {
                return Err(PatchError::Unavailable(reason.clone()));
            }
        };
        let mut next = current.clone();
        if let Some(remote_content) = patch.remote_content {
            next.remote_content = remote_content;
        }
        next.trusted_senders
            .retain(|entry| !untrust.iter().any(|value| value == &entry.value));
        for (value, kind) in trust {
            if next
                .trusted_senders
                .iter()
                .any(|entry| entry.value == value)
            {
                continue;
            }
            next.trusted_senders.push(TrustedSender {
                value,
                kind,
                added_at: now,
            });
        }
        if next.trusted_senders.len() > MAX_TRUSTED_SENDERS {
            return Err(PatchError::Invalid(format!(
                "可信发件人超过 {MAX_TRUSTED_SENDERS} 条上限"
            )));
        }
        save_file(&self.path, &next).map_err(PatchError::Io)?;
        *state = State::Ready(next.clone());
        Ok(next)
    }

    /// 对一封信的 `From` 头给出结论。匹配规则固定在内核：先看全局放行，再比地址、再比
    /// **精确**域名（子域不匹配父域）。`From` 解析不出地址时永远是 `default`。
    pub fn decide(&self, from_header: &str) -> RemoteContentDecision {
        let snapshot = self.snapshot();
        decide_with(&snapshot, from_header)
    }
}

/// [`ReadingPolicy::decide`] 的纯函数形态，便于对快照直接测试。
pub fn decide_with(snapshot: &PolicySnapshot, from_header: &str) -> RemoteContentDecision {
    if snapshot.remote_content == RemoteContent::Allow {
        return RemoteContentDecision {
            decision: RemoteContent::Allow,
            reason: DecisionReason::AllowAll,
        };
    }
    let Some(address) = crate::compose::extract_addresses(from_header)
        .into_iter()
        .next()
        .map(|address| address.to_lowercase())
    else {
        return RemoteContentDecision::FALLBACK;
    };
    if snapshot
        .trusted_senders
        .iter()
        .any(|entry| entry.kind == TrustKind::Address && entry.value == address)
    {
        return RemoteContentDecision {
            decision: RemoteContent::Allow,
            reason: DecisionReason::TrustedAddress,
        };
    }
    let domain = crate::addresses::domain_of(&address).unwrap_or_default();
    if !domain.is_empty()
        && snapshot
            .trusted_senders
            .iter()
            .any(|entry| entry.kind == TrustKind::Domain && entry.value == domain)
    {
        return RemoteContentDecision {
            decision: RemoteContent::Allow,
            reason: DecisionReason::TrustedDomain,
        };
    }
    RemoteContentDecision::FALLBACK
}

/// 规范化一条用户输入：去首尾空白、小写；含 `@` 走地址语法门，否则走域名形状门。
pub fn normalize_value(raw: &str) -> Result<(String, TrustKind), String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        return Err("可信条目不能为空".into());
    }
    if value.contains('@') {
        crate::addresses::validate_address(&value)
            .map_err(|error| format!("地址 `{value}` 无效: {error}"))?;
        return Ok((value, TrustKind::Address));
    }
    if value.len() > MAX_DOMAIN_BYTES {
        return Err(format!("域名过长（上限 {MAX_DOMAIN_BYTES} 字节）"));
    }
    let shape_ok = value.contains('.')
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("..")
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '.'));
    if !shape_ok {
        return Err(format!("域名 `{value}` 形状不合法"));
    }
    Ok((value, TrustKind::Domain))
}

fn load_file(path: &Path) -> Result<PolicySnapshot, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PolicySnapshot::default())
        }
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    crate::plugin_state::check_owned_file(&metadata)?;
    let bytes = std::fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(format!("超过 {MAX_POLICY_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    let file: PolicyFile = toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    if file.format_version != FORMAT_VERSION {
        return Err(format!(
            "未来 format_version {}（当前支持 {FORMAT_VERSION}）",
            file.format_version
        ));
    }
    if file.trusted_senders.len() > MAX_TRUSTED_SENDERS {
        return Err(format!("可信发件人超过 {MAX_TRUSTED_SENDERS} 条上限"));
    }
    let mut seen: Vec<&str> = Vec::with_capacity(file.trusted_senders.len());
    for entry in &file.trusted_senders {
        let (value, kind) = normalize_value(&entry.value)?;
        if value != entry.value || kind != entry.kind {
            return Err(format!("条目 `{}` 未规范化或种类不符", entry.value));
        }
        if seen.contains(&entry.value.as_str()) {
            return Err(format!("重复条目: {}", entry.value));
        }
        seen.push(&entry.value);
    }
    Ok(PolicySnapshot {
        remote_content: file.remote_content,
        trusted_senders: file.trusted_senders,
    })
}

fn save_file(path: &Path, snapshot: &PolicySnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("策略文件路径缺少父目录: {path:?}"))?;
    let file = PolicyFile {
        format_version: FORMAT_VERSION,
        remote_content: snapshot.remote_content,
        trusted_senders: snapshot.trusted_senders.clone(),
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("序列化失败: {error}"))?;
    let temp = parent.join(format!(
        ".{POLICY_FILE}.tmp-{}",
        crate::plugin_identity::generate_token()
    ));
    let result = crate::plugin_state::publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// 策略文件路径（活动配置同目录）。
pub fn policy_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(POLICY_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secure_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn write_policy(dir: &Path, body: &str) {
        std::fs::write(dir.join(POLICY_FILE), body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.join(POLICY_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }

    fn patch(remote: Option<RemoteContent>, trust: &[&str], untrust: &[&str]) -> PolicyPatch {
        PolicyPatch {
            remote_content: remote,
            trust: trust.iter().map(|value| value.to_string()).collect(),
            untrust: untrust.iter().map(|value| value.to_string()).collect(),
        }
    }

    #[test]
    fn missing_file_is_default_block_and_writes_are_persisted_lowercased() {
        let dir = secure_dir();
        let path = dir.path().join(POLICY_FILE);
        let policy = ReadingPolicy::open(path.clone());
        assert!(policy.is_ready());
        assert_eq!(policy.snapshot(), PolicySnapshot::default());
        assert!(!path.exists(), "只读从不落盘");

        let next = policy
            .apply(
                &patch(None, &["  News@Example.COM ", "Example.ORG"], &[]),
                1_757_600_000,
            )
            .unwrap();
        assert_eq!(next.remote_content, RemoteContent::Block);
        assert_eq!(
            next.trusted_senders,
            vec![
                TrustedSender {
                    value: "news@example.com".into(),
                    kind: TrustKind::Address,
                    added_at: 1_757_600_000
                },
                TrustedSender {
                    value: "example.org".into(),
                    kind: TrustKind::Domain,
                    added_at: 1_757_600_000
                },
            ]
        );
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        // 重新打开 = 重启：策略仍在。
        let reopened = ReadingPolicy::open(path);
        assert_eq!(reopened.snapshot(), next);
    }

    #[test]
    fn patch_is_atomic_idempotent_and_rejects_conflicts() {
        let dir = secure_dir();
        let policy = ReadingPolicy::open(dir.path().join(POLICY_FILE));
        policy
            .apply(&patch(None, &["a@example.com"], &[]), 1)
            .unwrap();

        // 一项非法 → 整包拒绝，好的那项也不落。
        let error = policy
            .apply(&patch(None, &["ok@example.com", "not an address"], &[]), 2)
            .unwrap_err();
        assert!(matches!(error, PatchError::Invalid(_)), "{error:?}");
        assert_eq!(policy.snapshot().trusted_senders.len(), 1);

        // 已存在的 trust 与不存在的 untrust 都是 no-op；added_at 不被刷新。
        let next = policy
            .apply(&patch(None, &["A@EXAMPLE.COM"], &["nobody@example.com"]), 3)
            .unwrap();
        assert_eq!(next.trusted_senders.len(), 1);
        assert_eq!(next.trusted_senders[0].added_at, 1);

        // 同一 patch 里既 trust 又 untrust 是矛盾。
        let error = policy
            .apply(&patch(None, &["x@example.com"], &["X@example.com"]), 4)
            .unwrap_err();
        assert!(matches!(error, PatchError::Invalid(_)));

        // 空 patch 不是「成功什么都没做」。
        assert!(matches!(
            policy.apply(&PolicyPatch::default(), 5),
            Err(PatchError::Invalid(_))
        ));

        // untrust 真的删；remote_content 与名单可以同一包改。
        let next = policy
            .apply(
                &patch(Some(RemoteContent::Allow), &[], &["a@example.com"]),
                6,
            )
            .unwrap();
        assert_eq!(next.remote_content, RemoteContent::Allow);
        assert!(next.trusted_senders.is_empty());
    }

    #[test]
    fn value_grammar_accepts_addresses_and_domains_and_rejects_the_rest() {
        assert_eq!(
            normalize_value("Alice@Example.com").unwrap(),
            ("alice@example.com".to_string(), TrustKind::Address)
        );
        assert_eq!(
            normalize_value("News.Example.com").unwrap(),
            ("news.example.com".to_string(), TrustKind::Domain)
        );
        for bad in [
            "",
            "   ",
            "com",
            ".example.com",
            "example.com.",
            "exa mple.com",
            "ex..ample.com",
            "-example.com",
            "a@@example.com",
            "@example.com",
            "alice@localhost",
            "http://example.com",
        ] {
            assert!(normalize_value(bad).is_err(), "{bad:?} 应被拒绝");
        }
        let long = format!("{}.com", "a".repeat(MAX_DOMAIN_BYTES));
        assert!(normalize_value(&long).is_err());
    }

    #[test]
    fn decisions_prefer_allow_all_then_address_then_exact_domain() {
        let snapshot = PolicySnapshot {
            remote_content: RemoteContent::Block,
            trusted_senders: vec![
                TrustedSender {
                    value: "news@example.com".into(),
                    kind: TrustKind::Address,
                    added_at: 0,
                },
                TrustedSender {
                    value: "example.org".into(),
                    kind: TrustKind::Domain,
                    added_at: 0,
                },
            ],
        };
        let decide = |from: &str| decide_with(&snapshot, from);
        assert_eq!(
            decide("Newsletter <NEWS@Example.com>").reason,
            DecisionReason::TrustedAddress
        );
        assert_eq!(decide("other@example.com").reason, DecisionReason::Default);
        assert_eq!(
            decide("anyone@example.org").reason,
            DecisionReason::TrustedDomain
        );
        assert_eq!(decide("anyone@example.org").decision, RemoteContent::Allow);
        // 子域不匹配父域：用户信任的是他看到的那个域。
        assert_eq!(decide("x@news.example.org").reason, DecisionReason::Default);
        assert_eq!(decide("x@news.example.org").decision, RemoteContent::Block);
        // 解析不出地址 → 缺省。
        assert_eq!(
            decide("garbage without address"),
            RemoteContentDecision::FALLBACK
        );
        assert_eq!(decide(""), RemoteContentDecision::FALLBACK);

        let allow_all = PolicySnapshot {
            remote_content: RemoteContent::Allow,
            trusted_senders: Vec::new(),
        };
        assert_eq!(
            decide_with(&allow_all, "anyone@anywhere.test"),
            RemoteContentDecision {
                decision: RemoteContent::Allow,
                reason: DecisionReason::AllowAll
            }
        );
        assert_eq!(decide_with(&allow_all, "").reason, DecisionReason::AllowAll);
    }

    #[test]
    fn corrupt_or_future_files_stall_reads_to_default_and_refuse_writes_without_overwriting() {
        for body in [
            "format_version = 2\n",
            "this is not toml = = =",
            "format_version = 1\ntrusted_senders = [{ value = \"Upper@Example.com\", kind = \"address\", added_at = 1 }]\n",
            "format_version = 1\ntrusted_senders = [{ value = \"example.com\", kind = \"address\", added_at = 1 }]\n",
            "format_version = 1\nremote_content = \"maybe\"\n",
        ] {
            let dir = secure_dir();
            write_policy(dir.path(), body);
            let path = dir.path().join(POLICY_FILE);
            let policy = ReadingPolicy::open(path.clone());
            assert!(!policy.is_ready(), "{body:?} 应停摆");
            assert_eq!(policy.snapshot(), PolicySnapshot::default());
            assert!(matches!(
                policy.apply(&patch(Some(RemoteContent::Allow), &[], &[]), 1),
                Err(PatchError::Unavailable(_))
            ));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body, "损坏文件不得被覆盖");
        }
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = secure_dir();
        let mut body = String::from("format_version = 1\n");
        body.push_str(&"# ".repeat((MAX_POLICY_BYTES / 2 + 8) as usize));
        write_policy(dir.path(), &body);
        assert!(!ReadingPolicy::open(dir.path().join(POLICY_FILE)).is_ready());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_wide_permissions_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = secure_dir();
        let real = dir.path().join("elsewhere.toml");
        std::fs::write(&real, "format_version = 1\n").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join(POLICY_FILE)).unwrap();
        assert!(!ReadingPolicy::open(dir.path().join(POLICY_FILE)).is_ready());

        let dir = secure_dir();
        write_policy(dir.path(), "format_version = 1\n");
        std::fs::set_permissions(
            dir.path().join(POLICY_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!ReadingPolicy::open(dir.path().join(POLICY_FILE)).is_ready());
    }

    #[test]
    fn entry_cap_is_enforced_on_write() {
        let dir = secure_dir();
        let policy = ReadingPolicy::open(dir.path().join(POLICY_FILE));
        let many: Vec<String> = (0..MAX_TRUSTED_SENDERS)
            .map(|index| format!("user{index}@example.com"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        policy.apply(&patch(None, &refs, &[]), 1).unwrap();
        assert!(matches!(
            policy.apply(&patch(None, &["one-more@example.com"], &[]), 2),
            Err(PatchError::Invalid(_))
        ));
        assert_eq!(policy.snapshot().trusted_senders.len(), MAX_TRUSTED_SENDERS);
    }

    #[test]
    fn policy_path_sits_next_to_the_config_file() {
        assert_eq!(
            policy_path_for(&Path::new("/etc").join(maild_brand::SLUG).join(maild_brand::CONFIG_FILE_NAME)),
            Path::new("/etc").join(maild_brand::SLUG).join("reading_policy.toml")
        );
        assert_eq!(
            policy_path_for(Path::new(maild_brand::CONFIG_FILE_NAME)),
            PathBuf::from("./reading_policy.toml")
        );
    }
}
