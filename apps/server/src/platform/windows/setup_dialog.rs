//! Native Win32 setup windows, sharing no shell or .NET runtime.
use super::desktop::checked;
use super::desktop::wide;
use anyhow::Context;
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
            // MAKEINTRESOURCEW(1): an integer resource ID, not a pointer to text.
            hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
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
        "移除后台服务、开机入口和防火墙规则。\r\n当前远控会断开。程序、密码配置、日志和其他文件保留。",
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
    updates: Option<std::sync::mpsc::Sender<String>>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}
impl Progress {
    pub(super) fn new(title: &str) -> Result<Self> {
        let (updates, receiver) = std::sync::mpsc::channel::<String>();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let title = title.to_owned();
        let thread = std::thread::Builder::new()
            .name("setup-window".into())
            .spawn(move || -> Result<()> {
                // HWND and its state are created, dispatched and destroyed on
                // this thread, independent of blocking driver/SCM operations.
                let view = match ProgressWindow::new(&title) {
                    Ok(view) => view,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return Ok(());
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return Ok(());
                }
                loop {
                    match receiver.try_recv() {
                        Ok(text) => view.stage(&text)?,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    unsafe {
                        let mut message = MSG::default();
                        while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                            ensure!(message.message != WM_QUIT, "安装窗口提前退出");
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                        ensure!(
                            MsgWaitForMultipleObjectsEx(
                                0,
                                null(),
                                50,
                                QS_ALLINPUT,
                                MWMO_INPUTAVAILABLE
                            ) != WAIT_FAILED,
                            "等待安装窗口消息失败: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                }
            })?;
        let progress = Self {
            updates: Some(updates),
            thread: Some(thread),
        };
        initialized.recv().context("安装窗口线程启动失败")??;
        Ok(progress)
    }
    pub(super) fn stage(&self, text: &str) -> Result<()> {
        crate::diagnostics::log(text);
        self.updates
            .as_ref()
            .context("安装窗口已关闭")?
            .send(text.to_owned())
            .context("安装窗口线程已退出")
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        self.updates.take();
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => crate::diagnostics::log(&format!("安装窗口失败: {error:#}")),
                Err(_) => crate::diagnostics::log("安装窗口线程异常退出"),
            }
        }
    }
}
struct ProgressWindow {
    window: Window,
    label: HWND,
    _state: Box<State>,
}
impl ProgressWindow {
    fn new(title: &str) -> Result<Self> {
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
    fn stage(&self, text: &str) -> Result<()> {
        unsafe {
            checked(
                SetWindowTextW(self.label, wide(text).as_ptr()),
                "更新安装进度",
            )?;
            UpdateWindow(self.window.0);
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
    #[ignore = "Requires native Windows window message dispatch"]
    fn native_setup_progress_responds_while_installer_waits() -> Result<()> {
        let title = format!("LanDesk progress test {}", std::process::id());
        let progress = Progress::new(&title)?;
        let hwnd = unsafe { FindWindowW(wide(CLASS).as_ptr(), wide(&title).as_ptr()) };
        ensure!(!hwnd.is_null(), "Progress window missing");
        let thread = unsafe { GetWindowThreadProcessId(hwnd, null_mut()) };
        ensure!(
            thread != unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
            "Installer still owns the UI message loop"
        );
        progress.stage("Waiting for desktop worker")?;
        // The installer thread blocks here. The window must respond without
        // any call to stage() pumping messages on this thread.
        ensure!(
            unsafe {
                SendMessageTimeoutW(
                    hwnd,
                    WM_NULL,
                    0,
                    0,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    1000,
                    null_mut(),
                )
            } != 0,
            "Progress window stopped responding during installer wait"
        );
        drop(progress);
        ensure!(unsafe { IsWindow(hwnd) } == 0, "Progress window leaked");
        Ok(())
    }

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
