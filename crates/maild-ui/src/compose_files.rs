//! Native-selected files: JS receives opaque tokens and progress, never paths or bytes.
use crate::{Bridge, UiError};
use base64::Engine;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
const MAX_FILE: u64 = 20 * 1024 * 1024;
const CHUNK: usize = 512 * 1024;
#[derive(Clone, Debug, Serialize)]
pub struct PickedFile {
    pub token: String,
    pub filename: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct FileProgress {
    pub token: String,
    pub offset: u64,
    pub size: u64,
}
struct Selected {
    file: Mutex<File>,
    name: String,
    size: u64,
    cancel: AtomicBool,
    busy: AtomicBool,
}
#[derive(Default)]
pub struct FileSelections {
    files: Mutex<HashMap<String, Arc<Selected>>>,
}
impl FileSelections {
    pub fn select(&self, path: &Path) -> Result<PickedFile, UiError> {
        let mut files = self.files.lock().unwrap();
        if files.len() >= 16 {
            return Err(UiError::invalid("too_many_files", "最多同时准备 16 个文件，请先完成或移除。"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        if std::fs::symlink_metadata(path)
            .map_err(|e| UiError::failed("select", e, "file_read_failed", "无法读取文件。"))?
            .file_type()
            .is_symlink()
        {
            return Err(UiError::invalid("file_symlink", "请选择原文件，不能使用符号链接。"));
        }
        let file = options
            .open(path)
            .map_err(|e| UiError::failed("select", e, "file_read_failed", "无法读取文件。"))?;
        let meta = file
            .metadata()
            .map_err(|e| UiError::failed("select", e, "file_read_failed", "无法读取文件。"))?;
        if !meta.is_file() || meta.len() > MAX_FILE {
            return Err(UiError::invalid("file_too_large", "请选择不超过 20 MiB 的普通文件。"));
        }
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| UiError::invalid("file_name_encoding", "文件名需要使用 UTF-8。"))?
            .to_owned();
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| UiError::failed("token", e, "attachment_prepare_failed", "无法准备附件。"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        files.insert(
            token.clone(),
            Arc::new(Selected {
                file: Mutex::new(file),
                name: filename.clone(),
                size: meta.len(),
                cancel: AtomicBool::new(false),
                busy: AtomicBool::new(false),
            }),
        );
        Ok(PickedFile {
            token,
            filename,
            size: meta.len(),
        })
    }
    pub fn release(&self, token: &str) {
        if let Some(f) = self.files.lock().unwrap().remove(token) {
            f.cancel.store(true, Ordering::SeqCst);
        }
    }
    pub async fn upload(
        &self,
        bridge: &Bridge,
        draft_id: &str,
        token: &str,
        progress: impl Fn(FileProgress),
    ) -> Result<String, UiError> {
        let file = self
            .files
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .ok_or_else(|| UiError::not_found("file_released", "所选文件已释放，请重新选择。"))?;
        if file.busy.swap(true, Ordering::SeqCst) {
            return Err(UiError::invalid("file_uploading", "这个文件正在上传。"));
        }
        struct Busy(Arc<Selected>);
        impl Drop for Busy {
            fn drop(&mut self) {
                self.0.busy.store(false, Ordering::SeqCst);
            }
        }
        let _busy = Busy(file.clone());
        let init:Result<maild_client::AttachmentUpload,UiError>=bridge.compose_call::<UploadReply>("beginAttachmentUpload",json!({"draft_id":draft_id,"filename":file.name,"mime_type":"application/octet-stream","size":file.size})).await.map(|r|r.upload);
        let upload = init?;
        let result=async{
            {let mut f=file.file.lock().unwrap();f.seek(SeekFrom::Start(0)).map_err(|e|UiError::failed("seek",e,"attachment_read_failed", "无法读取附件。"))?;}
            let mut offset=0;
            loop {
                if file.cancel.load(Ordering::SeqCst){return Err(UiError::invalid("upload_cancelled", "已取消附件上传。"));}
                let f=file.clone();
                let bytes=tokio::task::spawn_blocking(move||{
                    let mut bytes=vec![0;CHUNK];let n=f.file.lock().unwrap().read(&mut bytes)?;bytes.truncate(n);Ok::<_,std::io::Error>(bytes)
                }).await.map_err(|e|UiError::failed("read",e,"attachment_read_error", "读取附件失败。"))?.map_err(|e|UiError::failed("read",e,"attachment_read_error", "读取附件失败。"))?;
                if bytes.is_empty(){break;}
                let reply:UploadReply=bridge.compose_call("writeAttachmentUpload",json!({"upload_id":upload.upload_id,"offset":offset,"data":base64::engine::general_purpose::STANDARD.encode(&bytes)})).await?;
                offset+=bytes.len() as u64;
                if reply.upload.upload_id!=upload.upload_id || reply.upload.offset!=offset{return Err(UiError::invalid("upload_receipt_mismatch", "上传回执不一致，请重新选择文件。"));}
                progress(FileProgress{token:token.into(),offset,size:file.size});
            }
            if offset!=file.size || file.cancel.load(Ordering::SeqCst){return Err(UiError::invalid("file_changed", "文件已变化或上传已取消。"));}
            Ok(upload.upload_id.clone())
        }.await;
        if result.is_err() {
            let _ = bridge.cancel_upload(&upload.upload_id).await;
        }
        result
    }
}
#[derive(serde::Deserialize)]
struct UploadReply {
    upload: maild_client::AttachmentUpload,
}
