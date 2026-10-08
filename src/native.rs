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
    NSFont, NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardTypeString, NSScreen,
    NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
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
    pub cancel: AtomicBool,
    pub shutdown: AtomicBool,
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
pub struct Native {
    pub tx: mpsc::Sender<Command>,
    pub shared: Arc<Shared>,
}

impl Native {
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

pub fn run(native: Native, rx: mpsc::Receiver<Command>) -> Result<()> {
    let mtm = MainThreadMarker::new().expect("native UI runs on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let frame = NSScreen::mainScreen(mtm)
        .ok_or_else(|| anyhow::anyhow!("没有显示器"))?
        .frame();
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
    let mut last_update = Instant::now() - std::time::Duration::from_secs(2);
    // macOS input-source APIs used by Enigo require the main thread.
    let mut input: Option<Input> = None;
    let mut awake: Option<crate::power::KeepAwake> = None;
    while !native.shared.shutdown.load(Ordering::Acquire) {
        objc2::rc::autoreleasepool(|_| {
            while let Ok(command) = rx.try_recv() {
                match command {
                    Command::PasteImage { png, reply } => {
                        let result = (|| {
                            let input = input
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
                        let result = (|| {
                            let text = NSPasteboard::generalPasteboard()
                                .stringForType(unsafe { NSPasteboardTypeString })
                                .ok_or_else(|| {
                                    anyhow::anyhow!("Mac 剪贴板没有文字；不支持图片或文件")
                                })?
                                .to_string();
                            ensure!(!text.is_empty(), "Mac 剪贴板文字为空");
                            ensure!(
                                text.len() <= MAX_TEXT_BYTES && !text.contains('\0'),
                                "剪贴板文字超过 64 KiB 或包含空字符"
                            );
                            Ok(text)
                        })();
                        let _ = reply.send(result);
                    }
                    Command::BeginInput {
                        width,
                        height,
                        reply,
                    } => {
                        let result = (|| {
                            ensure!(input.is_none(), "键鼠控制尚未结束");
                            let guard = crate::power::KeepAwake::new()?;
                            input = Some(Input::new(width, height)?);
                            awake = Some(guard);
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    }
                    Command::Input { message, reply } => {
                        let result = input
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("键鼠控制尚未启动"))
                            .and_then(|input| input.handle(message));
                        let _ = reply.send(result);
                    }
                    Command::RestoreSession { reply } => {
                        let release = input
                            .take()
                            .map(|mut input| input.release_all())
                            .unwrap_or(Ok(()));
                        drop(awake.take());
                        if let Some(reply) = reply {
                            let _ = reply.send(release);
                        }
                    }
                }
            }
            if native.shared.active.load(Ordering::Acquire) {
                let screens = NSScreen::screens(mtm);
                if screens.len() != 1 || screens.objectAtIndex(0).frame() != frame {
                    native.shared.cancel.store(true, Ordering::Release);
                }
            }
            if last_update.elapsed().as_secs_f32() >= 1.0 {
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
                let text = if native.shared.active.load(Ordering::Acquire) {
                    "远程连接中 · 本机显示画面"
                } else {
                    "等待连接 · 仅监听本机，通过 SSH 加密连接"
                };
                status.setStringValue(&NSString::from_str(text));
                last_update = Instant::now();
            }
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
    drop(input);
    drop(awake);
    native.shared.cancel.store(true, Ordering::Release);
    native.shared.shutdown.store(true, Ordering::Release);
    Ok(())
}
