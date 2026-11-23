//! locale_preference：用户的语言偏好（ADR-0048 决策 1/2）。
//!
//! 这是 daemon 拥有的**持久偏好**，与「默认前端」「远程内容策略」同级：`locale_preference.toml`
//! 与活动配置同目录，文件纪律照搬 `reading_policy.toml`（0600、当前用户拥有、单硬链接、拒符号链接、
//! 64 KiB 上限、未知 `format_version` / 未知字段 fail-closed、临时文件 + fsync + 原子 rename）。
//! 只读从不创建文件，首次写入才创建；`maild backup` 不覆盖它（是偏好，不是邮箱数据）。
//!
//! 内核只下发**有序偏好列表**（[`LocalePreference::effective`]），不替任何前端挑定一种语言：
//! 各前端与自己支持的语言协商。`system` 模式下的系统首选语言由 Agent 统一读取一次再下发，
//! 避免 Studio、Menu、启动器各读各的按 app 语言设置而不一致。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cordis::Service;
use serde::{Deserialize, Serialize};

/// 偏好文件名（活动配置同目录）。
pub const PREFERENCE_FILE: &str = "locale_preference.toml";
const FORMAT_VERSION: i64 = 1;
const MAX_PREFERENCE_BYTES: u64 = 64 * 1024;
/// `explicit` 模式下语言条目上限；系统首选语言也截到同一上限，保证应答与推送帧有界。
pub const MAX_LANGUAGES: usize = 8;
/// 单个 BCP 47 标签的字节上限。
pub const MAX_TAG_BYTES: usize = 35;
/// 系统读取失败或为空时的兜底：`effective` 永不为空。
pub const FALLBACK_LANGUAGE: &str = "zh-Hans";

/// 偏好模式。缺省 `System`：跟随系统首选语言。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocaleMode {
    #[default]
    System,
    Explicit,
}

/// 对外可见的偏好（`getLocalePreference` / `setLocalePreference` 应答里的 `preference`）。
/// `system` 模式下 `languages` 恒为空。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalePreferenceSnapshot {
    pub mode: LocaleMode,
    pub languages: Vec<String>,
}

/// `getLocalePreference` / `setLocalePreference` 的应答形状。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocaleView {
    pub preference: LocalePreferenceSnapshot,
    /// 解析后的有序列表：`explicit` 时等于 `languages`；`system` 时是系统首选语言。永不为空。
    pub effective: Vec<String>,
    /// `false` = 偏好文件停摆：读到的是缺省偏好，写会被拒绝。
    pub persisted: bool,
}

/// 磁盘形态。未知字段拒绝：一个看不懂的键说明文件来自别处或未来，不能当作缺省静默接受。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceFile {
    format_version: i64,
    mode: LocaleMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    languages: Vec<String>,
}

/// `setLocalePreference` 参数。未知键拒绝；`languages` 是否出现由 `Option` 区分。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalePreferenceRequest {
    pub mode: LocaleMode,
    #[serde(default)]
    pub languages: Option<Vec<String>>,
}

/// 写入失败的三种原因，API 层分别映射 `-32602` / `-32001` / `-32603`。
#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    Invalid(String),
    Unavailable(String),
    Io(String),
}

enum State {
    Ready(LocalePreferenceSnapshot),
    /// 文件损坏/未来版本：读返回缺省、写一律拒绝且**绝不覆盖**磁盘上的那份。
    Unavailable(String),
}

type SystemLanguages = dyn Fn() -> Vec<String> + Send + Sync;

/// 语言偏好服务。一个进程一份，`Mutex` 保护内存副本，写入先落盘再更新内存。
pub struct LocalePreference {
    path: PathBuf,
    state: Mutex<State>,
    /// 系统首选语言来源。生产用 [`system_preferred_languages`]；测试注入固定值以保持 hermetic。
    system: Box<SystemLanguages>,
}

impl Service for LocalePreference {
    fn name(&self) -> &'static str {
        "locale.preference"
    }
}

impl LocalePreference {
    /// 从磁盘加载；文件不存在 → 缺省偏好（`system`）Ready（只读不制造状态文件）。
    pub fn open(path: PathBuf) -> Self {
        Self::open_with_system(path, system_preferred_languages)
    }

