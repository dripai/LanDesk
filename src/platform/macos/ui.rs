use crate::{
    desktop::ControlSession,
    desktop::{Command, DesktopControl, Shared},
    platform::macos::MacPlatform,
};
use anyhow::Result;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSEventMask,
    NSFont, NSScreen, NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};
use std::{
    cell::RefCell,
    sync::atomic::Ordering,
    sync::{Arc, mpsc},
    time::Instant,
};

struct PortState {
    input: Retained<NSTextField>,
    status: Retained<NSTextField>,
    shared: Arc<Shared>,
    pending: RefCell<Option<mpsc::Receiver<Result<u16>>>>,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PortState]
    struct PortActions;
    unsafe impl NSObjectProtocol for PortActions {}
    impl PortActions {
        #[unsafe(method(applyPort:))]
        fn apply_port(&self, _sender: &AnyObject) {
            let state = self.ivars();
            if state.pending.borrow().is_some() { return; }
            let result = (|| {
                anyhow::ensure!(!state.shared.active.load(Ordering::Acquire), "请先断开远控，再在本机修改 SSH 端口");
                crate::ssh_config::parse_port(&state.input.stringValue().to_string())
            })();
            match result {
                Err(error) => state.status.setStringValue(&NSString::from_str(&error.to_string())),
                Ok(port) => {
                    let guard = match state.shared.begin_maintenance() { Ok(guard) => guard, Err(error) => { state.status.setStringValue(&NSString::from_str(&error.to_string())); return; } };
                    let (tx, rx) = mpsc::channel();
                    *state.pending.borrow_mut() = Some(rx);
                    state.status.setStringValue(&NSString::from_str("等待管理员授权并重启 SSH…"));
                    std::thread::spawn(move || {
                        let _maintenance = guard; let _ = tx.send(crate::ssh_config::request_change(port).map(|_| port)); });
                }
            }
        }
    }
);

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

pub fn run(native: DesktopControl, rx: mpsc::Receiver<Command>) -> Result<()> {
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
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 420.0)),
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
    label(&panel, mtm, "LanDesk", 364.0, 28.0);
    label(
        &panel,
        mtm,
        "通过 LanDeskClient 建立 SSH 连接后，浏览器自动进入桌面",
        318.0,
        16.0,
    );
    let status = label(&panel, mtm, "等待连接", 270.0, 15.0);
    let permission_label = label(&panel, mtm, "", 220.0, 13.0);
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
        button.setFrame(NSRect::new(
            NSPoint::new(x, 182.0),
            NSSize::new(250.0, 32.0),
        ));
        panel
            .contentView()
            .expect("native content view")
            .addSubview(&button);
    }
    let port_label = label(&panel, mtm, "SSH 端口", 126.0, 14.0);
    port_label.setFrame(NSRect::new(
        NSPoint::new(24.0, 131.0),
        NSSize::new(90.0, 24.0),
    ));
    let current = crate::ssh_config::current_port();
    let field = NSTextField::textFieldWithString(
        &NSString::from_str(&current.as_ref().map(u16::to_string).unwrap_or_default()),
        mtm,
    );
    field.setFrame(NSRect::new(
        NSPoint::new(115.0, 128.0),
        NSSize::new(110.0, 28.0),
    ));
    panel.contentView().unwrap().addSubview(&field);
    let port_status = label(
        &panel,
        mtm,
        &current
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "应用会重启系统 SSH；客户端需填写新端口。".into()),
        67.0,
        12.0,
    );
    let port_actions: Retained<PortActions> = unsafe {
        let actions = PortActions::alloc(mtm).set_ivars(PortState {
            input: field,
            status: port_status,
            shared: native.shared.clone(),
            pending: RefCell::new(None),
        });
        msg_send![super(actions), init]
    };
    let apply = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("应用（管理员授权）"),
            Some(&port_actions),
            Some(sel!(applyPort:)),
            mtm,
        )
    };
    apply.setFrame(NSRect::new(
        NSPoint::new(245.0, 126.0),
        NSSize::new(220.0, 32.0),
    ));
    panel.contentView().unwrap().addSubview(&apply);
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
    let mut session = ControlSession::new(&MacPlatform);
    while !native.shared.shutdown.load(Ordering::Acquire) {
        objc2::rc::autoreleasepool(|_| {
            let result = port_actions
                .ivars()
                .pending
                .borrow()
                .as_ref()
                .and_then(|rx| rx.try_recv().ok());
            if let Some(result) = result {
                *port_actions.ivars().pending.borrow_mut() = None;
                let text = match result {
                    Ok(port) => format!("SSH 已监听 {port}；客户端请填写此端口。"),
                    Err(error) => format!("{error:#}"),
                };
                port_actions
                    .ivars()
                    .status
                    .setStringValue(&NSString::from_str(&text));
            }
            while let Ok(command) = rx.try_recv() {
                session.handle(command);
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
    drop(session);
    native.shared.cancel.store(true, Ordering::Release);
    native.shared.shutdown.store(true, Ordering::Release);
    Ok(())
}
