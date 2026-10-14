//! The user-session tray is separate from the always-available console service.
use super::{
    desktop::{Handle, checked, wide},
    service::ServiceHandle,
};
use anyhow::{Context, Result, ensure};
use std::{
    mem::size_of,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        Console::FreeConsole, LibraryLoader::GetModuleHandleW, Registry::*, Services::*,
        Threading::*,
    },
    UI::{Controls::BST_CHECKED, Shell::*, WindowsAndMessaging::*},
};

const CLASS: &str = "LanDeskServerSettings";
const OPTIONS: &str = r"SOFTWARE\LanDesk\Server";
const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const ENTRY: &str = "LanDeskServer";
const AUTO: usize = 101;
const MINIMIZED: usize = 102;
const APPLY: usize = 103;
const OPEN: usize = 104;
const EXIT: usize = 105;
const TRAY: u32 = WM_APP + 1;

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn key(path: &str) -> Result<Key> {
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
fn value(root: HKEY, path: &str, name: &str) -> Result<Option<(u32, Vec<u8>)>> {
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
fn put(key: &Key, name: &str, data: Option<&(u32, Vec<u8>)>) -> Result<()> {
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
fn minimized() -> Result<bool> {
    match value(HKEY_LOCAL_MACHINE, OPTIONS, "MinimizeOnStart")? {
        None => Ok(true),
        Some((REG_DWORD, bytes)) if bytes.len() == 4 => {
            Ok(u32::from_le_bytes(bytes.try_into().unwrap()) != 0)
        }
        _ => anyhow::bail!("启动最小化设置无效"),
    }
}
fn service(access: u32) -> Result<ServiceHandle> {
    let manager =
        ServiceHandle::new(unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) })?;
    ServiceHandle::new(unsafe { OpenServiceW(manager.0, wide(ENTRY).as_ptr(), access) })
        .context("请先运行安装脚本安装 LanDeskServer")
}
fn configuration(service: &ServiceHandle) -> Result<(u32, PathBuf)> {
    let mut needed = 0;
    unsafe {
        QueryServiceConfigW(service.0, null_mut(), 0, &mut needed);
    }
    ensure!(needed > 0 && needed <= 65536, "无法读取后台服务配置");
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    checked(
        unsafe { QueryServiceConfigW(service.0, buffer.as_mut_ptr().cast(), needed, &mut needed) },
        "读取服务配置",
    )?;
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let path = unsafe {
        let mut length = 0;
        while *config.lpBinaryPathName.add(length) != 0 {
            length += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(config.lpBinaryPathName, length))?
    };
    let path = path
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix("\" --service"))
        .context("服务程序路径格式无效")?;
    Ok((config.dwStartType, PathBuf::from(path)))
}

