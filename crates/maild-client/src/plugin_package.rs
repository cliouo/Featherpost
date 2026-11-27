//! Local plugin package inspection and staging shared by graphical and CLI clients.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const MANIFEST_NAME: &str = "plugin.toml";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 4096;
const MAX_PACKAGE_DEPTH: usize = 32;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(target_os = "macos")]
const QUARANTINE_XATTR: &[u8] = b"com.apple.quarantine\0";
#[cfg(target_os = "macos")]
const MAX_QUARANTINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginPackageKind {
    External,
    Frontend,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPackageSummary {
    pub source: PathBuf,
    pub id: String,
    pub name: String,
    pub version: String,
    pub lifecycle: String,
    pub has_ui: bool,
}

#[derive(Debug)]
pub struct StagedPluginPackage {
    staging: String,
    path: PathBuf,
}

impl StagedPluginPackage {
    pub fn staging(&self) -> &str {
        &self.staging
    }
}

impl Drop for StagedPluginPackage {
    fn drop(&mut self) {
        if self.path.file_name().and_then(|name| name.to_str()) == Some(self.staging.as_str()) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema_version: u64,
    protocol_version: u64,
    id: String,
    name: String,
    version: String,
    lifecycle: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    ui_executable: Option<PathBuf>,
    /// ADR-0048 决策 5：仅 `schema_version = 2` 起允许。
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    l10n: std::collections::BTreeMap<String, crate::PackageL10nEntry>,
}

/// Read enough package metadata to present an exact install confirmation. The daemon remains the
/// authority and repeats full manifest, capability, executable and version validation at commit.
pub fn inspect_plugin_package(source: &Path) -> Result<PluginPackageSummary, String> {
    let source = checked_source_directory(source)?;
    let manifest_path = source.join(MANIFEST_NAME);
    let metadata = fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("无法读取 plugin.toml: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("plugin.toml 必须是普通文件".into());
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err("plugin.toml 过大".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&manifest_path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| format!("无法读取 plugin.toml: {error}"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "plugin.toml 必须是 UTF-8")?;
    let manifest: PackageManifest =
        toml::from_str(text).map_err(|error| format!("plugin.toml 无效: {error}"))?;
    if !matches!(manifest.schema_version, 1 | 2) || manifest.protocol_version != 1 {
        return Err("插件包协议版本不受支持".into());
    }
    if manifest.schema_version < 2
        && (manifest.description.is_some() || !manifest.l10n.is_empty())
    {
        return Err("description 与 l10n 需要 schema_version = 2".into());
    }
    if manifest.id.trim().is_empty()
        || manifest.name.trim().is_empty()
        || manifest.version.trim().is_empty()
        || manifest.lifecycle.trim().is_empty()
    {
        return Err("plugin.toml 缺少 id、name、version 或 lifecycle".into());
    }
    let lowered_id = manifest.id.to_ascii_lowercase();
    if lowered_id == "builtin" || lowered_id.starts_with("builtin.") {
        return Err("plugin.toml id 不能占用保留命名空间 builtin.*".into());
    }
    let _ = (manifest.capabilities, manifest.executable);
    Ok(PluginPackageSummary {
        source,
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        lifecycle: manifest.lifecycle,
        has_ui: manifest.ui_executable.is_some(),
    })
}

/// Copy a package into a fresh managed `.incoming/<random>` directory. Symlinks and special files
/// are rejected so package selection cannot copy data from outside the selected directory.
pub fn stage_plugin_package(
    source: &Path,
    kind: PluginPackageKind,
) -> Result<StagedPluginPackage, String> {
    let source = checked_source_directory(source)?;
    let paths =
        maild_paths::AppPaths::discover().map_err(|error| format!("平台目录发现失败: {error}"))?;
    let root = match kind {
        PluginPackageKind::External => paths.plugin_dir,
        PluginPackageKind::Frontend => paths.frontend_dir,
    };
    stage_plugin_package_into(&source, &root)
}

pub fn stage_plugin_package_into(source: &Path, root: &Path) -> Result<StagedPluginPackage, String> {
    let source = checked_source_directory(source)?;
    ensure_directory(root, "插件目录")?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("插件目录不可解析: {error}"))?;
    if root.starts_with(&source) {
        return Err("插件包源目录不能与受管插件目录重叠".into());
    }
    let incoming = root.join(".incoming");
    ensure_directory(&incoming, "插件 staging 目录")?;

    let mut last_error = None;
    for _ in 0..8 {
        let staging = random_staging_name()?;
        let target = incoming.join(&staging);
        match create_staging_directory(&target) {
            Ok(()) => {
                let mut limits = CopyLimits::default();
                let copied = preserve_quarantine_path(&source, &target)
                    .map_err(|error| error.to_string())
                    .and_then(|()| copy_directory_contents(&source, &target, &mut limits, 0));
                if let Err(error) = copied {
                    let _ = fs::remove_dir_all(&target);
                    return Err(format!("复制到 staging 失败: {error}"));
                }
                return Ok(StagedPluginPackage {
                    staging,
                    path: target,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
            Err(error) => return Err(format!("staging 建立失败: {error}")),
        }
    }
    Err(format!(
        "无法分配 staging 目录: {}",
        last_error.map_or_else(|| "随机名称冲突".into(), |error| error.to_string())
    ))
}

fn checked_source_directory(source: &Path) -> Result<PathBuf, String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| format!("源目录不可读取: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("源必须是非符号链接目录".into());
    }
    validate_source_permissions(&metadata)?;
    source
        .canonicalize()
        .map_err(|error| format!("源目录不可解析: {error}"))
}

#[cfg(unix)]
fn validate_source_permissions(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err("插件包只能包含当前用户所有且不可被 group/other 写入的条目".into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_source_permissions(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

fn ensure_directory(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!("{label}必须是非符号链接目录"));
        }
        Ok(metadata) => {
            validate_source_permissions(&metadata)?;
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("检查{label}失败: {error}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|error| format!("建立{label}失败: {error}"))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path).map_err(|error| format!("建立{label}失败: {error}"))?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("检查{label}失败: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label}必须是非符号链接目录"));
    }
    validate_source_permissions(&metadata)?;
    Ok(())
}

fn create_staging_directory(path: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[derive(Default)]
struct CopyLimits {
    entries: usize,
    bytes: u64,
}

fn copy_directory_contents(
    source: &Path,
    target: &Path,
    limits: &mut CopyLimits,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_PACKAGE_DEPTH {
        return Err("插件包目录层级过深".into());
    }
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let destination = target.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(format!("插件包不能包含符号链接: {:?}", entry.file_name()));
        }
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        validate_source_permissions(&metadata)?;
        limits.entries = limits.entries.saturating_add(1);
        if limits.entries > MAX_PACKAGE_ENTRIES {
            return Err("插件包超过目录项数量限制".into());
        }
        if file_type.is_dir() {
            if depth == MAX_PACKAGE_DEPTH {
                return Err("插件包目录层级过深".into());
            }
            create_staging_directory(&destination).map_err(|error| error.to_string())?;
            preserve_quarantine_path(&entry.path(), &destination)
                .map_err(|error| error.to_string())?;
            copy_directory_contents(&entry.path(), &destination, limits, depth + 1)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(format!("插件包不能包含特殊文件: {:?}", entry.file_name()));
        }
        limits.bytes = limits.bytes.saturating_add(metadata.len());
        if limits.bytes > MAX_PACKAGE_BYTES {
            return Err("插件包超过大小限制".into());
        }
        copy_regular_file(&entry.path(), &destination, &metadata)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn copy_regular_file(source: &Path, target: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut input = options.open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut bounded = (&mut input).take(metadata.len().saturating_add(1));
    let copied = io::copy(&mut bounded, &mut output)?;
    if copied != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "源文件在复制期间发生变化",
        ));
    }
    output.flush()?;
    preserve_quarantine_file(&input, &output)?;
    output.set_permissions(metadata.permissions())
}

#[cfg(target_os = "macos")]
fn preserve_quarantine_file(source: &File, target: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let Some(value) = read_quarantine_fd(source.as_raw_fd())? else {
        return Ok(());
    };
    let result = unsafe {
        libc::fsetxattr(
            target.as_raw_fd(),
            QUARANTINE_XATTR.as_ptr().cast(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn preserve_quarantine_file(_source: &File, _target: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_quarantine_fd(fd: std::os::fd::RawFd) -> io::Result<Option<Vec<u8>>> {
    let size = unsafe {
        libc::fgetxattr(
            fd,
            QUARANTINE_XATTR.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let size = usize::try_from(size).map_err(|_| io::Error::other("隔离属性长度无效"))?;
    if size > MAX_QUARANTINE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "隔离属性过大"));
    }
    let mut value = vec![0u8; size];
    let read = unsafe {
        libc::fgetxattr(
            fd,
            QUARANTINE_XATTR.as_ptr().cast(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(read).ok() != Some(value.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "隔离属性在复制期间发生变化",
        ));
    }
    Ok(Some(value))
}

#[cfg(target_os = "macos")]
fn preserve_quarantine_path(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let source = std::ffi::CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "源路径包含 NUL"))?;
    let target = std::ffi::CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "目标路径包含 NUL"))?;
    let size = unsafe {
        libc::getxattr(
            source.as_ptr(),
            QUARANTINE_XATTR.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let size = usize::try_from(size).map_err(|_| io::Error::other("隔离属性长度无效"))?;
    if size > MAX_QUARANTINE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "隔离属性过大"));
    }
    let mut value = vec![0u8; size];
    let read = unsafe {
        libc::getxattr(
            source.as_ptr(),
            QUARANTINE_XATTR.as_ptr().cast(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(read).ok() != Some(value.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "隔离属性在复制期间发生变化",
        ));
    }
    let result = unsafe {
        libc::setxattr(
            target.as_ptr(),
            QUARANTINE_XATTR.as_ptr().cast(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn preserve_quarantine_path(_source: &Path, _target: &Path) -> io::Result<()> {
    Ok(())
}

fn random_staging_name() -> Result<String, String> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| format!("系统随机源不可用: {error}"))?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(24), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> &'static str {
        "schema_version = 1\nprotocol_version = 1\nid = \"org.example.mail\"\nname = \"Example Mail\"\nversion = \"1.2.3\"\nlifecycle = \"supervised\"\ncapabilities = [\"transport_provider\"]\nexecutable = \"bin/provider\"\nui_executable = \"bin/ui\"\n"
    }

    #[test]
    fn inspection_returns_confirmation_metadata() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(MANIFEST_NAME), manifest()).unwrap();
        let summary = inspect_plugin_package(root.path()).unwrap();
        assert_eq!(summary.id, "org.example.mail");
        assert_eq!(summary.version, "1.2.3");
        assert!(summary.has_ui);
    }

    #[test]
    fn inspection_accepts_schema_v2_translation_provider_and_rejects_builtin_namespace() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(MANIFEST_NAME),
            "schema_version = 2\nprotocol_version = 1\nid = \"org.example.translator\"\nname = \"Example Translator\"\ndescription = \"Local translation provider\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"translation_provider\"]\n[l10n.\"zh-Hans\"]\nname = \"示例翻译\"\n",
        )
        .unwrap();
        let summary = inspect_plugin_package(root.path()).unwrap();
        assert_eq!(summary.id, "org.example.translator");
        assert_eq!(summary.lifecycle, "independent");

        let v1_with_l10n = tempfile::tempdir().unwrap();
        fs::write(
            v1_with_l10n.path().join(MANIFEST_NAME),
            "schema_version = 1\nprotocol_version = 1\nid = \"org.example.bad\"\nname = \"Bad\"\ndescription = \"not allowed in v1\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"translation_provider\"]\n",
        )
        .unwrap();
        assert!(inspect_plugin_package(v1_with_l10n.path()).is_err());

        let builtin = tempfile::tempdir().unwrap();
        fs::write(
            builtin.path().join(MANIFEST_NAME),
            "schema_version = 2\nprotocol_version = 1\nid = \"builtin.openai_compatible\"\nname = \"Fake Builtin\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"translation_provider\"]\n",
        )
        .unwrap();
        assert!(inspect_plugin_package(builtin.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn copy_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(source.path().join("payload"), b"ok").unwrap();
        symlink(source.path().join("payload"), source.path().join("link")).unwrap();
        let mut limits = CopyLimits::default();
        assert!(
            copy_directory_contents(source.path(), target.path(), &mut limits, 0)
                .unwrap_err()
                .contains("符号链接")
        );
    }

    #[test]
    fn copy_rejects_excessive_depth_and_counts_directories() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let mut current = source.path().to_path_buf();
        for index in 0..=MAX_PACKAGE_DEPTH {
            current.push(format!("level-{index}"));
            fs::create_dir(&current).unwrap();
        }
        let mut limits = CopyLimits::default();
        assert!(
            copy_directory_contents(source.path(), target.path(), &mut limits, 0)
                .unwrap_err()
                .contains("层级过深")
        );

        let shallow_source = tempfile::tempdir().unwrap();
        let shallow_target = tempfile::tempdir().unwrap();
        fs::create_dir(shallow_source.path().join("directory")).unwrap();
        let mut limits = CopyLimits {
            entries: MAX_PACKAGE_ENTRIES,
            bytes: 0,
        };
        assert!(copy_directory_contents(
            shallow_source.path(),
            shallow_target.path(),
            &mut limits,
            0,
        )
        .unwrap_err()
        .contains("目录项数量"));
    }

    #[test]
    fn staged_guard_cleans_uncommitted_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("abcdef");
        fs::create_dir(&path).unwrap();
        let guard = StagedPluginPackage {
            staging: "abcdef".into(),
            path: path.clone(),
        };
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn staging_copies_nested_files_and_guard_cleans_them() {
        let source = tempfile::tempdir().unwrap();
        let managed = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("bin")).unwrap();
        fs::write(source.path().join(MANIFEST_NAME), manifest()).unwrap();
        fs::write(source.path().join("bin/provider"), b"provider").unwrap();
        let staged = stage_plugin_package_into(source.path(), managed.path()).unwrap();
        let staged_path = managed.path().join(".incoming").join(staged.staging());
        assert_eq!(
            fs::read(staged_path.join("bin/provider")).unwrap(),
            b"provider"
        );
        drop(staged);
        assert!(!staged_path.exists());
    }

    #[test]
    fn staging_rejects_source_and_managed_root_overlap() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join(MANIFEST_NAME), manifest()).unwrap();
        let managed = source.path().join("managed");
        let error = stage_plugin_package_into(source.path(), &managed).unwrap_err();
        assert!(error.contains("不能与受管插件目录重叠"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staging_preserves_quarantine_on_executables() {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        let source = tempfile::tempdir().unwrap();
        let managed = tempfile::tempdir().unwrap();
        fs::write(source.path().join(MANIFEST_NAME), manifest()).unwrap();
        fs::create_dir(source.path().join("bin")).unwrap();
        let executable = source.path().join("bin/provider");
        fs::write(&executable, b"provider").unwrap();
        let path = std::ffi::CString::new(executable.as_os_str().as_bytes()).unwrap();
        let quarantine = b"0081;test;Maild;";
        assert_eq!(
            unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    QUARANTINE_XATTR.as_ptr().cast(),
                    quarantine.as_ptr().cast(),
                    quarantine.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            },
            0
        );

        let staged = stage_plugin_package_into(source.path(), managed.path()).unwrap();
        let copied = File::open(staged.path.join("bin/provider")).unwrap();
        assert_eq!(
            read_quarantine_fd(copied.as_raw_fd()).unwrap(),
            Some(quarantine.to_vec())
        );
    }
}
