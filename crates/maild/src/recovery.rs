//! Fixed, bundled frontend recovery (ADR-0039). No caller-selected source or download.
//!
//! Online callers hold the API mutation lane and frontend-directory lease. Offline entry points
//! acquire Agent, frontend-directory and (when configured) database leases. This module
//! never opens SQLite, edits accounts, or starts a frontend executable.

mod files;
#[cfg(test)]
pub(crate) mod tests;

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use cordis::Service;
use maild_paths::AppPaths;
use serde::Serialize;

use crate::agent_lifecycle::AgentLifecycleLock;
use crate::store::{DatabaseLock, StoreConfig};

pub const BUNDLED_FRONTEND_ID: &str = maild_brand::BUNDLED_FRONTEND_ID;
const STATE: &str = ".default.json";
const BACKUPS: &str = ".repair-backups";

/// One-shot preinstall marker beside the installed packages (`.preinstalled-<frontend id>`).
fn marker() -> String {
    format!(".preinstalled-{BUNDLED_FRONTEND_ID}")
}

/// Default-selection state written by repair: the bundled frontend.
fn default_state() -> Vec<u8> {
    format!("{{\"version\":1,\"default\":\"{BUNDLED_FRONTEND_ID}\"}}\n").into_bytes()
}

/// Bundle-relative executables (launcher, Agent) that locate the top-level app bundle.
fn bundle_entry_points() -> [PathBuf; 2] {
    [
        Path::new("Contents/MacOS").join(maild_brand::LAUNCHER_BIN),
        Path::new("Contents/Library/LoginItems")
            .join(maild_brand::AGENT_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(maild_brand::AGENT_BIN),
    ]
}

/// A source derived from the running app bundle (`maild_brand::APP_BUNDLE_NAME`), never from RPC parameters or configuration.
pub struct BundledFrontend {
    app: PathBuf,
    source: PathBuf,
}

impl Service for BundledFrontend {
    fn name(&self) -> &'static str {
        "frontend.bundled-recovery"
    }
}

impl BundledFrontend {
    /// Bare development executables and old bundles have no recovery payload.
    pub fn discover() -> Result<Option<Self>, String> {
        Self::from_executable(&std::env::current_exe().map_err(|_| "无法确定应用位置")?)
    }

    fn from_executable(exe: &Path) -> Result<Option<Self>, String> {
        let suffixes = bundle_entry_points();
        let Some(app) = suffixes.iter().find_map(|suffix| {
            let count = suffix.components().count();
            let app = exe.ancestors().nth(count)?;
            (app.file_name()? == maild_brand::APP_BUNDLE_NAME && app.join(suffix) == exe)
                .then_some(app)
        }) else {
            return Ok(None);
        };
        // Validate every bundle-relative component, including the executable's enclosing app.
        files::check_chain(app, exe.parent().ok_or("应用位置无效")?)?;
        files::check_file(exe)?;
        let source = app
            .join("Contents/Resources/frontends")
            .join(BUNDLED_FRONTEND_ID);
        // A missing payload is expected in pre-09 bundles; an unsafe existing path is an error.
        if !files::check_optional_chain(app, &source)? {
            return Ok(None);
        }
        Ok(Some(Self {
            app: app.into(),
            source,
        }))
    }

    fn stage(
        &self,
        root: &Path,
    ) -> Result<(maild_client::StagedPluginPackage, PathBuf, String), String> {
        files::check_chain(&self.app, &self.source)?;
        let staged = maild_client::stage_plugin_package_into(&self.source, root)
            .map_err(|_| "内置前端复制失败，未替换现有包")?;
        let path = root.join(".incoming").join(staged.staging());
        let version = crate::frontend::inspect_bundled_frontend(&path)?;
        files::sync_tree(&path)?;
        Ok((staged, path, version))
    }