pub fn set_startup(automatic: bool, minimize: bool) -> Result<()> {
    let service = service(SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)?;
    let (previous, executable) = configuration(&service)?;
    // The helper modifies only the installed service and its own tray startup.
    ensure!(
        std::fs::canonicalize(std::env::current_exe()?)? == std::fs::canonicalize(&executable)?,
        "请从已安装的 LanDeskServer 修改启动设置"
    );
    let old_run = value(HKEY_LOCAL_MACHINE, RUN, ENTRY)?;
    let old_min = value(HKEY_LOCAL_MACHINE, OPTIONS, "MinimizeOnStart")?;
    let run = key(RUN)?;
    let settings = key(OPTIONS)?;
    let command = format!("\"{}\" --startup-ui", executable.display());
    let command = (
        REG_SZ,
        wide(&command)
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect(),
    );
    let min_value = (REG_DWORD, u32::from(minimize).to_le_bytes().to_vec());
    let result = (|| {
        put(&settings, "MinimizeOnStart", Some(&min_value))?;
        put(&run, ENTRY, automatic.then_some(&command))?;
        checked(
            unsafe {
                ChangeServiceConfigW(
                    service.0,
                    SERVICE_NO_CHANGE,
                    if automatic {
                        SERVICE_AUTO_START
                    } else {
                        SERVICE_DEMAND_START
                    },
                    SERVICE_NO_CHANGE,
                    null(),
                    null(),
                    null_mut(),
                    null(),
                    null(),
                    null(),
                    null(),
                )
            },
            "设置服务开机启动",
        )
    })();
    if let Err(error) = result {
        let rollback = (|| {
            put(&settings, "MinimizeOnStart", old_min.as_ref())?;
            put(&run, ENTRY, old_run.as_ref())?;
            checked(
                unsafe {
                    ChangeServiceConfigW(
                        service.0,
                        SERVICE_NO_CHANGE,
                        previous,
                        SERVICE_NO_CHANGE,
                        null(),
                        null(),
                        null_mut(),
                        null(),
                        null(),
                        null(),
                        null(),
                    )
                },
                "恢复服务启动设置",
            )
        })();
        if let Err(restore) = rollback {
            anyhow::bail!("{error:#}；恢复旧设置失败: {restore:#}");
        }
        return Err(error);
    }
    Ok(())
}
struct Window {
    auto: HWND,
    minimized: HWND,
    taskbar: u32,
    icon: NOTIFYICONDATAW,
    tray: std::cell::Cell<bool>,
}
fn error_box(hwnd: HWND, error: impl std::fmt::Display) {
    unsafe {
        MessageBoxW(
            hwnd,
            wide(&error.to_string()).as_ptr(),
            wide("LanDeskServer").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
}
fn refresh(window: &Window) -> Result<()> {
    let (automatic, _) = configuration(&service(SERVICE_QUERY_CONFIG)?)?;
    unsafe {
        SendMessageW(
            window.auto,
            BM_SETCHECK,
            usize::from(automatic == SERVICE_AUTO_START),
            0,
        );
        SendMessageW(window.minimized, BM_SETCHECK, usize::from(minimized()?), 0);
    }
    Ok(())
}
fn apply(hwnd: HWND, window: &Window) -> Result<()> {
    let automatic = unsafe { SendMessageW(window.auto, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
    let minimize =
        unsafe { SendMessageW(window.minimized, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
    let (previous, path) = configuration(&service(SERVICE_QUERY_CONFIG)?)?;
    if (previous == SERVICE_AUTO_START) == automatic && minimized()? == minimize {
        return Ok(());
    }
    elevated(
        hwnd,
        &path,
        &format!(
            "--set-startup {} {}",
            u8::from(automatic),
            u8::from(minimize)
        ),
    )?;
    refresh(window)
}
fn elevated(hwnd: HWND, executable: &std::path::Path, arguments: &str) -> Result<()> {
    let parameters = wide(arguments);
    let path = wide(&executable.to_string_lossy());
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    checked(
        unsafe { ShellExecuteExW(&mut info) },
        "操作后台服务需要管理员权限",
    )?;
    let process = Handle::new(info.hProcess, "服务设置进程")?;
    ensure!(
        unsafe { WaitForSingleObject(process.0, 30000) } == WAIT_OBJECT_0,
        "操作超时，请重新打开设置确认服务状态"
    );
    let mut exit = 1;
    checked(
        unsafe { GetExitCodeProcess(process.0, &mut exit) },
        "读取服务操作结果",
    )?;
    ensure!(exit == 0, "操作失败，请检查 server.log");
    Ok(())
}
pub fn start_background() -> Result<()> {
    let service = service(SERVICE_START | SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    let (_, executable) = configuration(&service)?;
    ensure!(
        std::fs::canonicalize(std::env::current_exe()?)? == std::fs::canonicalize(executable)?,
        "请启动已安装的 LanDeskServer"
    );
    if unsafe { StartServiceW(service.0, 0, null()) } == 0 {
        ensure!(
            unsafe { GetLastError() } == ERROR_SERVICE_ALREADY_RUNNING,
            "后台服务启动失败: {}",
            std::io::Error::last_os_error()
        );
    }
    for _ in 0..200 {
        let mut status = SERVICE_STATUS::default();
        checked(
            unsafe { QueryServiceStatus(service.0, &mut status) },
            "读取后台状态",
        )?;
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        ensure!(
            status.dwCurrentState != SERVICE_STOPPED,
            "后台服务启动失败，请检查 server.log"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("后台服务启动超时")
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Window;
    if !pointer.is_null() {
        let window = unsafe { &*pointer };
        if message == window.taskbar {
            window
                .tray
                .set(unsafe { Shell_NotifyIconW(NIM_ADD, &window.icon) } != 0);
            if !window.tray.get() {
                show(hwnd);
            }
            return 0;
        }
        match message {
            WM_COMMAND => {
                match wparam & 0xffff {
                    APPLY => {
                        if let Err(error) = apply(hwnd, window) {
                            error_box(hwnd, format!("{error:#}"));
                            let _ = refresh(window);
                        }
                    }
                    OPEN => show(hwnd),
                    EXIT => unsafe {
                        DestroyWindow(hwnd);
                    },
                    _ => {}
                }
                return 0;
            }
            WM_CLOSE => {
                unsafe {
                    if window.tray.get() {
                        ShowWindow(hwnd, SW_HIDE);
                    } else {
                        DestroyWindow(hwnd);
                    }
                }
                return 0;
            }
            WM_SIZE if wparam == SIZE_MINIMIZED as usize && window.tray.get() => {
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                return 0;
            }
            TRAY => {
                if lparam as u32 == WM_LBUTTONUP {
                    show(hwnd);
                }
                if lparam as u32 == WM_RBUTTONUP {
                    unsafe {
                        let menu = CreatePopupMenu();
                        if !menu.is_null() {
                            AppendMenuW(menu, MF_STRING, OPEN, wide("设置").as_ptr());
                            AppendMenuW(menu, MF_STRING, EXIT, wide("退出托盘").as_ptr());
                            let mut point = POINT::default();
                            GetCursorPos(&mut point);
                            SetForegroundWindow(hwnd);
                            TrackPopupMenu(
                                menu,
                                TPM_RIGHTBUTTON,
                                point.x,
                                point.y,
                                0,
                                hwnd,
                                null(),
                            );
                            DestroyMenu(menu);
                            PostMessageW(hwnd, WM_NULL, 0, 0);
                        }
                    }
                }
                return 0;
            }
            WM_DESTROY => {
                unsafe {
                    Shell_NotifyIconW(NIM_DELETE, &window.icon);
                    PostQuitMessage(0);
                }
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub fn run(force_show: bool) -> Result<()> {
    // Detach this UI process only; never hide the caller's PowerShell window.
    unsafe {
        FreeConsole();
    }
    let result = run_window(force_show);
    if let Err(error) = &result {
        error_box(null_mut(), format!("{error:#}"));
    }
    result
}
fn run_window(force_show: bool) -> Result<()> {
    let existing = unsafe { FindWindowW(wide(CLASS).as_ptr(), null()) };
    if !existing.is_null() {
        if force_show {
            show(existing);
        }
        return Ok(());
    }
    let service = service(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    let (automatic, executable) = configuration(&service)?;
    let mut status = SERVICE_STATUS::default();
    checked(
        unsafe { QueryServiceStatus(service.0, &mut status) },
        "读取后台状态",
    )?;
    if status.dwCurrentState == SERVICE_STOPPED {
        elevated(null_mut(), &executable, "--start-background")?;
    }
    let minimize = minimized()?;
    let instance = unsafe { GetModuleHandleW(null()) };
    let class = wide(CLASS);
    let wc = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    ensure!(unsafe { RegisterClassW(&wc) } != 0, "设置窗口注册失败");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide("LanDeskServer 设置").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            440,
            240,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    ensure!(!hwnd.is_null(), "设置窗口创建失败");
    let child =
        |class: &str, text: &str, style: u32, y: i32, width: i32, id: usize| -> Result<HWND> {
            let control = unsafe {
                CreateWindowExW(
                    0,
                    wide(class).as_ptr(),
                    wide(text).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    24,
                    y,
                    width,
                    28,
                    hwnd,
                    id as HMENU,
                    instance,
                    null(),
                )
            };
            ensure!(!control.is_null(), "设置控件创建失败");
            unsafe {
                SendMessageW(
                    control,
                    WM_SETFONT,
                    GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                    1,
                );
            }
            Ok(control)
        };
    let auto = child(
        "BUTTON",
        "开机启动",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        24,
        370,
        AUTO,
    )?;
    let min = child(
        "BUTTON",
        "启动时最小化到托盘",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        64,
        370,
        MINIMIZED,
    )?;
    child(
        "STATIC",
        "未登录也可远控；退出托盘不停止后台服务。",
        0,
        105,
        390,
        0,
    )?;
    child(
        "BUTTON",
        "保存",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        145,
        90,
        APPLY,
    )?;
    unsafe {
        SendMessageW(
            auto,
            BM_SETCHECK,
            usize::from(automatic == SERVICE_AUTO_START),
            0,
        );
        SendMessageW(min, BM_SETCHECK, usize::from(minimize), 0);
    }
    let mut icon = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: TRAY,
        hIcon: wc.hIcon,
        ..Default::default()
    };
    let tip = wide("LanDeskServer");
    icon.szTip[..tip.len()].copy_from_slice(&tip);
    let taskbar = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    ensure!(taskbar != 0, "托盘消息注册失败");
    let mut window = Box::new(Window {
        auto,
        minimized: min,
        taskbar,
        icon,
        tray: std::cell::Cell::new(false),
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *window as *mut Window) as isize);
    }
    let tray = unsafe { Shell_NotifyIconW(NIM_ADD, &window.icon) };
    window.tray.set(tray != 0);
    if force_show || !minimize || tray == 0 {
        show(hwnd);
    }
    if tray == 0 {
        error_box(hwnd, "托盘图标创建失败，设置窗口保持显示。");
    }
    let mut message = MSG::default();
    loop {
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status == 0 {
            break;
        }
        ensure!(status != -1, "窗口消息读取失败");
        unsafe {
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(())
}
