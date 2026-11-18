//! rules：通知路由规则（`notify/route` bail 事件的策略监听器）。
//!
//! ARCHITECTURE §5：`notify/route` 是 bail——策略监听器直接返回最终通知通道。
//! R4.5 的 rules 提供三条基础策略：
//! - 敏感内容总开关：`show_sensitive = false` → 敏感通知降级为 silent（预览策略）；
//! - 关键词静默：subject 命中 `silent_keywords` → silent。
//! - 时间静默：`quiet_hours` 按日周期窗口 → silent（查询时即时计算，无后台轮询唤醒）。
//!
//! per-account 作用域天然由 `scoped(realm)` 事件语义提供（rules 挂在账户 scope
//! 上即只对该账户生效）；时间窗口支持 `follow_system_timezone = true`（POSIX `localtime_r`
//! 跟随系统时区与夏令时）或固定 `timezone_offset_minutes`，零外部时区库依赖。
//!
//! ADR-0043 additive 扩展：
//! - `notify_new_mail` / `muted_accounts`：普适收件箱新邮件通知（[`mail_notify`]），
//!   去重、防冷启动刷屏、敏感预览脱敏、按账户静音、按账户滑动窗口合并；
//! - `persist`：TOML 开关，配合 [`RulesStatePath`] / [`RulesSettings::open_persistent`]
//!   把运行时 patch 原子落盘到 `rules_state.toml`（[`persistence`]，纪律对齐
//!   `reading_policy.toml`）。

pub mod mail_notify;
pub mod persistence;

use std::any::TypeId;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use cordis::{Context, Plugin, PluginError, Service};
use serde::{Deserialize, Serialize};

pub use persistence::{rules_state_path_for, RulesStatePath, RULES_STATE_FILE};

use crate::events::{NotifyRoute, NotifyRouteEvent, QuietHoursQuery, QuietHoursRequest};

const MINUTES_PER_DAY: u16 = 24 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHoursConfig {
    /// 日周期开始时间，格式 `HH:MM`（含开始时刻）。
    pub start: String,
    /// 日周期结束时间，格式 `HH:MM`（不含结束时刻；`24:00` 表示当日日末）；允许跨午夜。
    pub end: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RulesConfig {
    /// 敏感内容总开关（false → 敏感通知一律静默，不预览）。
    #[serde(default = "default_show_sensitive")]
    pub show_sensitive: bool,
    /// 标题命中任一关键词 → 静默。
    #[serde(default)]
    pub silent_keywords: Vec<String>,
    /// 静默时段。多个窗口取并集。
    #[serde(default)]
    pub quiet_hours: Vec<QuietHoursConfig>,
    /// 本地时间相对 UTC 的分钟偏移，例如中国标准时间为 480。
    #[serde(default)]
    pub timezone_offset_minutes: i32,
    /// 静默时段跟随系统时区（含夏令时自动切换）；缺省 `false` 时按固定
    /// `timezone_offset_minutes` 解释，不出现在 JSON 里。
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_system_timezone: bool,
    /// 普适收件箱新邮件系统通知（ADR-0043）。缺省关闭；关闭时不出现在 JSON 里，
    /// 旧客户端看到的 `rules` 逐字节不变。
    #[serde(default, skip_serializing_if = "is_false")]
    pub notify_new_mail: bool,
    /// 按账户（`MailMessage::account`，即账户 ID/名称）静音新邮件通知。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted_accounts: Vec<String>,
    /// 仅 TOML：是否启用 `rules_state.toml` 侧车持久化。不进 API `rules` JSON，
    /// 也不能经 `setSettings` 修改。
    #[serde(default, skip_serializing)]
    pub persist: bool,
}

fn default_show_sensitive() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `muted_accounts` 上限。
pub const MAX_MUTED_ACCOUNTS: usize = 64;
/// `muted_accounts` 单项字节上限。
pub const MAX_MUTED_ACCOUNT_BYTES: usize = 128;

impl Default for RulesConfig {
    /// 与 serde 默认值一致。
    fn default() -> Self {
        RulesConfig {
            show_sensitive: default_show_sensitive(),
            silent_keywords: Vec::new(),
            quiet_hours: Vec::new(),
            timezone_offset_minutes: 0,
            follow_system_timezone: false,
            notify_new_mail: false,
            muted_accounts: Vec::new(),
            persist: false,
        }
    }
}

impl RulesConfig {
    /// `account` 是否在静音名单里（精确匹配，名单项首尾空白不计）。
    pub fn is_account_muted(&self, account: &str) -> bool {
        self.muted_accounts
            .iter()
            .any(|muted| muted.trim() == account)
    }
}

impl Service for RulesConfig {
    fn name(&self) -> &'static str {
        "rules.config"
    }
}