    /// Caller owns the frontend-directory lease plus the lifecycle or API mutation lane.
    /// The marker is durable BEFORE publication, so removal is never silently undone on retry.
    pub(crate) fn preinstall(&self, root: &Path) -> Result<PreinstallOutcome, String> {
        files::check_directory(root)?;
        if files::exists(&root.join(marker()))? {
            return Ok(PreinstallOutcome::AlreadyConsidered);
        }
        if files::exists(&root.join(BUNDLED_FRONTEND_ID))? {
            files::mark_once(root, &marker())?;
            return Ok(PreinstallOutcome::ExistingPreserved);
        }
        crate::frontend::check_recovery_capacity(root)?;
        let (_stage, staged, _) = self.stage(root)?;
        files::mark_once(root, &marker())?;
        files::rename_new(&staged, &root.join(BUNDLED_FRONTEND_ID))
            .map_err(|_| "首次预装未发布；现有条目保留，请使用显式修复")?;
        files::sync_directory(root)?;
        Ok(PreinstallOutcome::Installed)
    }

    /// App upgrade: replace the installed bundled frontend when the bundled copy has a strictly
    /// newer numeric version, or the same version with a different executable (a rebuilt or
    /// re-signed app installed over the old one, which the launcher would otherwise route to
    /// native recovery). Absent (user-removed), unreadable, newer, and same-version packages
    /// whose bytes already match are left alone; the default selection is never touched. Historical `protocol_version` values
    /// (`1..=API_PROTOCOL_VERSION`) are accepted so upgrades across a protocol bump still advance
    /// the installed copy. The old package is retained in `.repair-backups`. Caller owns the
    /// frontend-directory lease (Agent startup).
    pub(crate) fn refresh(&self, root: &Path) -> Result<RefreshOutcome, String> {
        self.refresh_up_to(root, crate::frontend::API_PROTOCOL_VERSION)
    }

    pub(crate) fn refresh_up_to(
        &self,
        root: &Path,
        max_protocol_version: u32,
    ) -> Result<RefreshOutcome, String> {
        files::check_directory(root)?;
        let target = root.join(BUNDLED_FRONTEND_ID);
        if !files::exists(&target)? {
            return Ok(RefreshOutcome::Absent);
        }
        // An unreadable installed package or a future protocol generation is left for explicit
        // repair, never guessed over or silently downgraded.
        let Ok(installed_version) =
            crate::frontend::inspect_installed_for_upgrade(&target, max_protocol_version)
        else {
            return Ok(RefreshOutcome::Kept);
        };
        let bundled = self.bundled_version()?;
        let same_version = match (parse_version(&installed_version), parse_version(&bundled)) {
            (Some(old), Some(new)) if new > old => false,
            // Same version, different build (rebuilt or re-signed app installed over the old
            // one): the installed copy is stale, not damaged, so it follows the bundled bytes.
            (Some(old), Some(new))
                if new == old
                    && crate::frontend::installed_bundled_frontend_is_stale(
                        &target,
                        &self.source,
                    ) =>
            {
                true
            }
            _ => return Ok(RefreshOutcome::Kept),
        };
        let (_stage, staged, version) = self.stage(root)?;
        let backups = root.join(BACKUPS);
        files::ensure_directory(&backups)?;
        let backup = tempfile::Builder::new()
            .prefix("upgrade-")
            .tempdir_in(&backups)
            .map_err(|_| "无法建立升级备份目录")?
            .keep();
        files::sync_directory(&backups)?;
        let old_package = backup.join(BUNDLED_FRONTEND_ID);
        files::rename_new(&target, &old_package).map_err(|_| "旧前端备份失败，未升级")?;
        files::sync_directory(root)?;
        files::sync_directory(&backup)?;
        if files::rename_new(&staged, &target).is_err() {
            // Put the original back; on an uncertain rollback both copies stay on disk.
            files::rename_new(&old_package, &target)
                .map_err(|_| "前端升级未完成，旧包保留于 .repair-backups，请从修复入口恢复")?;
            files::sync_directory(root)?;
            return Err("前端升级发布失败，已保留原包".into());
        }
        files::sync_directory(root)?;
        if same_version {
            return Ok(RefreshOutcome::Resynced { version });
        }
        Ok(RefreshOutcome::Upgraded {
            from: installed_version,
            to: version,
        })
    }

