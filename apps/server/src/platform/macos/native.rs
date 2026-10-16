use crate::platform::macos::input::Input;
use anyhow::Result;
use anyhow::ensure;
use landesk_core::domain::InputEvent;
use landesk_core::protocol::MAX_TEXT_BYTES;
use objc2::MainThreadMarker;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSApplication;
use objc2_app_kit::NSApplicationActivationPolicy;
use objc2_app_kit::NSPasteboard;
use objc2_app_kit::NSPasteboardItem;
use objc2_app_kit::NSPasteboardTypePNG;
use objc2_app_kit::NSPasteboardTypeString;
use objc2_foundation::NSArray;
use objc2_foundation::NSData;
use objc2_foundation::NSString;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Instant;
use tokio::sync::oneshot;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

use futures_util::future::BoxFuture;
use landesk_core::application::runtime::Shared;
use landesk_core::ports::DesktopControl;

pub fn request_capture() {
    unsafe {
        CGRequestScreenCaptureAccess();
    }
}
pub fn request_input() -> Result<()> {
    enigo::Enigo::new(&enigo::Settings::default())?;
    Ok(())
}

pub enum Command {
    PasteText {
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
    CopyText {
        reply: oneshot::Sender<Result<String>>,
    },
    PasteImage {
        png: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    ReadClipboard {
        reply: oneshot::Sender<Result<String>>,
    },
    BeginInput {
        reply: oneshot::Sender<Result<()>>,
    },
    Input {
        message: InputEvent,
        reply: oneshot::Sender<Result<()>>,
    },
    RestoreSession {
        reply: Option<oneshot::Sender<Result<()>>>,
    },
}

#[derive(Clone)]
pub struct Native {
    pub tx: mpsc::Sender<Command>,
    pub shared: Arc<Shared>,
}

impl Native {
    pub async fn paste_text(&self, text: String) -> Result<()> {
        ensure!(
            text.len() <= MAX_TEXT_BYTES && !text.contains('\0'),
            "粘贴文字超过 64 KiB 或包含空字符"
        );
        let (reply, response) = oneshot::channel();
        self.tx.send(Command::PasteText { text, reply })?;
        response.await?
    }
    pub async fn copy_text(&self) -> Result<String> {
        let (reply, response) = oneshot::channel();
        self.tx.send(Command::CopyText { reply })?;
        response.await?
    }

    pub async fn paste_image(&self, png: Vec<u8>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::PasteImage { png, reply })?;
        result.await?
    }
    pub async fn read_clipboard(&self) -> Result<String> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::ReadClipboard { reply })?;
        result.await?
    }
    pub async fn begin_input(&self, display_id: u32) -> Result<()> {
        self.shared.display.store(display_id, Ordering::Release);
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::BeginInput { reply })?;
        result.await?
    }
    pub async fn input(&self, message: InputEvent) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx.send(Command::Input { message, reply })?;
        result.await?
    }
    pub async fn end_session(&self) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.tx
            .send(Command::RestoreSession { reply: Some(reply) })?;
        result.await?
    }
    pub fn restore(&self) {
        if let Err(e) = self.tx.send(Command::RestoreSession { reply: None }) {
            eprintln!("结束远控命令失败: {e}");
        }
    }
}

fn clipboard_text() -> Result<String> {
    let text = NSPasteboard::generalPasteboard()
        .stringForType(unsafe { NSPasteboardTypeString })
        .ok_or_else(|| anyhow::anyhow!("Mac 剪贴板没有文字"))?
        .to_string();
    ensure!(!text.is_empty(), "Mac 剪贴板文字为空");
    ensure!(
        text.len() <= MAX_TEXT_BYTES && !text.contains('\0'),
        "剪贴板文字超过 64 KiB 或包含空字符"
    );
    Ok(text)
}

pub fn permissions() -> (bool, bool) {
    // Read-only TCC preflight, never bypasses consent.
    unsafe { (CGPreflightScreenCaptureAccess(), AXIsProcessTrusted() != 0) }
}