/// 供 headless API 修改的 rules 运行时配置。
///
/// - 非持久模式（[`RulesSettings::ephemeral`]）：patch 只作用于当前进程，重启后以 TOML 为准；
/// - 持久模式（[`RulesSettings::open_persistent`]）：patch 先原子写入 `rules_state.toml`
///   再更新内存；重启后「TOML 基线 + 侧车覆盖」。侧车损坏/不安全时停摆：读返回 TOML 基线、
///   写一律拒绝（[`RulesPatchError::Unavailable`]）、**绝不覆盖**磁盘上的那份。
#[derive(Clone)]
pub struct RulesSettings {
    inner: Arc<RulesSettingsInner>,
}

struct RulesSettingsInner {
    state: RwLock<RulesState>,
    /// 侧车路径：构造时给定，或经 [`RulesSettings::enable_persistence`] 运行时挂载一次。
    path: OnceLock<PathBuf>,
}

struct RulesState {
    config: RulesConfig,
    /// TOML 基线（持久模式下与 overlay 合成 `config`）。
    base: RulesConfig,
    /// 侧车里记录的用户覆盖（只含用户改过的字段）。
    overlay: RulesPatch,
    persistence: Persistence,
}

enum Persistence {
    /// 没有挂载侧车文件：写入只在内存。
    Detached,
    Ready,
    /// 侧车损坏/不安全：写拒绝、文件不覆盖。
    Unavailable(String),
}

/// `RulesSettings::try_patch` 失败原因；API 层可分别映射 `-32602` / `-32001` / `-32603`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RulesPatchError {
    Invalid(String),
    Unavailable(String),
    Io(String),
}

impl std::fmt::Display for RulesPatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(detail) => f.write_str(detail),
            Self::Unavailable(detail) => write!(f, "rules 持久化停摆: {detail}"),
            Self::Io(detail) => write!(f, "rules 持久化写入失败: {detail}"),
        }
    }
}

impl std::error::Error for RulesPatchError {}

impl Service for RulesSettings {
    fn name(&self) -> &'static str {
        "rules.settings"
    }
}

impl RulesSettings {
    fn build(
        base: RulesConfig,
        overlay: RulesPatch,
        persistence: Persistence,
        path: Option<PathBuf>,
    ) -> Self {
        let config = overlay.apply_to(&base);
        Self {
            inner: Arc::new(RulesSettingsInner {
                state: RwLock::new(RulesState {
                    config,
                    base,
                    overlay,
                    persistence,
                }),
                path: path.map(OnceLock::from).unwrap_or_default(),
            }),
        }
    }

    /// 非持久模式：patch 只作用于当前进程（`is_persisted() == false`）。
    pub fn ephemeral(base: RulesConfig) -> Self {
        Self::build(base, RulesPatch::default(), Persistence::Detached, None)
    }

