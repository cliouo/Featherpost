//! Bounded, principal-owned attachment staging (ADR-0035). No caller-supplied paths.
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};

pub const CHUNK_BYTES: usize = 512 * 1024;
pub const FILE_BYTES: u64 = 20 * 1024 * 1024;
pub const MESSAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const ACTIVE_LIMIT: usize = 8;
pub const RESERVED_BYTES: u64 = 128 * 1024 * 1024;
pub const IDLE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug)]
pub enum UploadError {
    Missing,
    Invalid(&'static str),
    Limit(&'static str, u64),
    State(u64, u64),
    Io(std::io::Error),
    Sql(rusqlite::Error),
    Draft(crate::store::DraftWrite),
}
impl From<std::io::Error> for UploadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rusqlite::Error> for UploadError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadStatus {
    pub upload_id: String,
    pub offset: u64,
    pub size: u64,
    pub chunk_bytes: usize,
}
pub(crate) struct Upload {
    pub owner: String,
    pub draft_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    offset: u64,
    touched: Instant,
    file: File,
}
impl Upload {
    fn status(&self, id: &str) -> UploadStatus {
        UploadStatus {
            upload_id: id.into(),
            offset: self.offset,
            size: self.size,
            chunk_bytes: CHUNK_BYTES,
        }
    }
    pub fn bytes(&mut self) -> Result<Vec<u8>, UploadError> {
        if self.offset != self.size {
            return Err(UploadError::State(self.offset, self.size));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(self.size as usize);
        Read::by_ref(&mut self.file)
            .take(self.size + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != self.size {
            return Err(UploadError::State(bytes.len() as u64, self.size));
        }
        Ok(bytes)
    }
}

/// File handles are owned here, independently of the database lock. Housekeeping may hold only this
/// mutex, never Arc<Store>, so it cannot pin a stopped daemon's database.
#[derive(Default)]
pub(crate) struct Uploads {
    entries: HashMap<String, Upload>,
    memory_dir: Option<tempfile::TempDir>,
}
impl Uploads {
    pub fn sweep(&mut self, now: Instant) {
        self.entries
            .retain(|_, u| now.duration_since(u.touched) < IDLE_TTL);
    }
    pub fn discard_draft(&mut self, draft_id: &str) {
        self.entries.retain(|_, u| u.draft_id != draft_id);
    }
    pub fn remove(&mut self, id: &str) {
        self.entries.remove(id);
    }
    pub fn get(&mut self, id: &str, owner: &str) -> Result<&mut Upload, UploadError> {
        self.sweep(Instant::now());
        self.entries
            .get_mut(id)
            .filter(|u| u.owner == owner)
            .ok_or(UploadError::Missing)
    }
    pub fn cancel(&mut self, id: &str, owner: &str) -> Result<(), UploadError> {
        self.get(id, owner)?;
        self.entries.remove(id);
        Ok(())
    }
    pub fn begin(
        &mut self,
        db: &str,
        owner: &str,
        draft_id: &str,
        filename: &str,
        mime_type: &str,
        size: u64,
    ) -> Result<UploadStatus, UploadError> {
        validate_metadata(filename, mime_type, size)?;
        self.sweep(Instant::now());
        if self.entries.len() >= ACTIVE_LIMIT {
            return Err(UploadError::Limit("active_uploads", ACTIVE_LIMIT as u64));
        }
        if self.entries.values().map(|u| u.size).sum::<u64>() + size > RESERVED_BYTES {
            return Err(UploadError::Limit("reserved_upload_bytes", RESERVED_BYTES));
        }
        let root = if db.is_empty() {
            if self.memory_dir.is_none() {
                self.memory_dir = Some(tempfile::tempdir()?);
            }
            self.memory_dir
                .as_ref()
                .unwrap()
                .path()
                .join("attachments-staging")
        } else {
            let path = Path::new(db);
            path.with_file_name(format!(
                ".{}.attachments-staging",
                path.file_name().unwrap().to_string_lossy()
            ))
        };
        let file = staging_file(&root)?;
        let id = crate::drafts::generate_draft_id().replacen("draft_", "upload_", 1);
        let upload = Upload {
            owner: owner.into(),
            draft_id: draft_id.into(),
            filename: filename.into(),
            mime_type: mime_type.into(),
            size,
            offset: 0,
            touched: Instant::now(),
            file,
        };
        let status = upload.status(&id);
        self.entries.insert(id, upload);
        Ok(status)
    }
    pub fn write(
        &mut self,
        id: &str,
        owner: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<UploadStatus, UploadError> {
        if bytes.is_empty() || bytes.len() > CHUNK_BYTES {
            return Err(UploadError::Invalid("chunk must contain 1..512 KiB"));
        }
        let u = self.get(id, owner)?;
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(UploadError::State(u.offset, u.size))?;
        if end > u.size || offset > u.offset || (offset < u.offset && end > u.offset) {
            return Err(UploadError::State(u.offset, u.size));
        }
        u.file.seek(SeekFrom::Start(offset))?;
        if offset < u.offset {
            let mut previous = vec![0; bytes.len()];
            u.file.read_exact(&mut previous)?;
            if previous != bytes {
                return Err(UploadError::State(u.offset, u.size));
            }
        } else {
            // A partial I/O failure never advances the acknowledged offset. Retrying overwrites it.
            u.file.write_all(bytes)?;
            u.offset = end;
        }
        u.touched = Instant::now();
        Ok(u.status(id))
    }
}

pub fn validate_metadata(filename: &str, mime: &str, size: u64) -> Result<(), UploadError> {
    if filename.is_empty()
        || filename.len() > 255
        || filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(UploadError::Invalid(
            "filename must be a plain display name",
        ));
    }
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    if mime.len() > 127
        || !mime
            .split_once('/')
            .is_some_and(|(a, b)| token(a) && token(b))
    {
        return Err(UploadError::Invalid("mime_type must be type/subtype"));
    }
    if size > FILE_BYTES {
        return Err(UploadError::Limit("attachment_bytes", FILE_BYTES));
    }
    Ok(())
}

fn staging_file(root: &Path) -> std::io::Result<File> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(std::io::Error::other("unsafe staging directory"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("reparse staging directory"));
        }
    }
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        let actual = dir.metadata()?;
        if actual.uid() != unsafe { libc::geteuid() } || actual.mode() & 0o077 != 0 {
            return Err(std::io::Error::other("unsafe staging owner or permissions"));
        }
        // macOS does not resolve children under /dev/fd/N. Use openat/unlinkat with the
        // held directory descriptor instead; no second path lookup can redirect this write.
        use std::{ffi::CString, os::fd::FromRawFd};
        let name = CString::new(crate::drafts::generate_draft_id()).expect("ASCII random ID");
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        tempfile::tempfile_in(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expiry_cancel_and_quotas_close_anonymous_files() {
        let mut uploads = Uploads::default();
        let u = uploads
            .begin("", "user", "draft", "x", "text/plain", 2)
            .unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_dir(
                uploads
                    .memory_dir
                    .as_ref()
                    .unwrap()
                    .path()
                    .join("attachments-staging")
            )
            .unwrap()
            .count(),
            0
        );
        assert!(matches!(
            uploads.cancel(&u.upload_id, "other"),
            Err(UploadError::Missing)
        ));
        uploads.sweep(Instant::now() + IDLE_TTL);
        assert!(matches!(
            uploads.get(&u.upload_id, "user"),
            Err(UploadError::Missing)
        ));
        for _ in 0..ACTIVE_LIMIT {
            uploads
                .begin("", "user", "draft", "x", "text/plain", 1)
                .unwrap();
        }
        assert!(matches!(
            uploads.begin("", "user", "draft", "x", "text/plain", 1),
            Err(UploadError::Limit(..))
        ));
        uploads.discard_draft("draft");
        assert!(uploads.entries.is_empty());
        for _ in 0..6 {
            uploads
                .begin("", "user", "draft", "x", "text/plain", FILE_BYTES)
                .unwrap();
        }
        assert!(matches!(
            uploads.begin("", "user", "draft", "x", "text/plain", FILE_BYTES),
            Err(UploadError::Limit(..))
        ));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_world_readable_staging_are_refused() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("staging");
        symlink(temp.path(), &root).unwrap();
        assert!(staging_file(&root).is_err());
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(staging_file(&root).is_err());
    }
}
