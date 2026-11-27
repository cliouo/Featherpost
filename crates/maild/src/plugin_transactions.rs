//! 插件安装事务（ADR-0019 §3）：受管目录的唯一写者是 Agent。
//!
//! 目录约定：`.incoming/<rand>/`（CLI staging）、`.removed/`（移除暂存）、
//! `.rollback/<id>/`（单深度回滚位）。点前缀目录被扫描器显式跳过，永远不会
//! 被激活或计数；崩溃残留不阻塞后续扫描与事务。

use std::fs;
use std::path::{Path, PathBuf};

use crate::external::version_tuple;

pub(crate) const INCOMING: &str = ".incoming";
pub(crate) const REMOVED: &str = ".removed";
pub(crate) const ROLLBACK: &str = ".rollback";

/// 事务对象的两类包命名空间。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxKind {
    External,
    Frontend,
}

impl TxKind {
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some("plugin") => Some(Self::External),
            Some("frontend") => Some(Self::Frontend),
            _ => None,
        }
    }

    fn validate_staged(&self, package: &Path) -> Result<(String, String), String> {
        match self {
            Self::External => {
                let record = crate::external::inspect_staged(package)?;
                Ok((record.id, record.version))
            }
            Self::Frontend => {
                let inspected = crate::frontend::inspect_staged_frontend(package)?;
                Ok((inspected.id, inspected.version))
            }
        }
    }

    fn inspect_published(&self, package: &Path) -> Result<(String, String), String> {
        match self {
            Self::External => {
                let record = crate::external::inspect_package(package)?;
                Ok((record.id, record.version))
            }
            Self::Frontend => crate::frontend::inspect_frontend_at(package),
        }
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    let lowered = id.to_ascii_lowercase();
    if id.is_empty()
        || id.len() > 128
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || id.contains("..")
        || lowered == "builtin"
        || lowered.starts_with("builtin.")
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("插件 id 无效: {id:?}"));
    }
    Ok(())
}

/// staging 名由 CLI 生成的随机段：只接受字母数字与连字符的短段，
/// 绝不接受路径分隔符、`..` 或点前缀。
pub(crate) fn valid_staging_name(name: &str) -> bool {
    (6..=64).contains(&name.len())
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !name.starts_with('-')
}

/// 安装/升级的提交结果。
#[derive(Clone, Debug, serde::Serialize)]
pub struct CommitOutcome {
    pub action: &'static str,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    pub version: String,
}

fn ensure_transaction_dirs(root: &Path) -> Result<PathBuf, String> {
    checked_directory(root)?;
    for dir in [INCOMING, REMOVED, ROLLBACK] {
        let path = root.join(dir);
        if fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            fs::create_dir(&path).map_err(|_| format!("{dir} 建立失败"))?;
        }
        checked_directory(&path)?;
    }
    Ok(root.to_path_buf())
}

fn checked_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "事务目录不可读取")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err("事务目录类型无效".into()); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("事务目录所有者或权限无效".into());
        }
    }
    Ok(())
}

/// Full validation for approval. No publication or executable launch happens here.
pub(crate) fn preview(
    root: &Path, kind: TxKind, staging: Option<&str>, rollback_id: Option<&str>,
) -> Result<maild_client::PackagePreview, String> {
    checked_directory(root)?;
    let (parent, child) = match (staging, rollback_id) {
        (Some(name), None) if valid_staging_name(name) => (INCOMING, name),
        (None, Some(id)) => { validate_id(id)?; (ROLLBACK, id) },
        _ => return Err("预览只接受一个 staging 或 rollback_id".into()),
    };
    checked_directory(&root.join(parent))?;
    let path = root.join(parent).join(child);
    let info = match kind {
        TxKind::External => {
            let r = crate::external::inspect_staged(&path)?;
            maild_client::PackagePreview { kind: "plugin".into(), id: r.id, name: r.name,
                description: r.text.description, l10n: r.text.l10n,
                version: r.version, lifecycle: r.lifecycle.as_str().into(), capabilities: r.capabilities,
                permissions: vec![], has_ui: r.ui_executable.is_some() }
        }
        TxKind::Frontend => {
            let r = crate::frontend::inspect_staged_frontend(&path)?.info;
            maild_client::PackagePreview { kind: "frontend".into(), id: r.id, name: r.name,
                description: r.description, l10n: r.l10n,
                version: r.version, lifecycle: r.lifecycle, capabilities: r.capabilities,
                permissions: r.permissions, has_ui: true }
        }
    };
    if let Some(id) = rollback_id {
        if info.id != id { return Err("回滚包身份不符".into()); }
    }
    Ok(info)
}