    /// 同 [`Self::open`]，但系统首选语言由调用方提供（测试用）。
    pub fn open_with_system(
        path: PathBuf,
        system: impl Fn() -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        let state = match load_file(&path) {
            Ok(snapshot) => State::Ready(snapshot),
            Err(reason) => {
                eprintln!(
                    "[maild/locale] {} 无法加载，语言偏好停摆（读返回缺省、写拒绝、文件不覆盖）: {reason}",
                    path.display()
                );
                State::Unavailable(reason)
            }
        };
        Self {
            path,
            state: Mutex::new(state),
            system: Box::new(system),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 偏好文件是否可用（= 写入会持久）。
    pub fn is_ready(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), State::Ready(_))
    }

    /// 当前偏好。停摆时返回缺省偏好（`system`），与「没设过」一致。
    pub fn snapshot(&self) -> LocalePreferenceSnapshot {
        match &*self.state.lock().unwrap() {
            State::Ready(snapshot) => snapshot.clone(),
            State::Unavailable(_) => LocalePreferenceSnapshot::default(),
        }
    }

    /// 解析后的有序语言列表，永不为空。`system` 模式下**每次调用都重读系统值**
    /// （ADR-0048 决策 2 的退化路径：不订阅系统语言变化通知）。
    pub fn effective(&self) -> Vec<String> {
        self.resolve(&self.snapshot())
    }

    /// 应答形状：偏好 + 解析结果 + 是否持久。
    pub fn view(&self) -> LocaleView {
        let (preference, persisted) = match &*self.state.lock().unwrap() {
            State::Ready(snapshot) => (snapshot.clone(), true),
            State::Unavailable(_) => (LocalePreferenceSnapshot::default(), false),
        };
        LocaleView {
            effective: self.resolve(&preference),
            preference,
            persisted,
        }
    }

    /// 整包替换偏好：任一项非法整包拒绝、不落盘；落盘成功后才更新内存副本。
    pub fn set(&self, request: &LocalePreferenceRequest) -> Result<LocaleView, SetError> {
        let next = validate_request(request).map_err(SetError::Invalid)?;
        {
            let mut state = self.state.lock().unwrap();
            if let State::Unavailable(reason) = &*state {
                return Err(SetError::Unavailable(reason.clone()));
            }
            save_file(&self.path, &next).map_err(SetError::Io)?;
            *state = State::Ready(next.clone());
        }
        Ok(LocaleView {
            effective: self.resolve(&next),
            preference: next,
            persisted: true,
        })
    }

    fn resolve(&self, preference: &LocalePreferenceSnapshot) -> Vec<String> {
        let languages = match preference.mode {
            LocaleMode::Explicit => preference.languages.clone(),
            LocaleMode::System => normalize_list((self.system)()),
        };
        if languages.is_empty() {
            vec![FALLBACK_LANGUAGE.to_string()]
        } else {
            languages
        }
    }
}

/// 校验 `setLocalePreference` 参数并规范化成快照。
pub fn validate_request(
    request: &LocalePreferenceRequest,
) -> Result<LocalePreferenceSnapshot, String> {
    match request.mode {
        LocaleMode::System => {
            if request.languages.is_some() {
                return Err("mode = \"system\" 时不得给出 `languages`".into());
            }
            Ok(LocalePreferenceSnapshot::default())
        }
        LocaleMode::Explicit => {
            let raw = request
                .languages
                .as_ref()
                .ok_or_else(|| "mode = \"explicit\" 时必须给出 `languages`".to_string())?;
            if raw.is_empty() {
                return Err("`languages` 至少一项".into());
            }
            if raw.len() > MAX_LANGUAGES {
                return Err(format!("`languages` 超过 {MAX_LANGUAGES} 项上限"));
            }
            let mut languages: Vec<String> = Vec::with_capacity(raw.len());
            for tag in raw {
                let tag = normalize_tag(tag)?;
                if !languages.contains(&tag) {
                    languages.push(tag);
                }
            }
            Ok(LocalePreferenceSnapshot {
                mode: LocaleMode::Explicit,
                languages,
            })
        }
    }
}

