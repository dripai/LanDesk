use crate::platform::{CaptureBackend, DisplayTarget};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone)]
pub enum FrameEvent {
    Frame { jpeg: Arc<Vec<u8>>, sequence: u64 },
    Error(String),
}

#[derive(Clone, Copy)]
pub enum Change {
    Resolution(Option<u32>),
    Display(u32),
    Source(DisplayTarget),
}

type Configuration = (
    Change,
    tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
);

pub struct CaptureHandle {
    pub frames: watch::Receiver<Option<FrameEvent>>,
    pub info: watch::Receiver<serde_json::Value>,
    commands: std::sync::mpsc::Sender<Configuration>,
}
impl CaptureHandle {
    pub fn configure(
        &self,
        change: Change,
    ) -> futures_util::future::BoxFuture<'static, Result<serde_json::Value>> {
        let commands = self.commands.clone();
        Box::pin(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            commands
                .send((change, reply))
                .context("采集工作线程已关闭")?;
            response.await.context("采集配置响应中断")?
        })
    }
}

// Exactly one thread owns all SCStreams. A disconnected session only drops
// its command sender; synchronous ScreenCaptureKit cleanup never blocks Tokio.
#[derive(Clone)]
pub struct CaptureService(
    tokio::sync::mpsc::Sender<(
        Option<DisplayTarget>,
        tokio::sync::oneshot::Sender<Result<CaptureHandle>>,
    )>,
);
impl CaptureService {
    pub fn new(factory: impl FnOnce() -> Box<dyn CaptureBackend> + Send + 'static) -> Result<Self> {
        let (requests, mut receiver) = tokio::sync::mpsc::channel::<(
            Option<DisplayTarget>,
            tokio::sync::oneshot::Sender<Result<CaptureHandle>>,
        )>(1);
        std::thread::Builder::new()
            .name("landesk-capture".into())
            .spawn(move || {
                let mut backend = factory();
                while let Some((target, reply)) = receiver.blocking_recv() {
                    if reply.is_closed() {
                        continue;
                    }
                    let mut capture = match backend.open(target) {
                        Ok(capture) => capture,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                    let (commands, operations) = std::sync::mpsc::channel();
                    let (frames, images) = watch::channel(None);
                    let (metadata, info) = watch::channel(capture.info(None));
                    if reply
                        .send(Ok(CaptureHandle {
                            frames: images,
                            info,
                            commands,
                        }))
                        .is_err()
                    {
                        continue;
                    }
                    let mut requested = None;
                    let mut sequence = 0;
                    let mut geometry = capture.geometry();
                    let mut checked = std::time::Instant::now();
                    loop {
                        match operations.recv_timeout(std::time::Duration::from_millis(10)) {
                            Ok((change, reply)) => {
                                if reply.is_closed() {
                                    continue;
                                }
                                let result = (|| -> Result<_> {
                                    match change {
                                        Change::Resolution(width) => {
                                            capture.set_resolution(width)?;
                                            requested = width;
                                        }
                                        Change::Source(target) => {
                                            capture = backend.open(Some(target))?;
                                            requested = None;
                                        }
                                        Change::Display(id) => {
                                            let replacement =
                                                backend.open(Some(DisplayTarget::Existing {
                                                    id: Some(id),
                                                }))?;
                                            capture = replacement;
                                            requested = None;
                                        }
                                    }
                                    geometry = capture.geometry();
                                    let info = capture.info(requested);
                                    metadata.send_replace(info.clone());
                                    Ok(info)
                                })();
                                let _ = reply.send(result);
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if checked.elapsed() >= std::time::Duration::from_millis(250) {
                            checked = std::time::Instant::now();
                            let current = capture.geometry();
                            if current != geometry {
                                // Re-enumerate the same selected display after a mode change.
                                // Do not silently switch to an unrelated monitor.
                                let result = backend
                                    .open(Some(DisplayTarget::Existing {
                                        id: Some(capture.display_id()),
                                    }))
                                    .and_then(|mut next| {
                                        let width = requested
                                            .map(|width: u32| width.min(next.native_width()));
                                        next.set_resolution(width)?;
                                        requested = width;
                                        Ok(next)
                                    });
                                match result {
                                    Ok(next) => {
                                        capture = next;
                                        geometry = current;
                                        metadata.send_replace(capture.info(requested));
                                    }
                                    Err(error) => {
                                        frames.send_replace(Some(FrameEvent::Error(format!(
                                            "更新显示器失败: {error:#}"
                                        ))));
                                        break;
                                    }
                                }
                            }
                        }
                        if capture.frames().has_changed().unwrap_or(false) {
                            let event = capture.frames_mut().borrow_and_update().clone();
                            let failed = matches!(event, Some(FrameEvent::Error(_)));
                            let event = event.map(|event| match event {
                                FrameEvent::Frame { jpeg, .. } => {
                                    sequence += 1;
                                    FrameEvent::Frame { jpeg, sequence }
                                }
                                error => error,
                            });
                            frames.send_replace(event);
                            if failed {
                                break;
                            }
                        }
                    }
                    eprintln!("LanDesk capture cleanup started");
                    drop(capture);
                    eprintln!("LanDesk capture cleanup finished");
                }
            })
            .context("无法启动采集工作线程")?;
        Ok(Self(requests))
    }
    pub async fn start(&self, target: Option<DisplayTarget>) -> Result<CaptureHandle> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.0
            .send((target, reply))
            .await
            .context("采集服务已关闭")?;
        response.await.context("采集启动响应中断")?
    }
}