/// 提交 staging：目标不存在 → 安装；存在 → 升级（版本严格更高）。
/// 校验失败时删除坏 staging（不留现场）。调用方随后 refresh。
pub(crate) fn commit_install(root: &Path, kind: TxKind, staging: &str) -> Result<CommitOutcome, String> {
    if !valid_staging_name(staging) {
        return Err(format!("非法 staging 名: {staging:?}"));
    }
    let root = ensure_transaction_dirs(root)?;
    let staged = root.join(INCOMING).join(staging);
    if !staged.is_dir() {
        return Err(format!("staging 不存在: {staging}"));
    }
    let (id, version) = match kind.validate_staged(&staged) {
        Ok(info) => info,
        Err(reason) => {
            let _ = fs::remove_dir_all(&staged);
            return Err(format!("包校验失败: {reason}"));
        }
    };
    validate_id(&id)?;

    let target = root.join(&id);
    let outcome = if target.exists() {
        // 升级：当前版必须可解析、id 一致且严格更低；先移入回滚位再发布，失败即还原。
        let (published_id, current_version) = kind.inspect_published(&target).map_err(|reason| {
            let _ = fs::remove_dir_all(&staged);
            format!("在册包损坏，拒绝覆盖: {reason}")
        })?;
        if published_id != id {
            let _ = fs::remove_dir_all(&staged);
            return Err(format!("在册包 id 与提交不一致: {published_id} != {id}"));
        }
        let new_version = version.clone();
        match (version_tuple(&current_version), version_tuple(&new_version)) {
            (Some(current), Some(new)) if new > current => {}
            _ => {
                let _ = fs::remove_dir_all(&staged);
                return Err(format!(
                    "升级版本必须严格高于当前 {current_version}（得到 {new_version}）"
                ));
            }
        }
        let rollback = root.join(ROLLBACK).join(&id);
        fs::create_dir_all(rollback.parent().expect("rollback 有父目录"))
            .map_err(|error| format!("回滚位建立失败: {error}"))?;
        if rollback.exists() {
            fs::remove_dir_all(&rollback).map_err(|error| format!("旧回滚清理失败: {error}"))?;
        }
        fs::rename(&target, &rollback).map_err(|error| {
            let _ = fs::remove_dir_all(&staged);
            format!("当前版移入回滚位失败: {error}")
        })?;
        if let Err(error) = fs::rename(&staged, &target) {
            let _ = fs::rename(&rollback, &target);
            let _ = fs::remove_dir_all(&staged);
            return Err(format!("发布失败（已还原当前版）: {error}"));
        }
        CommitOutcome {
            action: "upgraded",
            id,
            from_version: Some(current_version),
            version,
        }
    } else {
        fs::rename(&staged, &target)
            .map_err(|error| format!("发布失败: {error}"))?;
        CommitOutcome {
            action: "installed",
            id,
            from_version: None,
            version,
        }
    };
    Ok(outcome)
}

/// 移除：改名进 `.removed/` 后删除（原子出目录，子进程由随后的 refresh 停止）。
pub(crate) fn remove_package(root: &Path, id: &str) -> Result<(), String> {
    validate_id(id)?;
    let root = ensure_transaction_dirs(root)?;
    let target = root.join(id);
    if !target.is_dir() {
        return Err(format!("插件未安装: {id}"));
    }
    let removed = root
        .join(REMOVED)
        .join(format!("{id}-{}", crate::plugin_identity::generate_token()));
    fs::rename(&target, &removed).map_err(|error| format!("移除失败: {error}"))?;
    if let Err(error) = fs::remove_dir_all(&removed) {
        eprintln!("[maild/auth] .removed 清理失败（不影响已移除状态）: {error}");
    }
    Ok(())
}