    /// Cheap pre-check from the bundled manifest; the staged copy is fully inspected before use.
    fn bundled_version(&self) -> Result<String, String> {
        let manifest = self.source.join("frontend.toml");
        files::check_file(&manifest)?;
        let mut text = String::new();
        fs::File::open(&manifest)
            .map_err(|_| "内置前端清单不可读取")?
            .take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "内置前端清单读取失败")?;
        let table: toml::Table = toml::from_str(&text).map_err(|_| "内置前端清单无效")?;
        table
            .get("version")
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "内置前端清单缺少版本".into())
    }

    /// Explicit replacement, including same-version/corrupt packages. Old bytes are retained.
    /// Caller must serialize this with all package/default mutations.
    pub(crate) fn repair(&self, root: &Path) -> Result<RepairOutcome, String> {
        self.repair_with(root, |_| Ok(()))
    }

    fn repair_with(
        &self,
        root: &Path,
        checkpoint: impl Fn(&str) -> Result<(), String>,
    ) -> Result<RepairOutcome, String> {
        files::check_directory(root)?;
        crate::frontend::check_recovery_capacity(root)?;
        let (_stage, staged, version) = self.stage(root)?;
        files::mark_once(root, &marker())?;
        let backups = root.join(BACKUPS);
        files::ensure_directory(&backups)?;
        let backup = tempfile::Builder::new()
            .prefix("repair-")
            .tempdir_in(&backups)
            .map_err(|_| "无法建立恢复备份目录")?
            .keep();
        // Backups are deliberately retained even after a failed transaction.
        files::sync_directory(&backups)?;
        let target = root.join(BUNDLED_FRONTEND_ID);
        let state = root.join(STATE);
        let old_package = backup.join(BUNDLED_FRONTEND_ID);
        let old_state = backup.join(STATE);
        let mut package_saved = false;
        let mut state_saved = false;
        let mut published = false;
        let mut state_published = false;
        let result: Result<(), String> = (|| {
            if files::exists(&target)? {
                files::rename_new(&target, &old_package).map_err(|_| "旧包备份失败")?;
                package_saved = true;
            }
            if files::exists(&state)? {
                // Rename the entry itself, even when damaged/a symlink. Never follow it.
                files::rename_new(&state, &old_state).map_err(|_| "旧默认选择备份失败")?;
                state_saved = true;
            }
            files::sync_directory(&backup)?;
            files::sync_directory(root)?;
            checkpoint("backed_up")?;
            files::rename_new(&staged, &target).map_err(|_| "内置前端发布失败")?;
            published = true;
            files::sync_directory(root)?;
            checkpoint("package_published")?;
            // No overwrite: an unexpected new choice aborts the transaction.
            let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| "默认选择暂存失败")?;
            use std::io::Write as _;
            temp.write_all(&default_state())
                .map_err(|_| "默认选择写入失败")?;
            temp.as_file().sync_all().map_err(|_| "默认选择落盘失败")?;
            temp.persist_noclobber(&state)
                .map_err(|_| "默认选择发布失败")?;
            state_published = true;
            files::sync_directory(root)?;
            checkpoint("state_published")?;
            Ok(())
        })();
        if let Err(error) = result {
            // Do not delete either candidate or original; retain both on uncertain rollback.
            let rollback = (|| {
                if state_published {
                    files::rename_new(&state, &backup.join("candidate-default.json"))?;
                }
                if published {
                    files::rename_new(&target, &backup.join("candidate"))?;
                }
                if package_saved {
                    files::rename_new(&old_package, &target)?;
                }
                if state_saved {
                    files::rename_new(&old_state, &state)?;
                }
                Ok::<_, std::io::Error>(())
            })();
            if rollback.is_err()
                || files::sync_directory(root).is_err()
                || files::sync_directory(&backup).is_err()
            {
                return Err(
                    "恢复未完成，回退状态不确定；旧数据保留于 .repair-backups，请勿自动重试".into(),
                );
            }
            return Err(format!("{error}；已回退，备份保留"));
        }
        Ok(RepairOutcome {
            id: BUNDLED_FRONTEND_ID.into(),
            version,
            backup: backup
                .file_name()
                .ok_or("恢复备份名称无效")?
                .to_string_lossy()
                .into_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreinstallOutcome {
    Installed,
    ExistingPreserved,
    AlreadyConsidered,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    Absent,
    Kept,
    Upgraded { from: String, to: String },
    /// Same version, but the installed executable no longer matches the bundled checksum.
    Resynced { version: String },
}

/// `X`, `X.Y` or `X.Y.Z` numeric versions (the bundle's CFBundleShortVersionString form).
/// Anything else is not comparable and therefore never triggers an automatic replacement.
fn parse_version(value: &str) -> Option<[u64; 3]> {
    let mut parts = [0u64; 3];
    let mut count = 0;
    for part in value.split('.') {
        if count == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts[count] = part.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(parts)
}

#[derive(Debug, Serialize)]
pub struct RepairOutcome {
    pub id: String,
    pub version: String,
    /// Relative child of `.repair-backups`, never a source path or mail identity.
    pub backup: String,
}

/// Native launcher's offline entry. A live Agent or CLI daemon makes this fail without repairs.
pub fn repair_offline() -> Result<RepairOutcome, String> {
    let source = BundledFrontend::discover()?.ok_or("当前应用没有内置前端恢复副本")?;
    let paths = AppPaths::discover().map_err(|_| "无法确定平台目录")?;
    offline(&paths, |root| source.repair(root))
}

/// First-run bootstrap for the native launcher, before starting its embedded Agent.
pub fn preinstall_offline() -> Result<PreinstallOutcome, String> {
    let source = BundledFrontend::discover()?.ok_or("当前应用没有内置前端恢复副本")?;
    let paths = AppPaths::discover().map_err(|_| "无法确定平台目录")?;
    offline(&paths, |root| source.preinstall(root))
}

fn offline<T>(
    paths: &AppPaths,
    action: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let managed = if paths.runtime_dir.starts_with(&paths.data_dir) {
        &paths.data_dir
    } else {
        &paths.runtime_dir
    };
    let _agent = AgentLifecycleLock::acquire(managed, &paths.runtime_dir)
        .map_err(|_| "后台仍在运行或生命周期锁不可用，请先停止后台")?;
    let db = configured_database(paths)?;
    let _database = db
        .as_deref()
        .map(DatabaseLock::acquire)
        .transpose()
        .map_err(|_| "数据库仍被使用或维护锁不可用，未执行修复")?;
    files::ensure_directory(&paths.data_dir)?;
    files::ensure_directory(&paths.frontend_dir)?;
    let _frontends = crate::frontend::FrontendDirectoryLease::acquire(&paths.frontend_dir)?;
    action(&paths.frontend_dir)
}

fn configured_database(paths: &AppPaths) -> Result<Option<PathBuf>, String> {
    // Malformed/missing config is not permission to guess a different database maintenance lock.
    files::check_directory(&paths.config_dir)?;
    files::check_file(&paths.config_file)?;
    let mut text = String::new();
    fs::File::open(&paths.config_file)
        .map_err(|_| "无法读取后台配置")?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "后台配置读取失败")?;
    if text.len() > 1024 * 1024 {
        return Err("后台配置过大".into());
    }
    let config = crate::loader::plugin_config::<StoreConfig>(&text, "store")
        .map_err(|_| "后台配置无效，不能安全确定数据库维护锁")?;
    config
        .map(|config| crate::paths::resolve_store_path(&config, paths))
        .transpose()
        .map(Option::flatten)
        .map_err(|_| "数据库路径配置无效".into())
}
