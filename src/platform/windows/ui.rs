use crate::{
    desktop::{Command, ControlSession, DesktopControl},
    platform::windows::WindowsPlatform,
    ssh_config,
};
use anyhow::{Result, ensure};
use std::{
    ptr,
    sync::{atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        WindowsAndMessaging::*,
    },
};
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn text(window: HWND, value: &str) {
    unsafe {
        SetWindowTextW(window, wide(value).as_ptr());
    }
}
struct State {
    control: DesktopControl,
    field: HWND,
    status: HWND,
    pending: Option<mpsc::Receiver<Result<u16>>>,
}
fn port_form(current: Result<u16>) -> (String, String) {
    match current {
        Ok(port) => (
            port.to_string(),
            "修改端口会重启 SSH；客户端需填写新端口。".into(),
        ),
        Err(error) => (super::ssh::DEFAULT_PORT.to_string(), error.to_string()),
    }
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State;
        if !pointer.is_null() {
            let state = &mut *pointer;
            match message {
                WM_CLOSE => {
                    state.control.shared.shutdown.store(true, Ordering::Release);
                    DestroyWindow(window);
                    return 0;
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    return 0;
                }
                WM_COMMAND if w & 0xffff == 101 => {
                    if state.pending.is_some() {
                        return 0;
                    }
                    if state.control.shared.active.load(Ordering::Acquire) {
                        text(state.status, "请先断开远控，再在本机修改 SSH 端口");
                        return 0;
                    }
                    let mut buffer = [0u16; 32];
                    let count =
                        GetWindowTextW(state.field, buffer.as_mut_ptr(), buffer.len() as i32);
                    match ssh_config::parse_port(&String::from_utf16_lossy(
                        &buffer[..count.max(0) as usize],
                    )) {
                        Err(error) => text(state.status, &error.to_string()),
                        Ok(port) => {
                            let guard = match state.control.shared.begin_maintenance() {
                                Ok(guard) => guard,
                                Err(error) => {
                                    text(state.status, &error.to_string());
                                    return 0;
                                }
                            };
                            let (tx, rx) = mpsc::channel();
                            state.pending = Some(rx);
                            text(state.status, "正在检查并应用 SSH 端口…");
                            std::thread::spawn(move || {
                                let _maintenance = guard;
                                let _ = tx.send(ssh_config::request_change(port).map(|_| port));
                            });
                        }
                    }
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(window, message, w, l)
    }
}
unsafe fn control(
    parent: HWND,
    class: &str,
    value: &str,
    id: usize,
    rect: [i32; 4],
    extra: u32,
) -> HWND {
    unsafe {
        let handle = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(value).as_ptr(),
            WS_CHILD | WS_VISIBLE | extra,
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            parent,
            id as HMENU,
            GetModuleHandleW(ptr::null()),
            ptr::null(),
        );
        SendMessageW(
            handle,
            WM_SETFONT,
            GetStockObject(DEFAULT_GUI_FONT) as usize,
            1,
        );
        handle
    }
}
pub fn run(control_state: DesktopControl, commands: mpsc::Receiver<Command>) -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(ptr::null());
        let class = wide("LanDeskServerWindow");
        let registration = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hbrBackground: (COLOR_WINDOW + 1) as _,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            ..Default::default()
        };
        ensure!(RegisterClassW(&registration) != 0, "无法注册窗口");
        let mut state = Box::new(State {
            control: control_state.clone(),
            field: ptr::null_mut(),
            status: ptr::null_mut(),
            pending: None,
        });
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("LanDesk").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            315,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            (&mut *state as *mut State).cast(),
        );
        ensure!(!window.is_null(), "无法创建窗口");
        let connection = control(
            window,
            "STATIC",
            "等待连接 · 请先启用 Windows OpenSSH Server",
            0,
            [22, 20, 540, 24],
            0,
        );
        control(window, "STATIC", "SSH 端口", 0, [22, 70, 90, 25], 0);
        let (port, port_status) = port_form(ssh_config::current_port());
        state.field = control(
            window,
            "EDIT",
            &port,
            100,
            [112, 66, 100, 28],
            WS_BORDER | WS_TABSTOP,
        );
        control(
            window,
            "BUTTON",
            "应用（管理员授权）",
            101,
            [230, 65, 235, 32],
            WS_TABSTOP,
        );
        state.status = control(window, "STATIC", &port_status, 0, [22, 115, 540, 66], 0);
        control(
            window,
            "STATIC",
            "防火墙需允许所选 SSH 端口；关闭窗口停止服务，最小化不影响连接。",
            0,
            [22, 203, 540, 44],
            0,
        );
        ShowWindow(window, SW_SHOW);
        let mut session = ControlSession::new(&WindowsPlatform);
        let mut update = Instant::now();
        while !control_state.shared.shutdown.load(Ordering::Acquire) {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    control_state.shared.shutdown.store(true, Ordering::Release);
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            for command in commands.try_iter().take(256) {
                session.handle(command);
            }
            if let Some(result) = state.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
                state.pending = None;
                text(
                    state.status,
                    &match result {
                        Ok(port) => {
                            text(state.field, &port.to_string());
                            format!("SSH 已监听 {port}；请确认防火墙允许此端口。")
                        }
                        Err(error) => format!("{error:#}"),
                    },
                );
            }
            if update.elapsed() >= Duration::from_secs(1) {
                text(
                    connection,
                    if control_state.shared.active.load(Ordering::Acquire) {
                        "远程连接中"
                    } else {
                        "等待连接 · 仅监听本机，通过 SSH 加密连接"
                    },
                );
                update = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(session);
        control_state.shared.cancel.store(true, Ordering::Release);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_port_does_not_hide_configuration_errors_or_replace_custom_port() {
        let (port, status) = port_form(Err(anyhow::anyhow!("未找到 SSH 配置")));
        assert_eq!(port, "22");
        assert_eq!(status, "未找到 SSH 配置");
        assert_eq!(port_form(Ok(2222)).0, "2222");
    }
}
