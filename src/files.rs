use crate::protocol::UPLOAD_CHUNK_BYTES;
use crate::{
    platform::{FileSystem, FileTransfer},
    protocol::ClientMessage,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct FileSession {
    home: Arc<dyn FileSystem>,
    upload: Option<Box<dyn FileTransfer>>,
    cancelled: Arc<AtomicBool>,
}
impl FileSession {
    #[cfg(all(test, target_os = "macos"))]
    pub fn new(home: Arc<dyn FileSystem>) -> Self {
        Self::with_cancellation(home, Arc::new(AtomicBool::new(false)))
    }
    pub fn with_cancellation(home: Arc<dyn FileSystem>, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            home,
            upload: None,
            cancelled,
        }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    fn abort(&mut self) -> Result<()> {
        if let Some(mut upload) = self.upload.take() {
            upload.cleanup()?;
        }
        Ok(())
    }
    fn response(&mut self, id: u32, result: Result<Value>, abort: bool) -> Value {
        match result {
            Ok(value) => value,
            Err(error) => {
                let cleanup = if abort { self.abort() } else { Ok(()) };
                let message = match cleanup {
                    Ok(()) => format!("{error:#}"),
                    Err(cleanup) => format!("{error:#}；{cleanup:#}"),
                };
                json!({"type":"file_error","id":id,"message":message,"upload_active":self.upload.is_some()})
            }
        }
    }
    pub fn message(&mut self, message: ClientMessage) -> Value {
        let (id, result, abort) = match message {
            ClientMessage::ListDirectory { id, path } => (id, self.home.list(id, &path), false),
            ClientMessage::UploadStart {
                id,
                path,
                name,
                size,
            } => {
                let result = (|| {
                    ensure!(self.upload.is_none(), "已有文件正在上传");
                    self.upload = Some(self.home.start_upload(id, &path, &name, size)?);
                    ensure!(!self.cancelled.load(Ordering::Acquire), "文件会话已取消");
                    Ok(json!({"type":"upload_ready","id":id,"chunk_size":UPLOAD_CHUNK_BYTES}))
                })();
                (id, result, self.cancelled.load(Ordering::Acquire))
            }
            ClientMessage::UploadFinish { id } => {
                let result = (|| {
                    let upload = self.upload.as_mut().context("没有正在上传的文件")?;
                    ensure!(upload.id() == id, "上传编号不匹配");
                    let value = upload.finish(&self.cancelled)?;
                    self.upload.take();
                    Ok(value)
                })();
                (id, result, true)
            }
            ClientMessage::UploadCancel { id } => {
                let result = (|| {
                    ensure!(
                        self.upload.as_ref().is_some_and(|u| u.id() == id),
                        "上传编号不匹配"
                    );
                    self.abort()?;
                    Ok(json!({"type":"upload_cancelled","id":id}))
                })();
                (id, result, false)
            }
            _ => return json!({"type":"file_error","id":0,"message":"不是文件消息"}),
        };
        self.response(id, result, abort)
    }
    pub fn chunk(&mut self, data: &[u8]) -> Value {
        let id = self.upload.as_ref().map_or(0, |u| u.id());
        let result = self
            .upload
            .as_mut()
            .context("请先开始文件上传")
            .and_then(|u| u.chunk(data));
        self.response(id, result, true)
    }
}