/// 规范化一个 BCP 47 标签（RFC 5646 的 langtag 语法，不含 grandfathered / 纯 private-use）：
/// 主语言小写、书写系统首字母大写、地区大写、其余小写。只校验形状，不查 IANA 注册表。
pub fn normalize_tag(raw: &str) -> Result<String, String> {
    let tag = raw.trim();
    if tag.is_empty() {
        return Err("语言标签不能为空".into());
    }
    if tag.len() > MAX_TAG_BYTES {
        return Err(format!("语言标签过长（上限 {MAX_TAG_BYTES} 字节）"));
    }
    let invalid = || format!("语言标签 `{tag}` 不是合法的 BCP 47 标签");
    let subtags: Vec<&str> = tag.split('-').collect();
    if subtags
        .iter()
        .any(|s| s.is_empty() || s.len() > 8 || !s.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err(invalid());
    }
    let alpha = |s: &str| s.bytes().all(|b| b.is_ascii_alphabetic());
    let digit = |s: &str| s.bytes().all(|b| b.is_ascii_digit());

    let mut out: Vec<String> = Vec::with_capacity(subtags.len());
    let mut rest = subtags.iter().copied().peekable();
    let language = rest.next().ok_or_else(invalid)?;
    if !(matches!(language.len(), 2..=3 | 5..=8) && alpha(language)) {
        return Err(invalid());
    }
    out.push(language.to_ascii_lowercase());
    // extlang：至多三个 3ALPHA，只跟在 2–3 字母主语言之后。
    if language.len() <= 3 {
        for _ in 0..3 {
            match rest.peek() {
                Some(s) if s.len() == 3 && alpha(s) => {
                    out.push(s.to_ascii_lowercase());
                    rest.next();
                }
                _ => break,
            }
        }
    }
    // script：4ALPHA，首字母大写。
    if let Some(s) = rest.peek().copied().filter(|s| s.len() == 4 && alpha(s)) {
        let lower = s.to_ascii_lowercase();
        out.push(lower[..1].to_ascii_uppercase() + &lower[1..]);
        rest.next();
    }
    // region：2ALPHA（大写）或 3DIGIT。
    if let Some(s) = rest
        .peek()
        .copied()
        .filter(|s| (s.len() == 2 && alpha(s)) || (s.len() == 3 && digit(s)))
    {
        out.push(s.to_ascii_uppercase());
        rest.next();
    }
    // variant：5–8 alphanum，或数字开头的 4 位。
    while let Some(s) = rest
        .peek()
        .copied()
        .filter(|s| s.len() >= 5 || (s.len() == 4 && s.as_bytes()[0].is_ascii_digit()))
    {
        out.push(s.to_ascii_lowercase());
        rest.next();
    }
    // extension（singleton + 2–8 字符子标签）与 private-use（x + 1–8 字符子标签，吃掉余下全部）。
    while let Some(singleton) = rest.next() {
        if singleton.len() != 1 {
            return Err(invalid());
        }
        let singleton = singleton.to_ascii_lowercase();
        let private = singleton == "x";
        out.push(singleton);
        let mut count = 0;
        while let Some(s) = rest.peek().copied() {
            if s.len() == 1 && !private {
                break;
            }
            if !private && s.len() < 2 {
                return Err(invalid());
            }
            out.push(s.to_ascii_lowercase());
            rest.next();
            count += 1;
        }
        if count == 0 {
            return Err(invalid());
        }
    }
    Ok(out.join("-"))
}

/// 规范化一份外部来源的语言列表（系统首选语言）：非法项跳过、去重、截到 [`MAX_LANGUAGES`]。
fn normalize_list(raw: Vec<String>) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for tag in raw {
        let Ok(tag) = normalize_tag(&tag) else {
            continue;
        };
        if !languages.contains(&tag) {
            languages.push(tag);
        }
        if languages.len() == MAX_LANGUAGES {
            break;
        }
    }
    languages
}

/// 系统首选语言（未规范化，可能为空）。
///
/// macOS 读 `+[NSLocale preferredLanguages]`——与 `CFLocaleCopyPreferredLanguages` 是同一来源
/// （toll-free bridged），复用已有的 objc2-foundation 绑定，不另引依赖。其他平台按 POSIX 优先级
/// 读 `LC_ALL` → `LC_MESSAGES` → `LANG`。
#[cfg(target_os = "macos")]
pub fn system_preferred_languages() -> Vec<String> {
    objc2::rc::autoreleasepool(|_| {
        let languages = objc2_foundation::NSLocale::preferredLanguages();
        (0..languages.count())
            .map(|index| languages.objectAtIndex(index).to_string())
            .collect()
    })
}

/// 系统首选语言（未规范化，可能为空）。见 macOS 版本的说明。
#[cfg(not(target_os = "macos"))]
pub fn system_preferred_languages() -> Vec<String> {
    languages_from_env(|key| std::env::var(key).ok())
}