    /// 持久模式：以 `base`（TOML）为基线，加载 `path` 处的 `rules_state.toml` 覆盖。
    ///
    /// 文件不存在 → 基线即当前态，首次 patch 时才落盘；文件是符号链接/非当前用户拥有/
    /// 权限宽于 0600/超过 64 KiB/未知 `format_version`/非法字段 → 回退基线并停摆
    /// （`is_persisted() == false`，patch 返回 [`RulesPatchError::Unavailable`]，文件不动）。
    pub fn open_persistent(base: RulesConfig, path: PathBuf) -> Self {
        match persistence::load(&path, &base) {
            Ok(overlay) => Self::build(base, overlay, Persistence::Ready, Some(path)),
            Err(reason) => {
                eprintln!(
                    "[maild/rules] {} 无法加载，规则持久化停摆（读返回 TOML 基线、写拒绝、文件不覆盖）: {reason}",
                    path.display()
                );
                Self::build(
                    base,
                    RulesPatch::default(),
                    Persistence::Unavailable(reason),
                    Some(path),
                )
            }
        }
    }

    /// 写入是否会持久（挂载了侧车文件且未停摆）。
    pub fn is_persisted(&self) -> bool {
        matches!(
            self.inner.state.read().unwrap().persistence,
            Persistence::Ready
        )
    }

    /// 挂载的侧车路径（非持久模式为 `None`）。
    pub fn state_path(&self) -> Option<&Path> {
        self.inner.path.get().map(PathBuf::as_path)
    }

    /// 运行时挂载侧车（`setSettings` 的 `persist: true`）。
    ///
    /// - 已持久：幂等成功（路径不同也不改挂——一个进程只认一份侧车）；
    /// - 停摆：返回 [`RulesPatchError::Unavailable`]，文件不动；
    /// - 非持久：按与 [`RulesSettings::open_persistent`] 相同的纪律读取 `path` 处的覆盖层，
    ///   与本进程内已改过的字段合成（本进程的改动优先），有改动才原子落盘，然后转为持久。
    ///   读取或写入失败时保持非持久、内存不变。
    pub fn enable_persistence(&self, path: PathBuf) -> Result<RulesConfig, RulesPatchError> {
        let mut state = self.inner.state.write().unwrap();
        match &state.persistence {
            Persistence::Ready => return Ok(state.config.clone()),
            Persistence::Unavailable(reason) => {
                return Err(RulesPatchError::Unavailable(reason.clone()))
            }
            Persistence::Detached => {}
        }
        if self.inner.path.get().is_some_and(|existing| existing != &path) {
            return Err(RulesPatchError::Unavailable("侧车路径已挂载为其他文件".into()));
        }
        let on_disk = persistence::load(&path, &state.base).map_err(RulesPatchError::Unavailable)?;
        let overlay = on_disk.merged_with(&state.overlay);
        let next = overlay.apply_to(&state.base);
        validate_config(&next).map_err(RulesPatchError::Invalid)?;
        if overlay != on_disk {
            persistence::save(&path, &overlay).map_err(RulesPatchError::Io)?;
        }
        let _ = self.inner.path.set(path);
        state.overlay = overlay;
        state.persistence = Persistence::Ready;
        state.config = next.clone();
        Ok(next)
    }

