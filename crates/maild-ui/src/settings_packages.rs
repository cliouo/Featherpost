//! Native selected package snapshots. Only opaque, single-use approval tokens reach JS.
use crate::{Bridge, UiError};
use maild_client::{stage_plugin_package_into, PackagePreview, StagedPluginPackage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

struct Selected {
    staged: StagedPluginPackage,
    preview: PackagePreview,
    epoch: u64,
    created: std::time::Instant,
}
#[derive(Default)]
pub struct PackageSelections {
    entries: Mutex<HashMap<String, Selected>>,
    selecting: tokio::sync::Mutex<()>,
}
#[derive(Serialize)]
pub struct PackageChoice {
    pub token: String,
    pub source: String,
    pub preview: PackagePreview,
}
#[derive(Deserialize)]
struct Root {
    root: PathBuf,
}
impl PackageSelections {
    pub async fn select(
        &self,
        bridge: &Bridge,
        path: &Path,
        kind: &str,
    ) -> Result<PackageChoice, UiError> {
        crate::settings::check_kind(kind)?;
        bridge.guard("commitPluginInstall")?;
        let _selecting = self
            .selecting
            .try_lock()
            .map_err(|_| UiError::invalid("已有安装包正在准备。"))?;
        let epoch = bridge.snapshot().connection_epoch;
        {
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|_, e| {
                e.epoch == epoch && e.created.elapsed() < std::time::Duration::from_secs(600)
            });
            if entries.len() >= 4 {
                return Err(UiError::invalid("请先完成或取消已有安装。"));
            }
        }
        let root: Root = bridge
            .management_call("getPluginInstallRoot", json!({"kind":kind}))
            .await?;
        let source = path.to_path_buf();
        let staged =
            tokio::task::spawn_blocking(move || stage_plugin_package_into(&source, &root.root))
                .await
                .map_err(|_| UiError::invalid("准备安装包失败。"))?
                .map_err(|_| {
                    UiError::invalid(
                        "无法准备此目录。请选择当前用户所有、无符号链接且不超过 512 MiB 的完整包。",
                    )
                })?;
        let preview: PackagePreview = bridge
            .management_call(
                "previewPluginPackage",
                json!({"kind":kind,"staging":staged.staging()}),
            )
            .await?;
        if bridge.snapshot().connection_epoch != epoch || bridge.snapshot().phase != "online" {
            return Err(UiError::invalid("连接已变化，请重新选择安装包。"));
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| UiError::invalid("无法生成安装令牌。"))?;
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= 4 {
            return Err(UiError::invalid("请先完成或取消已有安装。"));
        }
        entries.insert(
            token.clone(),
            Selected {
                staged,
                preview: preview.clone(),
                epoch,
                created: std::time::Instant::now(),
            },
        );
        Ok(PackageChoice {
            token,
            source: path.display().to_string(),
            preview,
        })
    }
    pub fn cancel(&self, token: &str) {
        self.entries.lock().unwrap().remove(token);
    }
    pub async fn install(
        &self,
        bridge: &Bridge,
        token: &str,
    ) -> Result<maild_client::CommitPluginInstallResult, UiError> {
        let selected = self
            .entries
            .lock()
            .unwrap()
            .remove(token)
            .ok_or_else(|| UiError::invalid("安装确认已过期，请重新选择。"))?;
        if selected.epoch != bridge.snapshot().connection_epoch
            || selected.created.elapsed() >= std::time::Duration::from_secs(600)
        {
            return Err(UiError::invalid("连接或安装确认已过期，请重新选择安装包。"));
        }
        let r: Value=bridge.management_call("commitPluginInstall",json!({"kind":selected.preview.kind,"staging":selected.staged.staging(),"expected_preview":selected.preview})).await?;
        serde_json::from_value(r)
            .map_err(|_| UiError::invalid("后台未返回完整安装回执，请刷新清单。"))
    }
}
