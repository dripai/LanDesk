//! Native privilege boundary. UI work runs on the GPUI thread; helpers run off-thread.
use super::desktop::{Handle, checked, wide};
use super::service::ServiceHandle;
use anyhow::{Context, Result, ensure};
use std::{
    mem::size_of,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Registry::*, Threading::*},
    UI::{
        Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
        Shell::*,
        WindowsAndMessaging::*,
    },
};
pub(super) struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
pub(super) fn key(path: &str) -> Result<Key> {
    let mut handle = null_mut();
    let error = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(path).as_ptr(),
            0,
            null(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            null(),
            &mut handle,
            null_mut(),
        )
    };
    ensure!(
        error == ERROR_SUCCESS,
        "无法修改启动设置: {}",
        std::io::Error::from_raw_os_error(error as i32)
    );
    Ok(Key(handle))
}
pub(super) fn value(root: HKEY, path: &str, name: &str) -> Result<Option<(u32, Vec<u8>)>> {
    let mut len = 0;
    let mut kind = 0;
    let flags = RRF_RT_ANY | RRF_NOEXPAND | RRF_SUBKEY_WOW6464KEY;
    let error = unsafe {
        RegGetValueW(
            root,
            wide(path).as_ptr(),
            wide(name).as_ptr(),
            flags,
            &mut kind,
            null_mut(),
            &mut len,
        )
    };
    if matches!(error, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        return Ok(None);
    }
    ensure!(error == 0 && len <= 65536, "无法读取启动设置 ({error})");
    let mut data = vec![0u8; len as usize];
    let error = unsafe {
        RegGetValueW(
            root,
            wide(path).as_ptr(),
            wide(name).as_ptr(),
            flags,
            &mut kind,
            data.as_mut_ptr().cast(),
            &mut len,
        )
    };
    ensure!(error == 0, "无法读取启动设置 ({error})");
    data.truncate(len as usize);
    Ok(Some((kind, data)))
}
pub(super) fn put(key: &Key, name: &str, data: Option<&(u32, Vec<u8>)>) -> Result<()> {
    let error = match data {
        Some((kind, bytes)) => unsafe {
            RegSetValueExW(
                key.0,
                wide(name).as_ptr(),
                0,
                *kind,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        },
        None => unsafe { RegDeleteValueW(key.0, wide(name).as_ptr()) },
    };
    ensure!(
        error == 0 || (data.is_none() && error == ERROR_FILE_NOT_FOUND),
        "无法保存启动设置 ({error})"
    );
    Ok(())
}
pub(super) fn configuration(service: &ServiceHandle) -> Result<(u32, PathBuf)> {
    let config = super::setup_services::configuration(service)?;
    let path = config
        .binary
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix("\" --service"))
        .context("服务程序路径格式无效")?;
    Ok((config.start, PathBuf::from(path)))
}

pub(super) fn error_box(hwnd: HWND, error: impl std::fmt::Display) {
    crate::diagnostics::log(&format!("UI error: {error}"));
    let message = format!("{error}\n\n{}", super::bootstrap::log_locations());
    unsafe {
        MessageBoxW(
            hwnd,
            wide(&message).as_ptr(),
            wide("LanDeskServer").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
#[derive(Debug)]
struct HelperCancelled;
impl std::fmt::Display for HelperCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("用户取消了管理员授权")
    }
}
impl std::error::Error for HelperCancelled {}

pub(super) fn elevated(
    hwnd: HWND,
    executable: &std::path::Path,
    arguments: &str,
    installation: bool,
) -> Result<()> {
    let parameters = wide(arguments);
    let path = wide(&executable.to_string_lossy());
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // Suppress a new console independently of the helper's GUI windows.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE,
        hwnd,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        lpParameters: parameters.as_ptr(),
        // The helper may open a password/setup/error dialog. Windows applies
        // this startup value to its first ShowWindow call, even if that call
        // requests SW_SHOW. SW_HIDE leaves an invisible modal dialog while
        // the owner is disabled waiting for the helper to exit.
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_CANCELLED as i32) {
            return Err(HelperCancelled.into());
        }
        return Err(error).context("操作后台服务需要管理员权限");
    }
    let process = Handle::new(info.hProcess, "服务设置进程")?;
    wait_for_helper(hwnd, &process, installation)?;
    let mut exit = 1;
    checked(
        unsafe { GetExitCodeProcess(process.0, &mut exit) },
        "读取服务操作结果",
    )?;
    if exit != 0 {
        let status = super::setup_services::failure_summary();
        anyhow::bail!("操作失败（进程退出码 {exit}）。具体原因见日志。\r\n{status}");
    }
    Ok(())
}

