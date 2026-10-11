use crate::{
    files::{FileSession, HomeFiles},
    protocol::ClientMessage,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct FileService {
    home: Arc<HomeFiles>,
    slots: Arc<Semaphore>,
}

impl FileService {
    pub fn new(home: HomeFiles) -> Self {
        Self {
            home: Arc::new(home),
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn connect(&self) -> FileChannel {
        FileChannel {
            service: self.clone(),
            worker: None,
            pending: None,
            upload_id: None,
        }
    }

    #[cfg(test)]
    pub fn blocked_channel(&self) -> (FileChannel, oneshot::Receiver<()>, mpsc::Sender<()>) {
        let (started, ready) = oneshot::channel();
        let (release, blocked) = mpsc::channel();
        let mut channel = self.connect();
        channel.worker = Some(
            Worker::start(self.slots.clone(), move |_| {
                let mut started = Some(started);
                Box::new(move |_| {
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                    blocked.recv_timeout(Duration::from_secs(30)).unwrap();
                    json!({"type":"directory", "id":1})
                })
            })
            .unwrap(),
        );
        (channel, ready, release)
    }
}

pub enum FileRequest {
    Message(ClientMessage),
    Chunk(Vec<u8>),
}

struct Job {
    request: FileRequest,
    reply: oneshot::Sender<Value>,
}

struct Worker {
    sender: mpsc::SyncSender<Job>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Never join a thread that may be waiting for a macOS permission dialog.
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Worker {
    fn start(
        slots: Arc<Semaphore>,
        build: impl FnOnce(Arc<AtomicBool>) -> Box<dyn FnMut(FileRequest) -> Value + Send>
        + Send
        + 'static,
    ) -> Result<Self> {
        // Keep this permit until the OS call AND upload cleanup have returned,
        // even after disconnect. Reconnecting cannot accumulate blocked threads.
        let permit = slots
            .try_acquire_owned()
            .context("文件访问仍在等待系统响应，请处理 Mac 上的授权弹窗后重试；远控可继续使用")?;
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        std::thread::Builder::new()
            .name("landesk-files".into())
            .spawn(move || {
                let _permit = permit;
                let mut handle = build(flag.clone());
                while let Ok(job) = receiver.recv() {
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                    let response = handle(job.request);
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                    let _ = job.reply.send(response);
                }
                // FileSession and temporary uploads are dropped on this worker too.
            })
            .context("无法启动文件工作线程")?;
        Ok(Self { sender, cancelled })
    }
}

struct Pending {
    id: u32,
    reply: oneshot::Receiver<Value>,
    deadline: tokio::time::Instant,
}

pub struct FileChannel {
    service: FileService,
    worker: Option<Worker>,
    pending: Option<Pending>,
    upload_id: Option<u32>,
}

impl FileChannel {
    fn error(&self, id: u32, message: String) -> Value {
        json!({"type":"file_error", "id":id, "message":message, "upload_active":self.upload_id.is_some()})
    }

    pub fn submit(&mut self, request: FileRequest) -> Option<Value> {
        let id = match &request {
            FileRequest::Message(
                ClientMessage::ListDirectory { id, .. }
                | ClientMessage::UploadStart { id, .. }
                | ClientMessage::UploadFinish { id }
                | ClientMessage::UploadCancel { id },
            ) => *id,
            _ => self.upload_id.unwrap_or(0),
        };
        if self.pending.is_some() {
            return Some(self.error(
                id,
                "文件操作正在等待响应，请稍后重试；远控可继续使用".into(),
            ));
        }
        if self.worker.is_none() {
            let home = self.service.home.clone();
            match Worker::start(self.service.slots.clone(), move |cancelled| {
                let mut files = FileSession::with_cancellation(home, cancelled);
                Box::new(move |request| match request {
                    FileRequest::Message(message) => files.message(message),
                    FileRequest::Chunk(data) => files.chunk(&data),
                })
            }) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => return Some(self.error(id, format!("{error:#}"))),
            }
        }
        let (reply, response) = oneshot::channel();
        if self
            .worker
            .as_ref()
            .unwrap()
            .sender
            .try_send(Job { request, reply })
            .is_err()
        {
            self.worker = None;
            self.upload_id = None;
            return Some(self.error(id, "文件工作线程已停止，请重试".into()));
        }
        self.pending = Some(Pending {
            id,
            reply: response,
            deadline: tokio::time::Instant::now() + REQUEST_TIMEOUT,
        });
        None
    }

    // Cancellation-safe: select! may drop this future for every frame/heartbeat.
    pub async fn response(&mut self) -> Value {
        let Some(pending) = self.pending.as_mut() else {
            return std::future::pending().await;
        };
        let result = tokio::time::timeout_at(pending.deadline, &mut pending.reply).await;
        let id = self.pending.take().unwrap().id;
        match result {
            Ok(Ok(value)) => {
                match value["type"].as_str() {
                    Some("upload_ready") => self.upload_id = Some(id),
                    Some("upload_done" | "upload_cancelled") => self.upload_id = None,
                    Some("file_error") if value["upload_active"] == false => self.upload_id = None,
                    _ => {}
                }
                value
            }
            error => {
                self.worker = None;
                self.upload_id = None;
                let message = if error.is_err() {
                    "文件操作超时，可能正在等待 Mac 文件访问授权；请处理系统弹窗后刷新目录。远控可继续使用，未完成上传将取消；若刚提交完成上传，请刷新确认结果"
                } else {
                    "文件工作线程已停止，请重试"
                };
                self.error(id, message.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> FileService {
        FileService::new(HomeFiles::open(std::path::Path::new("/private/tmp")).unwrap())
    }
    fn list(id: u32) -> FileRequest {
        FileRequest::Message(ClientMessage::ListDirectory {
            cursor: String::new(),
            id,
            path: String::new(),
        })
    }

    #[tokio::test]
    async fn timeout_does_not_wait_for_os_and_reconnect_cannot_spawn_more_workers() {
        let service = service();
        let (mut files, started, release) = service.blocked_channel();
        assert!(files.submit(list(1)).is_none());
        started.await.unwrap();
        assert_eq!(files.submit(list(2)).unwrap()["type"], "file_error");
        files.pending.as_mut().unwrap().deadline = tokio::time::Instant::now();
        let error = tokio::time::timeout(Duration::from_secs(1), files.response())
            .await
            .unwrap();
        assert_eq!(error["id"], 1);
        assert!(error["message"].as_str().unwrap().contains("超时"));
        assert_eq!(error["upload_active"], false);
        drop(files);
        for _ in 0..20 {
            let error = service.connect().submit(list(3)).unwrap();
            assert!(error["message"].as_str().unwrap().contains("等待系统响应"));
        }
        release.send(()).unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(1), service.slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        let mut files = service.connect();
        assert!(files.submit(list(4)).is_none());
        let response = files.response().await;
        assert_eq!(response["type"], "directory");
        assert_eq!(response["id"], 4); // No late result from the cancelled request.
    }

    #[tokio::test]
    async fn abandoning_response_poll_keeps_request_and_deadline() {
        let service = service();
        let (mut files, started, release) = service.blocked_channel();
        files.submit(list(1));
        started.await.unwrap();
        let deadline = files.pending.as_ref().unwrap().deadline;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), files.response())
                .await
                .is_err()
        );
        assert_eq!(files.pending.as_ref().unwrap().deadline, deadline);
        release.send(()).unwrap();
        assert_eq!(files.response().await["id"], 1);
    }

    #[tokio::test]
    async fn file_error_does_not_poison_following_requests() {
        let mut files = service().connect();
        files.submit(FileRequest::Message(ClientMessage::ListDirectory {
            cursor: String::new(),
            id: 1,
            path: "..".into(),
        }));
        assert_eq!(files.response().await["type"], "file_error");
        files.submit(list(2));
        assert_eq!(files.response().await["type"], "directory");
    }

    #[tokio::test]
    async fn disconnect_cleans_partial_upload_before_next_file_session() {
        let root = std::path::PathBuf::from(format!(
            "/private/tmp/landesk-worker-test-{:016x}",
            getrandom::u64().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let service = FileService::new(HomeFiles::open(&root).unwrap());
        let mut files = service.connect();
        files.submit(FileRequest::Message(ClientMessage::UploadStart {
            id: 1,
            path: String::new(),
            name: "partial.txt".into(),
            size: 10,
        }));
        assert_eq!(files.response().await["type"], "upload_ready");
        files.submit(FileRequest::Chunk(vec![1, 2]));
        assert_eq!(files.response().await["written"], 2);
        drop(files);
        let permit = tokio::time::timeout(Duration::from_secs(1), service.slots.acquire())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        drop(permit);
        std::fs::remove_dir(&root).unwrap();
    }
}
