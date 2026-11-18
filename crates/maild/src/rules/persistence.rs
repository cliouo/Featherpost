//! `rules_state.toml`：rules 运行时设置的原子侧车持久化（ADR-0043）。
//!
//! 纪律逐条对齐 `reading_policy.toml` / `plugin_state.toml`：0600、单硬链接、拒符号链接、
//! 64 KiB 上限、未知 `format_version` fail-closed、未知字段拒绝、临时文件 + fsync + 原子
//! rename + 父目录 fsync。
//!
//! 磁盘上只记录**用户经 `setSettings` 改过的字段**（[`RulesPatch`] 形态的覆盖层），
//! 不是完整快照：用户从未在界面碰过的字段继续跟随 TOML 基线，升级 TOML 默认值不会被一份
//! 旧侧车静默盖住。

use std::path::{Path, PathBuf};

use cordis::Service;
use serde::{Deserialize, Serialize};

use super::{validate_config, RulesConfig, RulesPatch};

/// 侧车文件名（活动配置同目录）。
pub const RULES_STATE_FILE: &str = "rules_state.toml";
const FORMAT_VERSION: u64 = 1;
const MAX_STATE_BYTES: u64 = 64 * 1024;

/// 装配方（`paths.rs`）在 load 前 provide 的侧车路径；`RulesPlugin` 在 `persist = true`
/// 时据此打开持久模式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesStatePath(pub PathBuf);

impl Service for RulesStatePath {
    fn name(&self) -> &'static str {
        "rules.state_path"
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    format_version: u64,
    #[serde(default)]
    rules: RulesPatch,
}

/// 侧车路径（活动配置同目录）。
pub fn rules_state_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(RULES_STATE_FILE)
}

/// 读取覆盖层。文件不存在 → 空覆盖（不制造状态文件）。任何不安全/不合法 → `Err`，
/// 调用方停摆且不得覆盖该文件。
pub(super) fn load(path: &Path, base: &RulesConfig) -> Result<RulesPatch, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RulesPatch::default())
        }
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    crate::plugin_state::check_owned_file(&metadata)?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(format!("超过 {MAX_STATE_BYTES} 字节上限"));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(format!("超过 {MAX_STATE_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    // 先只看版本：未来版本可能带着当前版本不认识的字段，报「未来版本」比报「未知字段」诚实。
    let version = toml::from_str::<toml::Table>(&text)
        .map_err(|error| format!("解析失败: {error}"))?
        .get("format_version")
        .and_then(toml::Value::as_integer);
    match version {
        Some(version) if version == FORMAT_VERSION as i64 => {}
        Some(version) => {
            return Err(format!(
                "未知 format_version {version}（当前支持 {FORMAT_VERSION}）"
            ))
        }
        None => return Err("缺少 format_version".into()),
    }
    let file: StateFile = toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    validate_config(&file.rules.apply_to(base)).map_err(|error| format!("规则非法: {error}"))?;
    Ok(file.rules)
}

/// 原子写入覆盖层（0600 临时文件 + fsync + rename + 父目录 fsync）。
pub(super) fn save(path: &Path, overlay: &RulesPatch) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("规则状态文件路径缺少父目录: {path:?}"))?;
    let file = StateFile {
        format_version: FORMAT_VERSION,
        rules: overlay.clone(),
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("序列化失败: {error}"))?;
    if body.len() as u64 > MAX_STATE_BYTES {
        return Err(format!("序列化结果超过 {MAX_STATE_BYTES} 字节上限"));
    }
    let temp = parent.join(format!(
        ".{RULES_STATE_FILE}.tmp-{}",
        crate::plugin_identity::generate_token()
    ));
    let result = crate::plugin_state::publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_sits_next_to_config() {
        assert_eq!(
            rules_state_path_for(&Path::new("/etc").join(maild_brand::SLUG).join(maild_brand::CONFIG_FILE_NAME)),
            Path::new("/etc").join(maild_brand::SLUG).join("rules_state.toml")
        );
        assert_eq!(
            rules_state_path_for(Path::new(maild_brand::CONFIG_FILE_NAME)),
            PathBuf::from("./rules_state.toml")
        );
    }

    #[test]
    fn missing_file_is_empty_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = load(&dir.path().join(RULES_STATE_FILE), &RulesConfig::default()).unwrap();
        assert_eq!(overlay, RulesPatch::default());
    }
}