fn wait_for_helper(hwnd: HWND, process: &Handle, installation: bool) -> Result<()> {
    // Keep the owner repainting while UAC/helper dialogs are open, but prevent
    // reentrant maintenance commands and closing the owner during the wait.
    struct Disabled(HWND, bool);
    impl Drop for Disabled {
        fn drop(&mut self) {
            if self.1 {
                unsafe {
                    EnableWindow(self.0, 1);
                }
            }
        }
    }
    let enabled = !hwnd.is_null() && unsafe { IsWindowEnabled(hwnd) } != 0;
    let _disabled = Disabled(hwnd, enabled);
    if enabled {
        unsafe {
            EnableWindow(hwnd, 0);
        }
    }
    let deadline =
        (!installation).then(|| std::time::Instant::now() + std::time::Duration::from_secs(120));
    loop {
        let remaining = match deadline {
            Some(end) => {
                let left = end.saturating_duration_since(std::time::Instant::now());
                ensure!(
                    !left.is_zero(),
                    "操作超时，辅助进程可能仍在运行，请检查操作窗口和服务状态"
                );
                left.as_millis().min(u32::MAX as u128) as u32
            }
            None => INFINITE,
        };
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(1, &process.0, remaining, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        match result {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => {
                anyhow::bail!("操作超时，辅助进程可能仍在运行，请检查操作窗口和服务状态")
            }
            WAIT_FAILED => return Err(std::io::Error::last_os_error()).context("等待辅助进程失败"),
            value if value == WAIT_OBJECT_0 + 1 => {
                let mut message = MSG::default();
                while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
                    if message.message == WM_QUIT {
                        unsafe {
                            PostQuitMessage(message.wParam as i32);
                        }
                        anyhow::bail!("界面正在退出，辅助进程结果尚未确认");
                    }
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            value => anyhow::bail!("等待辅助进程返回未知状态 {value}"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    #[test]
    #[ignore = "requires an administrator Windows desktop; opens only a disposable test window"]
    fn native_setup_helper_first_dialog_is_visible() {
        assert_ne!(
            unsafe { IsUserAnAdmin() },
            0,
            "requires administrator runner"
        );
        // Use the production ShellExecuteEx/runas path and a fresh process:
        // testing ShowWindow in this process would miss STARTUPINFO behavior.
        elevated(
            null_mut(),
            &std::env::current_exe().unwrap(),
            "--exact platform::windows::setup_dialog::tests::password_dialog_first_show_child --ignored --nocapture",
            false,
        )
        .unwrap();
    }

    #[test]
    fn helper_wait_pumps_messages_and_restores_disabled_owner() -> Result<()> {
        // A signalled event stands in for a finished helper. Only the window
        // message can signal it, proving the wait dispatches messages.
        unsafe extern "system" fn test_window(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if message == WM_APP + 90 {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, isize::from(IsWindowEnabled(hwnd) == 0));
                    SetEvent(wparam as HANDLE);
                }
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        let instance = unsafe { GetModuleHandleW(null()) };
        let class_name = wide("LanDeskHelperWaitTest");
        let class = WNDCLASSW {
            lpfnWndProc: Some(test_window),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        ensure!(
            unsafe { RegisterClassW(&class) } != 0,
            "Register test window failed"
        );
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("").as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                10,
                10,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        ensure!(!hwnd.is_null(), "Create test window failed");
        let result = (|| {
            let event = Handle::new(unsafe { CreateEventW(null(), 1, 0, null()) }, "Test event")?;
            checked(
                unsafe { PostMessageW(hwnd, WM_APP + 90, event.0 as usize, 0) },
                "Post test message",
            )?;
            wait_for_helper(hwnd, &event, false)?;
            ensure!(
                unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } == 1,
                "Owner was not disabled while waiting"
            );
            ensure!(
                unsafe { IsWindowEnabled(hwnd) } != 0,
                "Owner remained disabled after completion"
            );
            Ok(())
        })();
        unsafe {
            DestroyWindow(hwnd);
            UnregisterClassW(class_name.as_ptr(), instance);
        }
        result
    }
}