/// POSIX 区域环境变量 → 语言标签。第一个**非空**的变量决定结果；`C` / `POSIX` 表示没有语言偏好。
#[cfg(any(not(target_os = "macos"), test))]
fn languages_from_env(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(value) = get(key).filter(|value| !value.is_empty()) else {
            continue;
        };
        let base = value.split(['.', '@']).next().unwrap_or_default();
        if base.is_empty() || base == "C" || base == "POSIX" {
            return Vec::new();
        }
        return vec![base.replace('_', "-")];
    }
    Vec::new()
}

fn load_file(path: &Path) -> Result<LocalePreferenceSnapshot, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalePreferenceSnapshot::default())
        }
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    crate::plugin_state::check_owned_file(&metadata)?;
    let bytes = std::fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_PREFERENCE_BYTES {
        return Err(format!("超过 {MAX_PREFERENCE_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    // 先认版本再按 v1 形状解析：未来版本的报错说清是「版本」而不是「字段」。
    let table: toml::Table = toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    match table
        .get("format_version")
        .and_then(toml::Value::as_integer)
    {
        Some(FORMAT_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "未来 format_version {other}（当前支持 {FORMAT_VERSION}）"
            ))
        }
        None => return Err("缺少 format_version".into()),
    }
    let file: PreferenceFile = table
        .try_into()
        .map_err(|error| format!("解析失败: {error}"))?;
    match file.mode {
        LocaleMode::System => {
            if !file.languages.is_empty() {
                return Err("mode = \"system\" 时不得有 languages".into());
            }
        }
        LocaleMode::Explicit => {
            if file.languages.is_empty() || file.languages.len() > MAX_LANGUAGES {
                return Err(format!("languages 须为 1..={MAX_LANGUAGES} 项"));
            }
            let mut seen: Vec<&str> = Vec::with_capacity(file.languages.len());
            for tag in &file.languages {
                if normalize_tag(tag)? != *tag {
                    return Err(format!("语言标签 `{tag}` 未规范化"));
                }
                if seen.contains(&tag.as_str()) {
                    return Err(format!("重复语言标签: {tag}"));
                }
                seen.push(tag);
            }
        }
    }
    Ok(LocalePreferenceSnapshot {
        mode: file.mode,
        languages: file.languages,
    })
}

fn save_file(path: &Path, snapshot: &LocalePreferenceSnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("偏好文件路径缺少父目录: {path:?}"))?;
    let file = PreferenceFile {
        format_version: FORMAT_VERSION,
        mode: snapshot.mode,
        languages: snapshot.languages.clone(),
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("序列化失败: {error}"))?;
    let temp = parent.join(format!(
        ".{PREFERENCE_FILE}.tmp-{}",
        crate::plugin_identity::generate_token()
    ));
    let result = crate::plugin_state::publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// 偏好文件路径（活动配置同目录）。
