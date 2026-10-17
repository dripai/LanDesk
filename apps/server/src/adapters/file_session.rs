//! Shared upload protocol and cleanup; native adapters only implement filesystem operations.
use anyhow::{Context, Result, ensure};
use landesk_core::domain::{FileCommand, FileResponse};
use landesk_core::protocol::UPLOAD_CHUNK_BYTES;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const TEMP_PREFIX: &str = ".landesk-upload-";
pub const PAGE_SIZE: usize = 500;

pub trait Store: Send + Sync {
    fn list(&self, id: u32, path: &str, cursor: &str) -> Result<FileResponse>;
    fn start(&self, id: u32, path: &str, name: &str, size: u64) -> Result<Box<dyn Upload>>;
}
pub trait Upload: Send {
    fn id(&self) -> u32;
    fn chunk(&mut self, data: &[u8]) -> Result<FileResponse>;
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<FileResponse>;
    fn cleanup(&mut self) -> Result<()>;
}

pub struct Session {
    home: Arc<dyn Store>,
    upload: Option<Box<dyn Upload>>,
    pub(crate) cancelled: Arc<AtomicBool>,
}
impl Session {
    #[cfg(test)]
    pub fn new(home: Arc<dyn Store>) -> Self {
        Self::with_cancellation(home, Arc::new(AtomicBool::new(false)))
    }
    pub fn with_cancellation(home: Arc<dyn Store>, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            home,
            upload: None,
            cancelled,
        }
    }
    fn abort(&mut self) -> Result<()> {
        if let Some(mut upload) = self.upload.take() {
            upload.cleanup()?;
        }
        Ok(())
    }
    fn response(&mut self, id: u32, result: Result<FileResponse>, abort: bool) -> FileResponse {
        match result {
            Ok(value) => value,
            Err(error) => {
                let cleanup = if abort { self.abort() } else { Ok(()) };
                let message = match cleanup {
                    Ok(()) => format!("{error:#}"),
                    Err(cleanup) => format!("{error:#}；{cleanup:#}"),
                };
                FileResponse::FileError {
                    id,
                    message,
                    upload_active: self.upload.is_some(),
                }
            }
        }
    }
    pub fn message(&mut self, message: FileCommand) -> FileResponse {
        let (id, result, abort) = match message {
            FileCommand::ListDirectory { id, path, cursor } => {
                (id, self.home.list(id, &path, &cursor), false)
            }
            FileCommand::UploadStart {
                id,
                path,
                name,
                size,
            } => {
                let result = (|| {
                    ensure!(self.upload.is_none(), "已有文件正在上传");
                    self.upload = Some(self.home.start(id, &path, &name, size)?);
                    ensure!(!self.cancelled.load(Ordering::Acquire), "文件会话已取消");
                    Ok(FileResponse::UploadReady {
                        id,
                        chunk_size: UPLOAD_CHUNK_BYTES,
                    })
                })();
                (id, result, self.cancelled.load(Ordering::Acquire))
            }
            FileCommand::UploadFinish { id } => {
                let result = (|| {
                    let upload = self.upload.as_mut().context("没有正在上传的文件")?;
                    ensure!(upload.id() == id, "上传编号不匹配");
                    let value = upload.finish(&self.cancelled)?;
                    self.upload.take();
                    Ok(value)
                })();
                (id, result, true)
            }
            FileCommand::UploadCancel { id } => {
                let result = (|| {
                    ensure!(
                        self.upload.as_ref().is_some_and(|u| u.id() == id),
                        "上传编号不匹配"
                    );
                    self.abort()?;
                    Ok(FileResponse::UploadCancelled { id })
                })();
                (id, result, false)
            }
        };
        self.response(id, result, abort)
    }
    pub fn chunk(&mut self, data: &[u8]) -> FileResponse {
        let id = self.upload.as_ref().map_or(0, |u| u.id());
        let result = if self.cancelled.load(Ordering::Acquire) {
            Err(anyhow::anyhow!("文件会话已取消"))
        } else {
            self.upload
                .as_mut()
                .context("请先开始文件上传")
                .and_then(|u| u.chunk(data))
        };
        self.response(id, result, true)
    }
}

impl landesk_core::ports::FileSession for Session {
    fn message(&mut self, message: FileCommand) -> FileResponse {
        Session::message(self, message)
    }
    fn chunk(&mut self, bytes: &[u8]) -> FileResponse {
        Session::chunk(self, bytes)
    }
}