/// 回滚：当前版与新回滚位**互换**——旧版回到发布位，被替换的新版进回滚位；
/// 再执行一次即切回（来回切换）。仅要求两边版本不同（同版本互换无意义）。
pub(crate) fn rollback_package(root: &Path, kind: TxKind, id: &str) -> Result<String, String> {
    validate_id(id)?;
    let root = ensure_transaction_dirs(root)?;
    let rollback = root.join(ROLLBACK).join(id);
    let target = root.join(id);
    if !rollback.is_dir() {
        return Err(format!("没有可用回滚: {id}"));
    }
    if !target.is_dir() {
        return Err(format!("当前包缺失，无法换回（请直接安装）: {id}"));
    }
    let (_, current_version) = kind.inspect_published(&target)?;
    let (_, previous_version) = kind.inspect_published(&rollback)?;
    match (
        version_tuple(&current_version),
        version_tuple(&previous_version),
    ) {
        (Some(current), Some(previous)) if current != previous => {}
        _ => {
            return Err(format!(
                "回滚版本必须与当前 {current_version} 不同（得到 {previous_version}）"
            ));
        }
    }
    let swap = root
        .join(REMOVED)
        .join(format!("{id}-swap-{}", crate::plugin_identity::generate_token()));
    fs::create_dir_all(swap.parent().expect("swap 有父目录"))
        .map_err(|error| format!("交换区建立失败: {error}"))?;
    fs::rename(&target, &swap).map_err(|error| format!("当前版移出失败: {error}"))?;
    if let Err(error) = fs::rename(&rollback, &target) {
        let _ = fs::rename(&swap, &target);
        return Err(format!("回滚发布失败（已还原）: {error}"));
    }
    fs::rename(&swap, &rollback).map_err(|error| format!("新版本入回滚位失败: {error}"))?;
    Ok(previous_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "schema_version = 1\nprotocol_version = 1\nid = \"org.example.tx\"\nname = \"Tx\"\nversion = \"{version}\"\nlifecycle = \"independent\"\ncapabilities = [\"messages_read\"]\n";

    #[cfg(unix)]
    fn secure_root() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[cfg(not(unix))]
    fn secure_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn stage(root: &Path, staging: &str, version: &str) {
        let dir = root.join(INCOMING).join(staging);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.toml"),
            MANIFEST.replace("{version}", version),
        )
        .unwrap();
        seal(&dir);
    }

    fn seal(dir: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for entry in walk(dir) {
                fs::set_permissions(
                    &entry,
                    fs::Permissions::from_mode(if entry.is_dir() { 0o700 } else { 0o600 }),
                )
                .unwrap();
            }
        }
        #[cfg(not(unix))]
        let _ = dir;
    }

    #[cfg(unix)]
    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = vec![dir.to_path_buf()];
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn staging_name_rules_reject_paths_and_short_names() {
        assert!(valid_staging_name("abcd1234"));
        assert!(valid_staging_name("abcd-1234-ef01"));
        assert!(!valid_staging_name("abc"));
        assert!(!valid_staging_name("-abcd123"));
        assert!(!valid_staging_name("../evil"));
        assert!(!valid_staging_name("a/bcdef1"));
        assert!(!valid_staging_name(".hidden1"));
    }

    #[test]
    fn install_then_upgrade_then_rollback_lifecycle() {
        let dir = secure_root();
        stage(dir.path(), "stage0001", "1.0.0");
        // 安装。
        let outcome =
            commit_install(dir.path(), TxKind::External, "stage0001").unwrap();
        assert_eq!(outcome.action, "installed");
        assert_eq!(outcome.id, "org.example.tx");
        assert!(dir.path().join("org.example.tx").is_dir());

        // 同版本重装拒绝（no-clobber / 版本必须严格更高）。
        stage(dir.path(), "stage0002", "1.0.0");
        assert!(commit_install(dir.path(), TxKind::External, "stage0002").is_err());
        assert!(!dir.path().join(INCOMING).join("stage0002").exists(), "坏请求现场被清");

        // 低版本升级拒绝并清理 staging。
        stage(dir.path(), "stage0003", "0.9.0");
        assert!(commit_install(dir.path(), TxKind::External, "stage0003").is_err());
        assert!(!dir.path().join(INCOMING).join("stage0003").exists());

        // 高版本升级成功且留下回滚位。
        stage(dir.path(), "stage0004", "1.1.0");
        let upgrade = commit_install(dir.path(), TxKind::External, "stage0004").unwrap();
        assert_eq!(upgrade.action, "upgraded");
        assert_eq!(upgrade.from_version.as_deref(), Some("1.0.0"));
        let rollback_manifest =
            dir.path().join(ROLLBACK).join("org.example.tx").join("plugin.toml");
        assert!(rollback_manifest.is_file(), "升级后回滚位应保留上一版");

        // 回滚换回 1.0.0；再换回等于前进到 1.1.0。
        let back = rollback_package(dir.path(), TxKind::External, "org.example.tx").unwrap();
        assert_eq!(back, "1.0.0");
        let forward = rollback_package(dir.path(), TxKind::External, "org.example.tx").unwrap();
        assert_eq!(forward, "1.1.0");

        // 移除后目录消失。
        remove_package(dir.path(), "org.example.tx").unwrap();
        assert!(!dir.path().join("org.example.tx").exists());
    }
}