struct NativeCommands {
    input: Option<Input>,
    awake: Option<crate::platform::macos::power::KeepAwake>,
    copying: Option<(oneshot::Sender<Result<String>>, isize, Instant)>,
}
impl NativeCommands {
    fn tick(&mut self, native: &Native, rx: &mpsc::Receiver<Command>) {
        while let Ok(command) = rx.try_recv() {
            match command {
                Command::PasteText { text, reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = (|| {
                        let input = self
                            .input
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("键鼠控制尚未启动"))?;
                        let pasteboard = NSPasteboard::generalPasteboard();
                        pasteboard.clearContents();
                        ensure!(
                            pasteboard.setString_forType(&NSString::from_str(&text), unsafe {
                                NSPasteboardTypeString
                            }),
                            "无法写入 Mac 文字剪贴板"
                        );
                        input.paste()
                    })();
                    let _ = reply.send(result);
                }
                Command::CopyText { reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let count = NSPasteboard::generalPasteboard().changeCount();
                    let result = self
                        .input
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("键鼠控制尚未启动"))
                        .and_then(Input::copy);
                    match result {
                        Ok(()) => self.copying = Some((reply, count, Instant::now())),
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Command::PasteImage { png, reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = (|| {
                        let input = self
                            .input
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("键鼠控制尚未启动"))?;
                        let item = NSPasteboardItem::new();
                        ensure!(
                            item.setData_forType(&NSData::with_bytes(&png), unsafe {
                                NSPasteboardTypePNG
                            }),
                            "无法准备剪贴板图片"
                        );
                        let objects = NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]);
                        let pasteboard = NSPasteboard::generalPasteboard();
                        pasteboard.clearContents();
                        ensure!(pasteboard.writeObjects(&objects), "无法写入 Mac 图片剪贴板");
                        input.paste()?;
                        Ok(())
                    })();
                    let _ = reply.send(result);
                }
                Command::ReadClipboard { reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = clipboard_text();
                    let _ = reply.send(result);
                }
                Command::BeginInput { reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = (|| {
                        ensure!(self.input.is_none(), "键鼠控制尚未结束");
                        let guard = crate::platform::macos::power::KeepAwake::new()?;
                        self.input = Some(Input::new(native.shared.display.clone())?);
                        self.awake = Some(guard);
                        Ok(())
                    })();
                    let _ = reply.send(result);
                }
                Command::Input { message, reply } => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = self
                        .input
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("键鼠控制尚未启动"))
                        .and_then(|input| input.handle(message));
                    let _ = reply.send(result);
                }
                Command::RestoreSession { reply } => {
                    self.copying = None;
                    let release = self
                        .input
                        .take()
                        .map(|mut input| input.release_all())
                        .unwrap_or(Ok(()));
                    drop(self.awake.take());
                    if let Some(reply) = reply {
                        let _ = reply.send(release);
                    }
                }
            }
        }
        if let Some((reply, baseline, started)) = &self.copying {
            if reply.is_closed() {
                self.copying = None;
            } else if NSPasteboard::generalPasteboard().changeCount() != *baseline {
                let (reply, _, _) = self.copying.take().unwrap();
                let _ = reply.send(clipboard_text());
            } else if started.elapsed() >= std::time::Duration::from_secs(2) {
                let (reply, _, _) = self.copying.take().unwrap();
                let _ = reply.send(Err(anyhow::anyhow!(
                    "远程应用未更新剪贴板，请先选中文字后复制"
                )));
            }
        }
    }
}

pub fn initialize_app() -> MainThreadMarker {
    let mtm = MainThreadMarker::new().expect("native UI runs on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    mtm
}

pub fn command_timer(native: Native, rx: mpsc::Receiver<Command>) -> super::run_loop::RunLoopTimer {
    let commands = std::cell::RefCell::new(NativeCommands {
        input: None,
        awake: None,
        copying: None,
    });
    super::run_loop::RunLoopTimer::new(
        MainThreadMarker::new().expect("UI main thread"),
        move || {
            objc2::rc::autoreleasepool(|_| commands.borrow_mut().tick(&native, &rx));
        },
    )
}

impl DesktopControl for Native {
    fn supports_files(&self) -> bool {
        true
    }
    fn capabilities(&self) -> landesk_core::domain::Capabilities {
        landesk_core::domain::Capabilities {
            os: "macos",
            virtual_display: true,
        }
    }
    fn check_permissions(&self) -> Result<()> {
        let (capture, input) = permissions();
        ensure!(
            capture,
            "请在 Mac 系统设置中授予 LanDesk 屏幕录制权限，然后关闭并重新打开应用"
        );
        ensure!(
            input,
            "请在 Mac 系统设置中授予 LanDesk 辅助功能权限，然后关闭并重新打开应用"
        );
        Ok(())
    }
    fn begin_input(&self, display_id: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(Native::begin_input(self, display_id))
    }
    fn input(&self, message: InputEvent) -> BoxFuture<'_, Result<()>> {
        Box::pin(Native::input(self, message))
    }
    fn paste_text(&self, text: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(Native::paste_text(self, text))
    }
    fn paste_image(&self, png: Vec<u8>) -> BoxFuture<'_, Result<()>> {
        Box::pin(Native::paste_image(self, png))
    }
    fn copy_text(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(Native::copy_text(self))
    }
    fn read_clipboard(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(Native::read_clipboard(self))
    }
    fn end_session(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(Native::end_session(self))
    }
    fn restore(&self) {
        Native::restore(self);
    }
}
