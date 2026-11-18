//! plugin_state：supervised 插件启用状态（ADR-0019 §3）。
//!
//! `plugin_state.toml` 与活动配置同目录，仅由 Agent 写入，纪律对齐
//! plugin_auth.toml：0600、单硬链接、拒符号链接、64 KiB 上限、未知
//! `format_version` fail-closed、临时文件 + fsync + 原子 rename 发布。
//! TOML allowlist 是一次性种子：状态文件缺失时由装配导入，此后以本文件为准。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cordis::Service;
use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "plugin_state.toml";
const STATE_FORMAT_VERSION: u64 = 1;
const MAX_STATE_BYTES: u64 = 64 * 1024;
const MAX_ENABLED: usize = 128;

#[derive(Serialize, Deserialize)]
struct StateFile {
    format_version: u64,
    #[serde(default)]
    enabled: Vec<String>,
}

/// 状态文件的内存形态；损坏即停摆（supervised 拒绝启动，与身份缝同姿态）。
pub enum PluginState {
    /// 当前启用的 supervised id 集。
    Ready(BTreeSet<String>),
    /// 文件损坏/未来版本：有界诊断后拒绝一切状态变更，已启用集视为空。
    Unavailable,
}

impl PluginState {
    /// 从磁盘加载；文件不存在 → 空 Ready（首次安装不制造状态）。
    pub fn load(path: &Path) -> Self {
        match load_enabled(path) {
            Ok(enabled) => Self::Ready(enabled),
            Err(reason) => {
                eprintln!(
                    "[maild/auth] {} 无法加载，插件启用状态停摆（supervised 不会启动）: {reason}",
                    path.display()
                );
                Self::Unavailable
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub fn enabled(&self) -> BTreeSet<String> {
        match self {
            Self::Ready(enabled) => enabled.clone(),
            Self::Unavailable => BTreeSet::new(),
        }
    }

    /// 写入新启用集。停摆时返回 Err 且绝不写入。
    pub fn store(&self, path: &Path, enabled: &BTreeSet<String>) -> Result<(), String> {
        if !self.is_ready() {
            return Err("插件启用状态停摆".into());
        }
        if enabled.len() > MAX_ENABLED {
            return Err(format!("启用数超过 {MAX_ENABLED} 上限"));
        }
        for id in enabled {
            validate_id(id)?;
        }
        save_state(
            path,
            &StateFile {
                format_version: STATE_FORMAT_VERSION,
                enabled: enabled.iter().cloned().collect(),
            },
        )
    }
}

impl Service for PluginState {
    fn name(&self) -> &'static str {
        "plugin.state"
    }
}

fn validate_id(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > 128
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("插件 id 无效: {value:?}"));
    }
    Ok(())
}

fn load_enabled(path: &Path) -> Result<BTreeSet<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeSet::new())
        }
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    check_owned_file(&metadata)?;
    let bytes = std::fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(format!("超过 {MAX_STATE_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    let file: StateFile = toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    if file.format_version != STATE_FORMAT_VERSION {
        return Err(format!(
            "未来 format_version {}（当前支持 {STATE_FORMAT_VERSION}）",
            file.format_version
        ));
    }
    if file.enabled.len() > MAX_ENABLED {
        return Err(format!("启用数超过 {MAX_ENABLED} 上限"));
    }
    let mut enabled = BTreeSet::new();
    for id in &file.enabled {
        validate_id(id)?;
        if !enabled.insert(id.clone()) {
            return Err(format!("重复条目: {id}"));
        }
    }
    Ok(enabled)
}

#[cfg(unix)]
pub(crate) fn check_owned_file(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        return Err("必须由当前用户拥有".into());
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(format!("权限过宽: {:o}（要求 0600）", metadata.mode()));
    }
    if metadata.nlink() != 1 {
        return Err(format!("必须只有单个硬链接（当前 {}）", metadata.nlink()));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn check_owned_file(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

fn save_state(path: &Path, file: &StateFile) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("状态文件路径缺少父目录: {path:?}"))?;
    let body =
        toml::to_string_pretty(file).map_err(|error| format!("序列化失败: {error}"))?;
    let temp = parent.join(format!(".{STATE_FILE}.tmp-{}", crate::plugin_identity::generate_token()));
    let result = publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// 临时文件 + fsync + 原子 rename + 父目录 fsync。`reading_policy` 复用同一条纪律（ADR-0032）。
pub(crate) fn publish_atomically(temp: &Path, target: &Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temp)
            .map_err(|error| format!("临时文件创建失败: {error}"))?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|error| format!("临时文件创建失败: {error}"))?;
    file.write_all(body)
        .map_err(|error| format!("临时文件写入失败: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("临时文件落盘失败: {error}"))?;
    drop(file);
    std::fs::rename(temp, target).map_err(|error| format!("原子发布失败: {error}"))?;
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(
            target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|error| format!("父目录打开失败: {error}"))?;
        dir.sync_all()
            .map_err(|error| format!("父目录落盘失败: {error}"))?;
    }
    Ok(())
}

/// 状态文件路径（活动配置同目录）。
pub fn state_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(STATE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secure_dir() -> tempfile::TempDir {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            dir
        }
        #[cfg(not(unix))]
        tempfile::tempdir().unwrap()
    }

    fn write_state(dir: &Path, body: &str) {
        std::fs::write(dir.join(STATE_FILE), body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.join(STATE_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }

    #[test]
    fn missing_file_is_empty_ready_and_store_roundtrips() {
        let dir = secure_dir();
        let path = dir.path().join(STATE_FILE);
        let state = PluginState::load(&path);
        assert!(state.is_ready());
        assert!(state.enabled().is_empty());

        let mut enabled = BTreeSet::new();
        enabled.insert("org.example.a".to_string());
        state.store(&path, &enabled).unwrap();

        let reloaded = PluginState::load(&path);
        assert_eq!(reloaded.enabled(), enabled);
    }

    #[test]
    fn corrupt_or_future_state_fails_closed_without_cleanup() {
        let dir = secure_dir();
        let path = dir.path().join(STATE_FILE);
        write_state(dir.path(), "format_version = 99\n");
        let state = PluginState::load(&path);
        assert!(!state.is_ready());
        assert!(state.enabled().is_empty());
        assert!(state.store(&path, &BTreeSet::new()).is_err());
        assert!(path.exists(), "停摆绝不清理");

        write_state(
            dir.path(),
            "format_version = 1\nenabled = [\"bad..id\"]\n",
        );
        let state = PluginState::load(&path);
        assert!(!state.is_ready());
    }
}