pub fn preference_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(PREFERENCE_FILE)
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

    fn write_preference(dir: &Path, body: &str) {
        std::fs::write(dir.join(PREFERENCE_FILE), body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.join(PREFERENCE_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }

    fn open(path: PathBuf) -> LocalePreference {
        LocalePreference::open_with_system(path, || vec!["en-US".into(), "zh-Hans-CN".into()])
    }

    fn explicit(languages: &[&str]) -> LocalePreferenceRequest {
        LocalePreferenceRequest {
            mode: LocaleMode::Explicit,
            languages: Some(languages.iter().map(|tag| tag.to_string()).collect()),
        }
    }

    fn system() -> LocalePreferenceRequest {
        LocalePreferenceRequest {
            mode: LocaleMode::System,
            languages: None,
        }
    }

    #[test]
    fn missing_file_is_system_mode_and_writes_are_persisted_normalized() {
        let dir = secure_dir();
        let path = dir.path().join(PREFERENCE_FILE);
        let preference = open(path.clone());
        assert!(preference.is_ready());
        assert_eq!(preference.snapshot(), LocalePreferenceSnapshot::default());
        assert_eq!(
            preference.view(),
            LocaleView {
                preference: LocalePreferenceSnapshot::default(),
                effective: vec!["en-US".into(), "zh-Hans-CN".into()],
                persisted: true,
            }
        );
        assert!(!path.exists(), "只读从不落盘");

        let view = preference
            .set(&explicit(&[" EN ", "zh-hans", "En", "ZH-HANS-cn"]))
            .unwrap();
        assert_eq!(view.preference.mode, LocaleMode::Explicit);
        assert_eq!(view.preference.languages, ["en", "zh-Hans", "zh-Hans-CN"]);
        assert_eq!(view.effective, view.preference.languages);
        assert!(view.persisted);
        assert!(path.exists(), "首次写入才创建");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\n    \"en\",\n    \"zh-Hans\",\n    \"zh-Hans-CN\",\n]\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        // 重新打开 = 重启：偏好仍在。
        let reopened = open(path.clone());
        assert_eq!(reopened.view(), view);

        // 切回 system：文件保留但不再列语言；effective 回到系统值。
        let view = reopened.set(&system()).unwrap();
        assert_eq!(view.preference, LocalePreferenceSnapshot::default());
        assert_eq!(view.effective, ["en-US", "zh-Hans-CN"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "format_version = 1\nmode = \"system\"\n"
        );
        assert_eq!(open(path).snapshot(), LocalePreferenceSnapshot::default());
    }

    #[test]
    fn invalid_requests_are_rejected_whole_and_never_written() {
        let dir = secure_dir();
        let path = dir.path().join(PREFERENCE_FILE);
        let preference = open(path.clone());
        let nine = ["en", "fr", "de", "ja", "ko", "es", "it", "pt", "ru"];
        // 36 字节：形状合法，只超长度。
        let long = "en-abcdefgh-abcdefgh-abcdefgh-abcdef";
        for request in [
            explicit(&["en", "not a tag"]),
            explicit(&[]),
            explicit(&nine),
            explicit(&[long]),
            explicit(&["e"]),
            LocalePreferenceRequest {
                mode: LocaleMode::Explicit,
                languages: None,
            },
            LocalePreferenceRequest {
                mode: LocaleMode::System,
                languages: Some(Vec::new()),
            },
            LocalePreferenceRequest {
                mode: LocaleMode::System,
                languages: Some(vec!["en".into()]),
            },
        ] {
            assert!(
                matches!(preference.set(&request), Err(SetError::Invalid(_))),
                "{request:?} 应被拒绝"
            );
        }
        assert!(!path.exists(), "整包拒绝不落盘");
        assert_eq!(preference.snapshot(), LocalePreferenceSnapshot::default());

        // 恰好 8 项、恰好 35 字节都合法。
        let eight = &nine[..MAX_LANGUAGES];
        assert_eq!(preference.set(&explicit(eight)).unwrap().effective.len(), 8);
        let max = "en-abcdefgh-abcdefgh-abcdefgh-abcde";
        assert_eq!(max.len(), MAX_TAG_BYTES);
        assert_eq!(long.len(), MAX_TAG_BYTES + 1);
        assert!(preference.set(&explicit(&[max])).is_ok());
    }

    #[test]
    fn tag_grammar_normalizes_case_and_rejects_malformed_tags() {
        for (raw, expected) in [
            ("en", "en"),
            ("EN-us", "en-US"),
            ("zh-hans-cn", "zh-Hans-CN"),
            ("ZH-HANT-TW", "zh-Hant-TW"),
            ("es-419", "es-419"),
            ("sr-latn-RS", "sr-Latn-RS"),
            ("de-CH-1996", "de-CH-1996"),
            ("sl-rozaj-biske", "sl-rozaj-biske"),
            ("zh-yue-HK", "zh-yue-HK"),
            ("en-US-u-CA-gregory", "en-US-u-ca-gregory"),
            ("en-x-Private-A", "en-x-private-a"),
            ("  fr  ", "fr"),
        ] {
            assert_eq!(normalize_tag(raw).unwrap(), expected, "{raw}");
        }
        for bad in [
            "",
            "   ",
            "e",
            "engl",
            "123",
            "en_US",
            "en--US",
            "en-",
            "-en",
            "en US",
            "en-abcdefghi",
            "en-u",
            "en-u-a",
            "en-x",
            "x-private",
            "en-US-a-b",
            "zh-汉",
            "en-US-12x",
        ] {
            assert!(normalize_tag(bad).is_err(), "{bad:?} 应被拒绝");
        }
        assert!(normalize_tag(&format!("en-{}", "a".repeat(40))).is_err());
    }

    #[test]
    fn effective_is_never_empty_and_system_values_are_normalized() {
        let dir = secure_dir();
        let empty = LocalePreference::open_with_system(dir.path().join(PREFERENCE_FILE), Vec::new);
        assert_eq!(empty.effective(), [FALLBACK_LANGUAGE]);

        let junk = LocalePreference::open_with_system(dir.path().join(PREFERENCE_FILE), || {
            vec!["".into(), "not a tag".into()]
        });
        assert_eq!(junk.effective(), [FALLBACK_LANGUAGE]);

        let many = LocalePreference::open_with_system(dir.path().join(PREFERENCE_FILE), || {
            let mut list: Vec<String> = vec!["EN-us".into(), "en-US".into(), "bad tag".into()];
            list.extend(
                ["fr", "de", "ja", "ko", "es", "it", "pt", "ru", "nl"]
                    .into_iter()
                    .map(String::from),
            );
            list
        });
        let effective = many.effective();
        assert_eq!(effective.len(), MAX_LANGUAGES);
        assert_eq!(effective[..3], ["en-US", "fr", "de"]);

        // 停摆时同样回到系统值，永不为空。
        let dir = secure_dir();
        write_preference(dir.path(), "format_version = 2\n");
        let stalled =
            LocalePreference::open_with_system(dir.path().join(PREFERENCE_FILE), Vec::new);
        assert_eq!(stalled.view().effective, [FALLBACK_LANGUAGE]);
        assert!(!stalled.view().persisted);
    }

    #[test]
    fn system_mode_rereads_the_system_value_on_every_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let dir = secure_dir();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let preference =
            LocalePreference::open_with_system(dir.path().join(PREFERENCE_FILE), move || {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    vec!["en".into()]
                } else {
                    vec!["ja".into()]
                }
            });
        assert_eq!(preference.effective(), ["en"]);
        assert_eq!(preference.effective(), ["ja"]);
        // explicit 时不读系统。
        preference.set(&explicit(&["fr"])).unwrap();
        let before = calls.load(Ordering::SeqCst);
        assert_eq!(preference.effective(), ["fr"]);
        assert_eq!(calls.load(Ordering::SeqCst), before);
    }

    #[test]
    fn corrupt_or_future_files_stall_reads_to_default_and_refuse_writes_without_overwriting() {
        for body in [
            "format_version = 2\nmode = \"system\"\n",
            "format_version = 1\n",
            "mode = \"system\"\n",
            "this is not toml = = =",
            "format_version = 1\nmode = \"auto\"\n",
            "format_version = 1\nmode = \"system\"\nlanguages = [\"en\"]\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = []\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\"EN\"]\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\"en\", \"en\"]\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\"not a tag\"]\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\"a1\",\"b\"]\n",
            "format_version = 1\nmode = \"explicit\"\nlanguages = [\"en\",\"fr\",\"de\",\"ja\",\"ko\",\"es\",\"it\",\"pt\",\"ru\"]\n",
            "format_version = 1\nmode = \"system\"\nfuture_field = true\n",
            "format_version = \"1\"\nmode = \"system\"\n",
        ] {
            let dir = secure_dir();
            write_preference(dir.path(), body);
            let path = dir.path().join(PREFERENCE_FILE);
            let preference = open(path.clone());
            assert!(!preference.is_ready(), "{body:?} 应停摆");
            assert_eq!(preference.snapshot(), LocalePreferenceSnapshot::default());
            let view = preference.view();
            assert!(!view.persisted);
            assert_eq!(view.effective, ["en-US", "zh-Hans-CN"]);
            assert!(matches!(
                preference.set(&explicit(&["en"])),
                Err(SetError::Unavailable(_))
            ));
            assert!(matches!(preference.set(&system()), Err(SetError::Unavailable(_))));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body, "损坏文件不得被覆盖");
        }
    }

    #[test]
    fn valid_files_load() {
        for (body, expected) in [
            (
                "format_version = 1\nmode = \"system\"\n",
                LocalePreferenceSnapshot::default(),
            ),
            (
                "format_version = 1\nmode = \"system\"\nlanguages = []\n",
                LocalePreferenceSnapshot::default(),
            ),
            (
                "format_version = 1\nmode = \"explicit\"\nlanguages = [\"en\", \"zh-Hans\"]\n",
                LocalePreferenceSnapshot {
                    mode: LocaleMode::Explicit,
                    languages: vec!["en".into(), "zh-Hans".into()],
                },
            ),
        ] {
            let dir = secure_dir();
            write_preference(dir.path(), body);
            let preference = open(dir.path().join(PREFERENCE_FILE));
            assert!(preference.is_ready(), "{body:?}");
            assert_eq!(preference.snapshot(), expected);
        }
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = secure_dir();
        let mut body = String::from("format_version = 1\nmode = \"system\"\n");
        body.push_str(&"# ".repeat((MAX_PREFERENCE_BYTES / 2 + 8) as usize));
        write_preference(dir.path(), &body);
        assert!(!open(dir.path().join(PREFERENCE_FILE)).is_ready());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_hard_links_and_wide_permissions_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let body = "format_version = 1\nmode = \"system\"\n";

        let dir = secure_dir();
        let real = dir.path().join("elsewhere.toml");
        std::fs::write(&real, body).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join(PREFERENCE_FILE)).unwrap();
        let preference = open(dir.path().join(PREFERENCE_FILE));
        assert!(!preference.is_ready());
        assert!(matches!(
            preference.set(&system()),
            Err(SetError::Unavailable(_))
        ));
        assert!(std::fs::symlink_metadata(dir.path().join(PREFERENCE_FILE))
            .unwrap()
            .file_type()
            .is_symlink());

        let dir = secure_dir();
        write_preference(dir.path(), body);
        std::fs::hard_link(
            dir.path().join(PREFERENCE_FILE),
            dir.path().join("twin.toml"),
        )
        .unwrap();
        assert!(!open(dir.path().join(PREFERENCE_FILE)).is_ready());

        let dir = secure_dir();
        write_preference(dir.path(), body);
        std::fs::set_permissions(
            dir.path().join(PREFERENCE_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!open(dir.path().join(PREFERENCE_FILE)).is_ready());
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = secure_dir();
        let preference = open(dir.path().join(PREFERENCE_FILE));
        preference.set(&explicit(&["en"])).unwrap();
        preference.set(&explicit(&["ja", "en"])).unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [PREFERENCE_FILE]);
    }

    #[test]
    fn write_failure_keeps_the_previous_preference() {
        let dir = secure_dir();
        // 父目录不存在 → 落盘失败；内存副本不变。
        let preference = open(dir.path().join("missing").join(PREFERENCE_FILE));
        assert!(matches!(
            preference.set(&explicit(&["en"])),
            Err(SetError::Io(_))
        ));
        assert_eq!(preference.snapshot(), LocalePreferenceSnapshot::default());
        assert!(preference.is_ready());
    }

    /// 真实绑定冒烟：只验证调用不崩、结果能被规范化（不断言具体语言，保持 hermetic）。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_preferred_languages_binding_is_callable() {
        for tag in normalize_list(system_preferred_languages()) {
            assert_eq!(normalize_tag(&tag).unwrap(), tag);
        }
    }

    #[test]
    fn posix_locale_variables_follow_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(
            languages_from_env(env(&[("LANG", "en_US.UTF-8")])),
            ["en-US"]
        );
        assert_eq!(
            languages_from_env(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "zh_CN.UTF-8")
            ])),
            ["zh-CN"]
        );
        assert_eq!(
            languages_from_env(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "zh_CN"),
                ("LC_ALL", "de_DE@euro")
            ])),
            ["de-DE"]
        );
        // 空值等于未设置；C / POSIX 表示没有偏好 → 交给兜底。
        assert_eq!(
            languages_from_env(env(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")])),
            ["fr-FR"]
        );
        assert!(languages_from_env(env(&[("LC_ALL", "C"), ("LANG", "fr_FR")])).is_empty());
        assert!(languages_from_env(env(&[("LANG", "POSIX")])).is_empty());
        assert!(languages_from_env(env(&[])).is_empty());
    }

    #[test]
    fn preference_path_sits_next_to_the_config_file() {
        assert_eq!(
            preference_path_for(
                &Path::new("/etc")
                    .join(maild_brand::SLUG)
                    .join(maild_brand::CONFIG_FILE_NAME)
            ),
            Path::new("/etc")
                .join(maild_brand::SLUG)
                .join("locale_preference.toml")
        );
        assert_eq!(
            preference_path_for(Path::new(maild_brand::CONFIG_FILE_NAME)),
            PathBuf::from("./locale_preference.toml")
        );
    }
}
