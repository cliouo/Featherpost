//! Persistent default selection. Corrupt state is preserved and never overwritten.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileState {
    version: u32,
    default: String,
}

pub(super) struct Selection {
    pub id: String,
    path: Option<PathBuf>,
    writable: bool,
    pub has_state: bool,
}

impl Selection {
    pub fn load(seed: &str, root: Option<&Path>) -> Self {
        let path = root.map(|root| root.join(".default.json"));
        let mut result = Self {
            id: super::migrate_default(seed).into(),
            path,
            writable: true,
            has_state: false,
        };
        if let Some(path) = &result.path {
            match read(path) {
                Ok(Some(id)) => {
                    result.id = id;
                    result.has_state = true;
                }
                Ok(None) => {}
                Err(_) => {
                    result.writable = false;
                    result.has_state = true;
                }
            }
        }
        result
    }

    pub fn store(&mut self, id: &str) -> Result<(), String> {
        if !self.writable {
            return Err("默认前端状态损坏，未覆盖原文件".into());
        }
        let path = self.path.as_ref().ok_or("未装配持久默认前端目录")?;
        // Re-check the current file before any replacement; never follow a substituted link.
        read(path)?;
        let parent = path.parent().ok_or("状态文件缺少父目录")?;
        let metadata = std::fs::symlink_metadata(parent).map_err(|_| "前端目录不可读取")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("前端目录无效".into());
        }
        super::validate_owned_entry(&metadata)?;
        let bytes = serde_json::to_vec(&FileState {
            version: 1,
            default: id.into(),
        })
        .map_err(|_| "状态序列化失败")?;
        let temp = parent.join(format!(
            ".default-{}",
            crate::plugin_identity::generate_token()
        ));
        let result = crate::plugin_state::publish_atomically(&temp, path, &bytes);
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temp);
            self.reconcile_uncertain_publication();
            return Err(error);
        }
        self.id = id.into();
        self.has_state = true;
        Ok(())
    }
    fn reconcile_uncertain_publication(&mut self) {
        // rename may already have succeeded before parent fsync failed. Reflect what is on disk,
        // but freeze subsequent default/removal mutations until restart resolves durability.
        if let Some(path) = &self.path {
            if let Ok(Some(id)) = read(path) {
                self.id = id;
            }
        }
        self.writable = false;
    }
    pub fn can_remove(&self, id: &str) -> bool {
        self.writable && self.id != id
    }

    pub(super) fn freeze(&mut self) {
        self.writable = false;
    }

    pub(super) fn is_healthy(&self) -> bool { self.writable }
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("默认前端状态不可读取".into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err("默认前端状态无效".into());
    }
    crate::plugin_state::check_owned_file(&metadata)?;
    let state: FileState =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "状态读取失败")?)
            .map_err(|_| "状态解析失败")?;
    if state.version != 1 {
        return Err("未知默认前端状态版本".into());
    }
    super::validate_frontend_id(&state.default)?;
    Ok(Some(super::migrate_default(&state.default).into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_roundtrip_is_private_and_corruption_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".default.json");
        let mut s = Selection::load("seed.ui", Some(dir.path()));
        s.store(maild_brand::BUNDLED_FRONTEND_ID).unwrap();
        assert_eq!(
            Selection::load("seed.ui", Some(dir.path())).id,
            maild_brand::BUNDLED_FRONTEND_ID
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let damaged = b"{\"version\":99,\"default\":\"future.ui\"}";
        std::fs::write(&path, damaged).unwrap();
        let mut s = Selection::load("seed.ui", Some(dir.path()));
        assert_eq!(s.id, "seed.ui");
        assert!(s.store(maild_brand::BUNDLED_FRONTEND_ID).is_err());
        assert_eq!(std::fs::read(path).unwrap(), damaged);
    }
    #[cfg(unix)]
    #[test]
    fn substituted_selection_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Selection::load("seed.ui", Some(dir.path()));
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join(".default.json")).unwrap();
        assert!(s.store(maild_brand::BUNDLED_FRONTEND_ID).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
    }
    #[test]
    fn persisted_default_is_resolved_before_a_removed_toml_seed() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("persisted.ui");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("frontend.toml"),
            "schema_version = 1\nprotocol_version = 1\nid = \"persisted.ui\"\nname = \"P\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"inbox\"]\n",
        )
        .unwrap();
        let mut s = Selection::load("former.frontend", Some(dir.path()));
        s.store("persisted.ui").unwrap();
        let config = super::super::FrontendConfig {
            default: "former.frontend".into(),
        };
        let broker =
            super::super::FrontendBroker::with_install_root(&config, Some(dir.path())).unwrap();
        assert_eq!(broker.default_frontend(), "persisted.ui");
    }
    #[test]
    fn management_metadata_does_not_consume_package_slots() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..64 {
            std::fs::create_dir(dir.path().join(format!("package{n}"))).unwrap();
        }
        for name in [".incoming", ".rollback", ".removed"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        let mut s = Selection::load("seed.ui", Some(dir.path()));
        s.store("seed.ui").unwrap();
        assert!(super::super::FrontendBroker::with_install_root(
            &super::super::FrontendConfig::default(),
            Some(dir.path())
        )
        .is_ok());
    }
    #[test]
    fn uncertain_fsync_reconciles_visible_selection_and_freezes_removal() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Selection::load("seed.ui", Some(dir.path()));
        s.store(maild_brand::BUNDLED_FRONTEND_ID).unwrap();
        // Simulate rename to another default followed by a directory-fsync error.
        std::fs::write(
            dir.path().join(".default.json"),
            br#"{"version":1,"default":"other.ui"}"#,
        )
        .unwrap();
        s.reconcile_uncertain_publication();
        assert_eq!(s.id, "other.ui");
        assert!(!s.can_remove("other.ui"));
        assert!(!s.can_remove(maild_brand::BUNDLED_FRONTEND_ID));
        assert!(s.store("seed.ui").is_err());
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;
    #[test]
    fn legacy_seed_and_state_read_as_tauri_default_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Selection::load("builtin-egui", None).id, maild_brand::BUNDLED_FRONTEND_ID);
        let path = dir.path().join(".default.json");
        let legacy = br#"{"version":1,"default":"builtin-egui"}"#;
        std::fs::write(&path, legacy).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let s = Selection::load("other.ui", Some(dir.path()));
        assert_eq!(s.id, maild_brand::BUNDLED_FRONTEND_ID);
        assert!(s.is_healthy());
        assert_eq!(std::fs::read(path).unwrap(), legacy);
    }
}
