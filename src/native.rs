use crate::{
    input::Input,
    protocol::{ClientMessage, MAX_TEXT_BYTES},
};
use anyhow::{Result, ensure};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSEventMask,
    NSFont, NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString,
};
use std::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    sync::{Arc, mpsc},
    time::Instant,
};
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

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct PermissionActions;
    unsafe impl NSObjectProtocol for PermissionActions {}
    impl PermissionActions {
        #[unsafe(method(requestCapture:))]
        fn request_capture(&self, _sender: &AnyObject) {
            unsafe { CGRequestScreenCaptureAccess(); }
        }
        #[unsafe(method(requestInput:))]
        fn request_input(&self, _sender: &AnyObject) {
            // Enigo's native permission prompt; this generates no input events.
            let _ = enigo::Enigo::new(&enigo::Settings::default());
        }
    }
);

#[derive(Default)]
pub struct Shared {
    pub active: AtomicBool,
    pub shutdown: AtomicBool,
    pub display: Arc<AtomicU32>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Arc<Shared>]
    struct WindowLifecycle;
    unsafe impl NSObjectProtocol for WindowLifecycle {}
    unsafe impl NSWindowDelegate for WindowLifecycle {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.ivars().shutdown.store(true, Ordering::Release);
        }
    }
);

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
        message: ClientMessage,
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
    pub async fn input(&self, message: ClientMessage) -> Result<()> {
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

fn label(
    window: &NSWindow,
    mtm: MainThreadMarker,
    text: &str,
    y: f64,
    size: f64,
) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFrame(NSRect::new(NSPoint::new(24.0, y), NSSize::new(552.0, 46.0)));
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    field.setSelectable(true);
    window
        .contentView()
        .expect("native content view")
        .addSubview(&field);
    field
}

struct NativeCommands {
    input: Option<Input>,
    awake: Option<crate::power::KeepAwake>,
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
                        let guard = crate::power::KeepAwake::new()?;
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

pub fn run(native: Native, rx: mpsc::Receiver<Command>) -> Result<()> {
    let mtm = MainThreadMarker::new().expect("native UI runs on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    // These retained windows never release themselves when closed.
    let panel = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 320.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe {
        panel.setReleasedWhenClosed(false);
    }
    panel.setTitle(&NSString::from_str("LanDesk"));
    let lifecycle: Retained<WindowLifecycle> = unsafe {
        let lifecycle = WindowLifecycle::alloc(mtm).set_ivars(native.shared.clone());
        msg_send![super(lifecycle), init]
    };
    panel.setDelegate(Some(ProtocolObject::from_ref(&*lifecycle)));
    panel.center();
    label(&panel, mtm, "LanDesk", 264.0, 28.0);
    label(
        &panel,
        mtm,
        "通过 LanDeskClient 建立 SSH 连接后，浏览器自动进入桌面",
        218.0,
        16.0,
    );
    let status = label(&panel, mtm, "等待连接", 170.0, 15.0);
    let permission_label = label(&panel, mtm, "", 120.0, 13.0);
    let actions: Retained<PermissionActions> =
        unsafe { msg_send![PermissionActions::alloc(mtm), init] };
    for (title, action, x) in [
        ("授权屏幕录制", sel!(requestCapture:), 24.0),
        ("授权辅助功能", sel!(requestInput:), 290.0),
    ] {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(&actions),
                Some(action),
                mtm,
            )
        };
        button.setFrame(NSRect::new(NSPoint::new(x, 72.0), NSSize::new(250.0, 32.0)));
        panel
            .contentView()
            .expect("native content view")
            .addSubview(&button);
    }
    label(
        &panel,
        mtm,
        "关闭此窗口即停止服务，最小化不影响远程连接。",
        14.0,
        13.0,
    );
    panel.makeKeyAndOrderFront(None);
    // Permission requests are explicit button actions, so configuration needs no consent prompt.
    let last_update = std::cell::Cell::new(Instant::now() - std::time::Duration::from_secs(2));
    let commands = std::cell::RefCell::new(NativeCommands {
        input: None,
        awake: None,
        copying: None,
    });
    let timer_native = native.clone();
    let timer = crate::run_loop::RunLoopTimer::new(mtm, move || {
        objc2::rc::autoreleasepool(|_| {
            commands.borrow_mut().tick(&timer_native, &rx);
            if last_update.get().elapsed().as_secs_f32() >= 1.0 {
                let (capture, input) = permissions();
                permission_label.setStringValue(&NSString::from_str(&format!(
                    "屏幕录制：{}    辅助功能：{}",
                    if capture {
                        "已授权"
                    } else {
                        "待授权，请在系统设置中添加本应用"
                    },
                    if input { "已授权" } else { "待授权" }
                )));
                let text = if timer_native.shared.active.load(Ordering::Acquire) {
                    "远程连接中 · 本机显示画面"
                } else {
                    "等待连接 · 仅监听本机，通过 SSH 加密连接"
                };
                status.setStringValue(&NSString::from_str(text));
                last_update.set(Instant::now());
            }
        });
    });
    while !native.shared.shutdown.load(Ordering::Acquire) {
        objc2::rc::autoreleasepool(|_| {
            let deadline = NSDate::dateWithTimeIntervalSinceNow(0.025);
            if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&deadline),
                unsafe { NSDefaultRunLoopMode },
                true,
            ) {
                app.sendEvent(&event);
            }
            app.updateWindows();
        });
    }
    drop(timer);
    native.shared.shutdown.store(true, Ordering::Release);
    Ok(())
}