    /// 侧车停摆原因（有界诊断；未停摆为 `None`）。
    pub fn persistence_error(&self) -> Option<String> {
        match &self.inner.state.read().unwrap().persistence {
            Persistence::Unavailable(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    pub fn snapshot(&self) -> RulesConfig {
        self.inner.state.read().unwrap().config.clone()
    }

    /// 在同一把写锁内合并 patch，避免并发请求互相覆盖不同字段。
    ///
    /// 兼容旧签名：错误折叠为字符串。新代码请用 [`RulesSettings::try_patch`] 区分错误种类。
    pub fn patch(&self, patch: &RulesPatch) -> Result<RulesConfig, String> {
        self.try_patch(patch).map_err(|error| error.to_string())
    }

    /// 原子应用 patch：校验失败整包拒绝；持久模式下先落盘、成功后才更新内存。
    pub fn try_patch(&self, patch: &RulesPatch) -> Result<RulesConfig, RulesPatchError> {
        let mut state = self.inner.state.write().unwrap();
        let next = patch.apply_to(&state.config);
        validate_config(&next).map_err(RulesPatchError::Invalid)?;
        match &state.persistence {
            Persistence::Detached => {
                // 只在内存里记下改过哪些字段：之后若经 `enable_persistence` 挂载侧车，
                // 落盘的覆盖层与「用户改过的字段」语义一致。
                state.overlay = state.overlay.merged_with(patch);
            }
            Persistence::Unavailable(reason) => {
                return Err(RulesPatchError::Unavailable(reason.clone()));
            }
            Persistence::Ready => {
                let overlay = state.overlay.merged_with(patch);
                let path = self
                    .inner
                    .path
                    .get()
                    .ok_or_else(|| RulesPatchError::Io("侧车路径缺失".into()))?;
                persistence::save(path, &overlay).map_err(RulesPatchError::Io)?;
                state.overlay = overlay;
            }
        }
        state.config = next.clone();
        Ok(next)
    }

    /// TOML 基线（不含侧车覆盖）。
    pub fn base(&self) -> RulesConfig {
        self.inner.state.read().unwrap().base.clone()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RulesPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_keywords: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<Vec<QuietHoursConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_offset_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_system_timezone: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_new_mail: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_accounts: Option<Vec<String>>,
}

impl RulesPatch {
    /// 后者覆盖前者（`other` 里给出的字段替换 `self` 的同名字段）。
    pub fn merged_with(&self, other: &RulesPatch) -> RulesPatch {
        RulesPatch {
            show_sensitive: other.show_sensitive.or(self.show_sensitive),
            silent_keywords: other
                .silent_keywords
                .clone()
                .or_else(|| self.silent_keywords.clone()),
            quiet_hours: other
                .quiet_hours
                .clone()
                .or_else(|| self.quiet_hours.clone()),
            timezone_offset_minutes: other
                .timezone_offset_minutes
                .or(self.timezone_offset_minutes),
            follow_system_timezone: other
                .follow_system_timezone
                .or(self.follow_system_timezone),
            notify_new_mail: other.notify_new_mail.or(self.notify_new_mail),
            muted_accounts: other
                .muted_accounts
                .clone()
                .or_else(|| self.muted_accounts.clone()),
        }
    }

    pub fn apply_to(&self, base: &RulesConfig) -> RulesConfig {
        RulesConfig {
            show_sensitive: self.show_sensitive.unwrap_or(base.show_sensitive),
            silent_keywords: self
                .silent_keywords
                .clone()
                .unwrap_or_else(|| base.silent_keywords.clone()),
            quiet_hours: self
                .quiet_hours
                .clone()
                .unwrap_or_else(|| base.quiet_hours.clone()),
            timezone_offset_minutes: self
                .timezone_offset_minutes
                .unwrap_or(base.timezone_offset_minutes),
            follow_system_timezone: self
                .follow_system_timezone
                .unwrap_or(base.follow_system_timezone),
            notify_new_mail: self.notify_new_mail.unwrap_or(base.notify_new_mail),
            muted_accounts: self
                .muted_accounts
                .clone()
                .unwrap_or_else(|| base.muted_accounts.clone()),
            persist: base.persist,
        }
    }
}

/// 纯决策：命中策略返回最终路由（未命中 → None，走下一监听器/默认）。
pub(crate) fn decide(route: &NotifyRoute, cfg: &RulesConfig) -> Option<NotifyRoute> {
    decide_at(route, cfg, unix_now_secs())
}

pub(crate) fn decide_at(
    route: &NotifyRoute,
    cfg: &RulesConfig,
    at_unix_secs: u64,
) -> Option<NotifyRoute> {
    if route.sensitive && !cfg.show_sensitive {
        return Some(NotifyRoute {
            channel: "silent".into(),
            ..route.clone()
        });
    }
    for kw in &cfg.silent_keywords {
        if !kw.is_empty() && route.subject.contains(kw) {
            return Some(NotifyRoute {
                channel: "silent".into(),
                ..route.clone()
            });
        }
    }
    if is_quiet_at(cfg, at_unix_secs) {
        return Some(NotifyRoute {
            channel: "silent".into(),
            ..route.clone()
        });
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuietWindow {
    pub start: u16,
    pub end: u16,
}

fn parse_clock(value: &str, allow_24_end: bool) -> Result<u16, String> {
    let (hour, minute) = value
        .split_once(':')
        .ok_or_else(|| format!("时间必须是 HH:MM: {value}"))?;
    if hour.len() != 2 || minute.len() != 2 {
        return Err(format!("时间必须是两位 HH:MM: {value}"));
    }
    let hour: u16 = hour.parse().map_err(|_| format!("小时无效: {value}"))?;
    let minute: u16 = minute.parse().map_err(|_| format!("分钟无效: {value}"))?;
    if hour > 24 || minute >= 60 || (hour == 24 && minute != 0) {
        return Err(format!("时间超出范围: {value}"));
    }
    let total = hour * 60 + minute;
    if total == MINUTES_PER_DAY && allow_24_end {
        Ok(MINUTES_PER_DAY)
    } else {
        Ok(total % MINUTES_PER_DAY)
    }
}

pub fn compile_windows(cfg: &RulesConfig) -> Result<Vec<QuietWindow>, String> {
    if !(-24 * 60..=24 * 60).contains(&cfg.timezone_offset_minutes) {
        return Err("timezone_offset_minutes 必须在 -1440..=1440 内".into());
    }
    cfg.quiet_hours
        .iter()
        .map(|window| {
            let start = parse_clock(&window.start, false)?;
            let end = parse_clock(&window.end, true)?;
            if start == end || window.start == window.end {
                return Err(format!(
                    "静默窗口 start 和 end 不能相同: {}-{}",
                    window.start, window.end
                ));
            }
            Ok(QuietWindow { start, end })
        })
        .collect()
}

pub(crate) fn validate_config(cfg: &RulesConfig) -> Result<(), String> {
    if cfg.silent_keywords.len() > 128 {
        return Err("silent_keywords 最多 128 项".into());
    }
    if cfg
        .silent_keywords
        .iter()
        .any(|keyword| keyword.len() > 256)
    {
        return Err("silent_keywords 单项最多 256 bytes".into());
    }
    if cfg.quiet_hours.len() > 32 {
        return Err("quiet_hours 最多 32 个窗口".into());
    }
    if cfg.muted_accounts.len() > MAX_MUTED_ACCOUNTS {
        return Err(format!("muted_accounts 最多 {MAX_MUTED_ACCOUNTS} 项"));
    }
    for account in &cfg.muted_accounts {
        if account.trim().is_empty() {
            return Err("muted_accounts 不能包含空项".into());
        }
        if account.len() > MAX_MUTED_ACCOUNT_BYTES {
            return Err(format!(
                "muted_accounts 单项最多 {MAX_MUTED_ACCOUNT_BYTES} bytes"
            ));
        }
        if account.chars().any(char::is_control) {
            return Err("muted_accounts 不能包含控制字符".into());
        }
    }
    compile_windows(cfg).map(|_| ())
}

pub fn is_quiet_at(cfg: &RulesConfig, at_unix_secs: u64) -> bool {
    let Ok(windows) = compile_windows(cfg) else {
        return false;
    };
    if cfg.follow_system_timezone {
        if let Some(parts) = crate::localtime::local_parts(at_unix_secs) {
            let local_minutes = u16::from(parts.hour) * 60 + u16::from(parts.minute);
            return is_quiet_at_minute(&windows, local_minutes);
        }
    }
    is_quiet_with_windows(&windows, cfg.timezone_offset_minutes, at_unix_secs)
}

pub fn is_quiet_with_windows(
    windows: &[QuietWindow],
    timezone_offset_minutes: i32,
    at_unix_secs: u64,
) -> bool {
    let day_secs = (at_unix_secs % 86_400) as i64;
    let local_secs = day_secs + i64::from(timezone_offset_minutes) * 60;
    let local_minutes = (local_secs.rem_euclid(86_400) / 60) as u16;
    is_quiet_at_minute(windows, local_minutes)
}

fn is_quiet_at_minute(windows: &[QuietWindow], local_minutes: u16) -> bool {
    windows.iter().any(|window| {
        if window.start < window.end {
            local_minutes >= window.start && local_minutes < window.end
        } else {
            local_minutes >= window.start || local_minutes < window.end
        }
    })
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// rules 插件：inject 配置 → 挂 `notify/route` bail 策略 + 普适新邮件通知。
///
/// 持久化：`persist = true` 时侧车路径取 [`RulesPlugin::with_state_path`] 显式给出的，
/// 否则取上下文里的 [`RulesStatePath`] 服务（装配方须在 load 前 provide）；两者都没有时
/// 退回非持久模式。`persist = false` 时永远非持久，即使给了路径。
pub struct RulesPlugin {
    state_path: Option<PathBuf>,
}

impl RulesPlugin {
    pub fn new(_config: &RulesConfig) -> Self {
        Self { state_path: None }
    }

    /// 显式指定 `rules_state.toml` 路径（仍受 `persist` 开关约束）。
    pub fn with_state_path(_config: &RulesConfig, path: PathBuf) -> Self {
        Self {
            state_path: Some(path),
        }
    }
}

impl Plugin for RulesPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<RulesConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<RulesConfig>().expect("inject 就绪");
        validate_config(&cfg).map_err(|error| format!("rules 配置错误: {error}"))?;
        let state_path = if cfg.persist {
            self.state_path
                .clone()
                .or_else(|| ctx.get::<RulesStatePath>().map(|path| path.0.clone()))
        } else {
            None
        };
        let settings = match state_path {
            Some(path) => RulesSettings::open_persistent(cfg.as_ref().clone(), path),
            None => {
                if cfg.persist {
                    eprintln!("[maild/rules] persist = true 但未提供 rules_state.toml 路径，规则仅保存在内存");
                }
                RulesSettings::ephemeral(cfg.as_ref().clone())
            }
        };
        ctx.provide::<RulesSettings>(Arc::new(settings.clone()));
        mail_notify::mount(ctx, settings.clone());
        let settings_for_route = settings.clone();
        ctx.on_bail_scoped::<NotifyRouteEvent>(move |route| {
            decide(&route, &settings_for_route.snapshot())
        });

        let settings_for_query = settings;
        ctx.on_bail_scoped::<QuietHoursQuery>(move |request: QuietHoursRequest| {
            Some(is_quiet_at(&settings_for_query.snapshot(), request.at_unix_secs))
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `setSettings { persist: true }` 的内核语义：本进程改过的字段与磁盘覆盖层合成后落盘；
    /// 侧车不安全时拒绝且不碰文件；挂载后幂等。
    #[test]
    fn enable_persistence_merges_runtime_edits_and_never_overwrites_a_bad_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RULES_STATE_FILE);
        let settings = RulesSettings::ephemeral(RulesConfig::default());
        settings
            .try_patch(&RulesPatch {
                silent_keywords: Some(vec!["周报".into()]),
                ..Default::default()
            })
            .unwrap();
        assert!(!settings.is_persisted());
        assert!(!path.exists());

        let config = settings.enable_persistence(path.clone()).unwrap();
        assert!(settings.is_persisted());
        assert_eq!(config.silent_keywords, ["周报"]);
        assert_eq!(settings.state_path(), Some(path.as_path()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("周报"), "{text}");
        assert!(!text.contains("show_sensitive"), "只记改过的字段: {text}");
        // 幂等：已持久再请求不报错。
        settings.enable_persistence(path.clone()).unwrap();
        settings
            .try_patch(&RulesPatch {
                notify_new_mail: Some(true),
                ..Default::default()
            })
            .unwrap();
        let reopened = RulesSettings::open_persistent(RulesConfig::default(), path.clone());
        assert!(reopened.snapshot().notify_new_mail);
        assert_eq!(reopened.snapshot().silent_keywords, ["周报"]);

        // 损坏的侧车：拒绝挂载、保持非持久、文件逐字节不动。
        let bad_dir = tempfile::tempdir().unwrap();
        let bad = bad_dir.path().join(RULES_STATE_FILE);
        std::fs::write(&bad, "format_version = 99\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let fresh = RulesSettings::ephemeral(RulesConfig::default());
        assert!(matches!(
            fresh.enable_persistence(bad.clone()),
            Err(RulesPatchError::Unavailable(_))
        ));
        assert!(!fresh.is_persisted());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "format_version = 99\n");
        fresh
            .try_patch(&RulesPatch {
                show_sensitive: Some(false),
                ..Default::default()
            })
            .expect("挂载失败后仍可按内存模式写入");
    }

    fn route(subject: &str) -> NotifyRoute {
        NotifyRoute {
            channel: "system".into(),
            sensitive: true,
            subject: subject.into(),
            from: "s@t".into(),
        }
    }

    #[test]
    fn sensitive_switch_silences() {
        let cfg = RulesConfig {
            show_sensitive: false,
            silent_keywords: vec![],
            ..RulesConfig::default()
        };
        assert_eq!(
            decide(&route("验证码"), &cfg).map(|r| r.channel),
            Some("silent".into())
        );
        // 非敏感通知不受影响
        assert_eq!(
            decide(
                &NotifyRoute {
                    sensitive: false,
                    ..route("x")
                },
                &cfg
            ),
            None
        );
    }

    #[test]
    fn keyword_silences() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec!["广告".into()],
            ..RulesConfig::default()
        };
        assert_eq!(
            decide(&route("双十一广告"), &cfg).map(|r| r.channel),
            Some("silent".into())
        );
        assert_eq!(decide(&route("正常邮件"), &cfg), None);
    }

    #[test]
    fn empty_keyword_ignored() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec!["".into()],
            ..RulesConfig::default()
        };
        assert_eq!(decide(&route("任意"), &cfg), None, "空关键词不误杀");
    }

    #[test]
    fn quiet_hours_supports_overnight_windows_and_offset() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec![],
            quiet_hours: vec![QuietHoursConfig {
                start: "22:00".into(),
                end: "07:00".into(),
            }],
            timezone_offset_minutes: 480,
            ..RulesConfig::default()
        };
        // 2026-01-01 14:30 UTC = 22:30 China time.
        let at = 1_767_277_800;
        assert!(is_quiet_at(&cfg, at));
        // 2026-01-01 01:30 UTC = 09:30 China time.
        assert!(!is_quiet_at(&cfg, 1_767_231_400));
    }

    #[test]
    fn quiet_hours_silences_route_at_requested_time() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec![],
            quiet_hours: vec![QuietHoursConfig {
                start: "00:00".into(),
                end: "01:00".into(),
            }],
            timezone_offset_minutes: 0,
            ..RulesConfig::default()
        };
        let route = route("正常邮件");
        assert_eq!(
            decide_at(&route, &cfg, 1_767_225_600).map(|route| route.channel),
            Some("silent".into())
        );
    }

    #[test]
    fn quiet_hours_after_u16_seconds_boundary_are_not_wrapped() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec![],
            quiet_hours: vec![QuietHoursConfig {
                start: "18:45".into(),
                end: "18:48".into(),
            }],
            timezone_offset_minutes: 0,
            ..RulesConfig::default()
        };
        // 18:46 UTC has 67,560 seconds since midnight, which does not fit u16.
        assert!(is_quiet_at(&cfg, 67_560));
    }

    #[test]
    fn invalid_quiet_hours_are_rejected() {
        let cfg = RulesConfig {
            show_sensitive: true,
            silent_keywords: vec![],
            quiet_hours: vec![QuietHoursConfig {
                start: "noon".into(),
                end: "01:00".into(),
            }],
            timezone_offset_minutes: 0,
            ..RulesConfig::default()
        };
        assert!(compile_windows(&cfg).is_err());

        for (start, end) in [
            ("22:00", "22:00"),
            ("00:00", "00:00"),
            ("24:00", "00:00"),
            ("24:00", "24:00"),
        ] {
            let same = RulesConfig {
                quiet_hours: vec![QuietHoursConfig {
                    start: start.into(),
                    end: end.into(),
                }],
                ..RulesConfig::default()
            };
            assert!(compile_windows(&same).is_err(), "{start}-{end} must fail");
        }
    }

    #[test]
    fn full_day_and_midnight_end_windows_are_supported() {
        let all_day = RulesConfig {
            quiet_hours: vec![QuietHoursConfig {
                start: "00:00".into(),
                end: "24:00".into(),
            }],
            ..RulesConfig::default()
        };
        let windows = compile_windows(&all_day).expect("00:00-24:00 should be valid");
        assert_eq!(windows, vec![QuietWindow { start: 0, end: 1440 }]);
        for sample in [0_u64, 60, 43_200, 86_399, 1_767_225_600] {
            assert!(is_quiet_at(&all_day, sample), "sample={sample}");
        }

        let late_evening = RulesConfig {
            quiet_hours: vec![QuietHoursConfig {
                start: "22:00".into(),
                end: "24:00".into(),
            }],
            ..RulesConfig::default()
        };
        assert!(!is_quiet_at(&late_evening, 21 * 3600 + 59 * 60));
        assert!(is_quiet_at(&late_evening, 22 * 3600));
        assert!(is_quiet_at(&late_evening, 23 * 3600 + 59 * 60));
        assert!(!is_quiet_at(&late_evening, 0));
    }

    #[test]
    fn follow_system_timezone_persists_and_overrides_fixed_offset_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RULES_STATE_FILE);
        let settings = RulesSettings::open_persistent(
            RulesConfig {
                persist: true,
                ..RulesConfig::default()
            },
            path.clone(),
        );
        assert!(!settings.snapshot().follow_system_timezone);
        assert!(
            serde_json::to_value(RulesConfig::default())
                .unwrap()
                .get("follow_system_timezone")
                .is_none(),
            "default false stays omitted on wire"
        );

        let updated = settings
            .try_patch(&RulesPatch {
                follow_system_timezone: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(updated.follow_system_timezone);
        assert_eq!(
            serde_json::to_value(&updated).unwrap()["follow_system_timezone"],
            true
        );

        let reopened = RulesSettings::open_persistent(
            RulesConfig {
                persist: true,
                ..RulesConfig::default()
            },
            path,
        );
        assert!(reopened.snapshot().follow_system_timezone);

        #[cfg(unix)]
        {
            let ts = 1_767_225_600_u64;
            let parts = crate::localtime::local_parts(ts).expect("unix local_parts");
            let local_minute = u16::from(parts.hour) * 60 + u16::from(parts.minute);
            let window = QuietHoursConfig {
                start: format!("{:02}:{:02}", local_minute / 60, local_minute % 60),
                end: format!(
                    "{:02}:{:02}",
                    ((local_minute + 30) % MINUTES_PER_DAY) / 60,
                    (local_minute + 30) % 60
                ),
            };
            // Deliberately shift fixed offset 12 hours away from system local offset:
            // follow_system_timezone = true must follow local_parts, not timezone_offset_minutes.
            let shifted_offset =
                ((parts.utc_offset_minutes + 720 + 1440).rem_euclid(2880)) - 1440;
            let cfg = RulesConfig {
                quiet_hours: vec![window],
                timezone_offset_minutes: shifted_offset,
                follow_system_timezone: true,
                ..RulesConfig::default()
            };
            assert!(is_quiet_at(&cfg, ts));
            let fixed_cfg = RulesConfig {
                follow_system_timezone: false,
                ..cfg.clone()
            };
            assert!(!is_quiet_at(&fixed_cfg, ts));

            // Out-of-range u64 falls back cleanly to fixed offset without panicking.
            let fallback_all_day = RulesConfig {
                quiet_hours: vec![QuietHoursConfig {
                    start: "00:00".into(),
                    end: "24:00".into(),
                }],
                follow_system_timezone: true,
                ..RulesConfig::default()
            };
            assert!(is_quiet_at(&fallback_all_day, u64::MAX));
        }
    }
}
