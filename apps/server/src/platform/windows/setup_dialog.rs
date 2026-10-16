//! Native Win32 setup windows, sharing no shell or .NET runtime.
use super::desktop::checked;
use super::desktop::wide;
use anyhow::Result;
use anyhow::ensure;
use std::cell::Cell;
use std::ptr::null;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::UpdateWindow;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::BST_CHECKED;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "LanDeskNativeSetup";
const ACCEPT: usize = 1;
struct State {
    done: Cell<bool>,
    accepted: Cell<bool>,
    progress: bool,
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if !state.is_null() {
            match message {
                WM_COMMAND if w & 0xffff == ACCEPT => {
                    (*state).accepted.set(true);
                    (*state).done.set(true);
                    return 0;
                }
                WM_CLOSE if !(*state).progress => {
                    (*state).done.set(true);
                    return 0;
                }
                WM_CLOSE => return 0,
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, w, l)
    }
}
fn window(title: &str, height: i32, state: &State) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW + 1) as _,
            ..Default::default()
        };
        ensure!(
            RegisterClassW(&wc) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
            "注册安装窗口失败"
        );
        let x = (GetSystemMetrics(SM_CXSCREEN) - 500) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_CAPTION | WS_SYSMENU,
            x,
            y,
            500,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        ensure!(
            !hwnd.is_null(),
            "创建安装窗口失败: {}",
            std::io::Error::last_os_error()
        );
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as *const State as isize);
        Ok(hwnd)
    }
}
fn control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    bounds: (i32, i32, i32, i32),
    id: usize,
) -> Result<HWND> {
    let (x, y, width, height) = bounds;
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            width,
            height,
            parent,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        )
    };
    ensure!(
        !hwnd.is_null(),
        "创建安装控件失败: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            windows_sys::Win32::Graphics::Gdi::GetStockObject(
                windows_sys::Win32::Graphics::Gdi::DEFAULT_GUI_FONT,
            ) as usize,
            1,
        );
    }
    Ok(hwnd)
}
fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetForegroundWindow(hwnd);
    }
}
fn modal(hwnd: HWND, state: &State) -> Result<()> {
    show(hwnd);
    // State changes only through synchronous window-message dispatch on this thread.
    let mut message = MSG::default();
    loop {
        if state.done.get() {
            break;
        }
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        ensure!(result > 0, "安装窗口消息循环退出 ({result})");
        unsafe {
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(())
}
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
pub(super) fn uninstall_choice() -> Result<Option<bool>> {
    let state = State {
        done: Cell::new(false),
        accepted: Cell::new(false),
        progress: false,
    };
    let window = Window(window("卸载 LanDeskServer", 245, &state)?);
    control(
        window.0,
        "STATIC",
        "移除后台服务、开机入口、防火墙规则和程序。\r\n当前远控会断开。密码配置、日志和其他已有文件会保留。",
        0,
        (20, 18, 450, 65),
        0,
    )?;
    let checkbox = control(
        window.0,
        "BUTTON",
        "同时移除 LanDesk 虚拟显示器",
        WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        (20, 95, 450, 30),
        2,
    )?;
    control(
        window.0,
        "BUTTON",
        "卸载",
        WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        (340, 155, 130, 30),
        ACCEPT,
    )?;
    modal(window.0, &state)?;
    Ok(state
        .accepted
        .get()
        .then(|| unsafe { SendMessageW(checkbox, BM_GETCHECK, 0, 0) == BST_CHECKED as isize }))
}
pub(super) struct Progress {
    window: Window,
    label: HWND,
    _state: Box<State>,
}
impl Progress {
    pub(super) fn new(title: &str) -> Result<Self> {
        let state = Box::new(State {
            done: Cell::new(false),
            accepted: Cell::new(false),
            progress: true,
        });
        let window = Window(window(title, 155, &state)?);
        let label = control(
            window.0,
            "STATIC",
            "正在检查安装环境…",
            0,
            (20, 25, 450, 65),
            0,
        )?;
        show(window.0);
        Ok(Self {
            window,
            label,
            _state: state,
        })
    }
    pub(super) fn stage(&self, text: &str) -> Result<()> {
        crate::diagnostics::log(text);
        unsafe {
            checked(
                SetWindowTextW(self.label, wide(text).as_ptr()),
                "更新安装进度",
            )?;
            UpdateWindow(self.window.0);
            let mut message = MSG::default();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}
pub(super) fn complete(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("LanDeskServer").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "child probe launched by native_setup_helper_first_dialog_is_visible"]
    fn password_dialog_first_show_child() {
        let state = State {
            done: Cell::new(false),
            accepted: Cell::new(false),
            progress: false,
        };
        // Installation dialogs must honor the initial ShowWindow state.
        // No credentials, services or installed configuration are touched.
        let dialog = Window(window("LanDesk dialog visibility test", 215, &state).unwrap());
        show(dialog.0);
        assert_ne!(
            unsafe { IsWindowVisible(dialog.0) },
            0,
            "helper dialog is hidden"
        );
        unsafe {
            SendMessageW(dialog.0, WM_CLOSE, 0, 0);
        }
        assert!(state.done.get(), "dialog cannot be closed");
        assert!(!state.accepted.get(), "closing must not save a password");
    }
}
