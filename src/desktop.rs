use crate::protocol::ClientMessage;
use crate::{
    platform::{HostPlatform, InputController, SessionPower},
    protocol::MAX_TEXT_BYTES,
};
use anyhow::Result;
use anyhow::{Context, ensure};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use tokio::sync::oneshot;

/// Owned by the platform UI thread; platform objects never cross thread boundaries.
pub struct ControlSession<'a> {
    platform: &'a dyn HostPlatform,
    input: Option<Box<dyn InputController>>,
    awake: Option<Box<dyn SessionPower>>,
}
impl<'a> ControlSession<'a> {
    pub fn new(platform: &'a dyn HostPlatform) -> Self {
        Self {
            platform,
            input: None,
            awake: None,
        }
    }
    pub fn handle(&mut self, command: Command) {
        match command {
            Command::BeginInput {
                width,
                height,
                reply,
            } => {
                let result = (|| {
                    ensure!(self.input.is_none(), "键鼠控制尚未结束");
                    let awake = self.platform.keep_awake()?;
                    let input = self.platform.input(width, height)?;
                    self.awake = Some(awake);
                    self.input = Some(input);
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Command::Input { message, reply } => {
                let result = self
                    .input
                    .as_mut()
                    .context("键鼠控制尚未启动")
                    .and_then(|input| input.handle(message));
                let _ = reply.send(result);
            }
            Command::ReadClipboard { reply } => {
                let result = self.platform.read_clipboard().and_then(|text| {
                    ensure!(!text.is_empty(), "远程剪贴板文字为空");
                    ensure!(
                        text.len() <= MAX_TEXT_BYTES && !text.contains('\0'),
                        "剪贴板文字超过 64 KiB 或包含空字符"
                    );
                    Ok(text)
                });
                let _ = reply.send(result);
            }
            Command::PasteImage { png, reply } => {
                let result = (|| {
                    let input = self.input.as_mut().context("键鼠控制尚未启动")?;
                    self.platform.write_clipboard_image(&png)?;
                    input.paste()
                })();
                let _ = reply.send(result);
            }
            Command::RestoreSession { reply } => {
                let result = self.restore();
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
    }
    fn restore(&mut self) -> Result<()> {
        let result = self
            .input
            .take()
            .map(|mut input| input.release_all())
            .unwrap_or(Ok(()));
        self.awake.take();
        result
    }
}
impl Drop for ControlSession<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("释放远控资源失败：{error:#}");
        }
    }
}

#[derive(Default)]
pub struct Shared {
    pub connection: std::sync::OnceLock<crate::transport::Control>,
    pub active: AtomicBool,
    pub cancel: AtomicBool,
    pub shutdown: AtomicBool,
    pub maintenance: AtomicBool,
}

pub struct Maintenance(Arc<Shared>);
impl Shared {
    pub fn begin_maintenance(self: &Arc<Self>) -> Result<Maintenance> {
        self.maintenance
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| anyhow::anyhow!("连接设置正在保存"))?;
        let guard = Maintenance(self.clone());
        ensure!(
            !self.active.load(Ordering::SeqCst),
            "请先断开远控，再修改连接设置"
        );
        Ok(guard)
    }
}
impl Drop for Maintenance {
    fn drop(&mut self) {
        self.0.maintenance.store(false, Ordering::Release);
    }
}

pub enum Command {
    PasteImage {
        png: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    ReadClipboard {
        reply: oneshot::Sender<Result<String>>,
    },
    BeginInput {
        width: i32,
        height: i32,
        reply: oneshot::Sender<Result<()>>,
    },
    Input {
        message: ClientMessage,
        reply: oneshot::Sender<Result<()>>,
    },
    RestoreSession {
        reply: Option<oneshot::Sender<Result<()>>>,
    },
}

#[derive(Clone)]
pub struct DesktopControl {
    pub tx: mpsc::Sender<Command>,
    pub shared: Arc<Shared>,
}

impl DesktopControl {
    pub async fn paste_image(&self, png: Vec<u8>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::PasteImage { png, reply })?;
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await??
    }
    pub async fn read_clipboard(&self) -> Result<String> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::ReadClipboard { reply })?;
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await??
    }
    pub async fn begin_input(&self, width: i32, height: i32) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::BeginInput {
            width,
            height,
            reply,
        })?;
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await??
    }
    pub async fn input(&self, message: ClientMessage) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::Input { message, reply })?;
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await??
    }
    pub async fn end_session(&self) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx
            .send(Command::RestoreSession { reply: Some(reply) })?;
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await??
    }
    pub fn restore(&self) {
        if let Err(e) = self.tx.send(Command::RestoreSession { reply: None }) {
            eprintln!("结束远控命令失败: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Capabilities, CaptureSession, PROTOCOL_VERSION, ServerInfo};
    use std::{path::PathBuf, sync::Mutex};
    struct MockPlatform {
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_input: bool,
    }
    struct MockInput(Arc<Mutex<Vec<&'static str>>>);
    struct MockPower(Arc<Mutex<Vec<&'static str>>>);
    impl InputController for MockInput {
        fn handle(&mut self, _: ClientMessage) -> Result<()> {
            Ok(())
        }
        fn paste(&mut self) -> Result<()> {
            self.0.lock().unwrap().push("paste");
            Ok(())
        }
        fn release_all(&mut self) -> Result<()> {
            self.0.lock().unwrap().push("release");
            Ok(())
        }
    }
    impl SessionPower for MockPower {}
    impl Drop for MockPower {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("power_off");
        }
    }
    impl HostPlatform for MockPlatform {
        fn info(&self) -> ServerInfo {
            ServerInfo {
                protocol_version: PROTOCOL_VERSION,
                os: "test",
                capabilities: Capabilities {
                    capture: true,
                    input: true,
                    clipboard_text: true,
                    clipboard_image: true,
                    files: true,
                    capture_resize: true,
                    display_sleep: false,
                },
            }
        }
        fn check_permissions(&self) -> Result<()> {
            Ok(())
        }
        fn capture(&self) -> Result<Box<dyn CaptureSession>> {
            anyhow::bail!("unused")
        }
        fn home_directory(&self) -> Result<PathBuf> {
            Ok(std::env::temp_dir())
        }
        fn input(&self, _: i32, _: i32) -> Result<Box<dyn InputController>> {
            ensure!(!self.fail_input, "input denied");
            Ok(Box::new(MockInput(self.events.clone())))
        }
        fn keep_awake(&self) -> Result<Box<dyn SessionPower>> {
            self.events.lock().unwrap().push("power_on");
            Ok(Box::new(MockPower(self.events.clone())))
        }
        fn read_clipboard(&self) -> Result<String> {
            Ok("hello".into())
        }
        fn write_clipboard_image(&self, _: &[u8]) -> Result<()> {
            self.events.lock().unwrap().push("clipboard");
            Ok(())
        }
        fn run_ui(&self, _: DesktopControl, _: mpsc::Receiver<Command>) -> Result<()> {
            anyhow::bail!("unused")
        }
    }
    #[tokio::test]
    async fn native_session_releases_input_and_power_and_pastes_after_clipboard() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let platform = MockPlatform {
            events: events.clone(),
            fail_input: false,
        };
        let mut session = ControlSession::new(&platform);
        let (reply, rx) = oneshot::channel();
        session.handle(Command::BeginInput {
            width: 10,
            height: 10,
            reply,
        });
        rx.await.unwrap().unwrap();
        let (reply, rx) = oneshot::channel();
        session.handle(Command::PasteImage { png: vec![], reply });
        rx.await.unwrap().unwrap();
        drop(session);
        assert_eq!(
            *events.lock().unwrap(),
            ["power_on", "clipboard", "paste", "release", "power_off"]
        );
    }
    #[tokio::test]
    async fn input_initialization_failure_rolls_back_power() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let platform = MockPlatform {
            events: events.clone(),
            fail_input: true,
        };
        let mut session = ControlSession::new(&platform);
        let (reply, rx) = oneshot::channel();
        session.handle(Command::BeginInput {
            width: 10,
            height: 10,
            reply,
        });
        assert!(rx.await.unwrap().is_err());
        assert_eq!(*events.lock().unwrap(), ["power_on", "power_off"]);
    }
    #[test]
    fn connection_maintenance_cannot_overlap_a_session_and_releases_on_error() {
        let shared = Arc::new(Shared::default());
        shared.active.store(true, Ordering::Release);
        assert!(shared.begin_maintenance().is_err());
        assert!(!shared.maintenance.load(Ordering::Acquire));
        shared.active.store(false, Ordering::Release);
        let guard = shared.begin_maintenance().unwrap();
        assert!(shared.begin_maintenance().is_err());
        drop(guard);
        assert!(!shared.maintenance.load(Ordering::Acquire));
    }
}
