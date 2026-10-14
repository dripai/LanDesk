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
    UI::{
        Controls::{BST_CHECKED, EM_SETSEL},
        Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
        Shell::*,
        WindowsAndMessaging::*,
    },
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
const EXPORT: usize = 106;
const UNINSTALL: usize = 107;
const PASSWORD: usize = 108;
const RANDOM_PASSWORD: usize = 109;
const COPY_PASSWORD: usize = 110;
const REPAIR: usize = 111;
const TRAY: u32 = WM_APP + 1;
const INITIALIZE: u32 = WM_APP + 2;

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
    super::setup_services::open_access(ENTRY, access)?.context("LanDeskServer 后台服务未安装")
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
    password: HWND,
    connection: HWND,
    status: HWND,
    taskbar: u32,
    icon: NOTIFYICONDATAW,
    tray: std::cell::Cell<bool>,
}
fn password_info() -> Result<(u16, zeroize::Zeroizing<String>)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let info = runtime.block_on(super::password_pipe::read())?;
    Ok((info.port, info.password()?))
}
fn refresh_password(window: &Window) -> Result<()> {
    // Clear stale text even if the helper committed a change but its response failed.
    unsafe {
        SetWindowTextW(window.password, wide("").as_ptr());
    }
    let (port, password) = password_info()?;
    checked(
        unsafe {
            SetWindowTextW(
                window.connection,
                wide(&format!("连接端口：{port}     连接密码")).as_ptr(),
            )
        },
        "显示连接端口",
    )?;
    let text = zeroize::Zeroizing::new(wide(&password));
    checked(
        unsafe { SetWindowTextW(window.password, text.as_ptr()) },
        "显示连接密码",
    )
}
fn refresh_runtime(window: &Window) -> Result<()> {
    unsafe {
        SetWindowTextW(window.password, wide("").as_ptr());
        SetWindowTextW(window.connection, wide("连接密码暂不可用").as_ptr());
    }
    for name in [
        super::setup_services::TRANSPORT,
        super::setup_services::SERVER,
    ] {
        let service = super::setup_services::open_access(name, SERVICE_QUERY_STATUS)?
            .with_context(|| format!("服务 {name} 未安装"))?;
        let state = super::setup_services::status(&service)?;
        ensure!(
            state.dwCurrentState == SERVICE_RUNNING,
            "{name} 未运行：state={}，exit={}，service_exit={}。可修复日志并启动，或导出诊断。",
            state.dwCurrentState,
            state.dwWin32ExitCode,
            state.dwServiceSpecificExitCode
        );
    }
    refresh_password(window)
}
fn display_result(window: &Window, result: Result<()>) {
    let text = match result {
        Ok(()) => "后台服务运行中".to_owned(),
        Err(error) => {
            let message = format!("{error:#}");
            crate::diagnostics::log(&format!("Management status: {message}"));
            message
        }
    };
    unsafe {
        SetWindowTextW(window.status, wide(&text).as_ptr());
    }
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
        false,
    )?;
    crate::diagnostics::log("Startup preferences saved");
    refresh(window)
}
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
    wait_for_helper(hwnd, &process, installation)?;
    let mut exit = 1;
    checked(
        unsafe { GetExitCodeProcess(process.0, &mut exit) },
        "读取服务操作结果",
    )?;
    if exit != 0 {
        let status = super::setup_services::failure_summary();
        anyhow::bail!("操作失败（进程退出码 {exit}）。具体原因见辅助进程提示。\r\n{status}");
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
pub fn start_background() -> Result<()> {
    let service = service(SERVICE_START | SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
    let (_, executable) = configuration(&service)?;
    ensure!(
        std::fs::canonicalize(std::env::current_exe()?)? == std::fs::canonicalize(executable)?,
        "请启动已安装的 LanDeskServer"
    );
    drop(service);
    // Start the dependency explicitly so failures name the actual failing service.
    super::setup_services::start(super::setup_services::TRANSPORT)?;
    super::setup_services::start(super::setup_services::SERVER)
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
            INITIALIZE => {
                let result = (|| -> Result<bool> {
                    refresh(window)?;
                    let handle = service(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG)?;
                    let state = super::setup_services::status(&handle)?;
                    let (_, executable) = configuration(&handle)?;
                    drop(handle);
                    if state.dwCurrentState == SERVICE_STOPPED {
                        elevated(hwnd, &executable, "--start-background", false)?;
                    }
                    refresh_runtime(window)?;
                    minimized()
                })();
                let hide = matches!(result, Ok(true)) && wparam == 0 && window.tray.get();
                display_result(window, result.map(|_| ()));
                if hide {
                    unsafe {
                        ShowWindow(hwnd, SW_HIDE);
                    }
                }
                return 0;
            }
            WM_COMMAND => {
                if unsafe { IsWindowEnabled(hwnd) } == 0 {
                    return 0;
                }
                match wparam & 0xffff {
                    REPAIR => {
                        let result = (|| {
                            let (_, path) = configuration(&service(SERVICE_QUERY_CONFIG)?)?;
                            elevated(hwnd, &path, "--repair-runtime", false)
                        })();
                        let refreshed = refresh_runtime(window);
                        display_result(window, result.and(refreshed));
                    }
                    APPLY => {
                        if let Err(error) = apply(hwnd, window) {
                            error_box(hwnd, format!("{error:#}"));
                            let _ = refresh(window);
                        }
                    }
                    EXPORT => {
                        let result = (|| -> Result<()> {
                            elevated(hwnd, &std::env::current_exe()?, "--export-logs", false)?;
                            super::bootstrap::open_folder(&super::bootstrap::diagnostic_exports()?)
                        })();
                        if let Err(error) = result {
                            error_box(hwnd, format!("{error:#}"));
                        }
                    }
                    PASSWORD | RANDOM_PASSWORD => {
                        let command = if wparam & 0xffff == PASSWORD {
                            "--change-password"
                        } else {
                            "--regenerate-password"
                        };
                        let result = std::env::current_exe()
                            .map_err(anyhow::Error::from)
                            .and_then(|path| elevated(hwnd, &path, command, true));
                        let refreshed = refresh_password(window);
                        display_result(window, result.and(refreshed));
                    }
                    COPY_PASSWORD => unsafe {
                        SendMessageW(window.password, EM_SETSEL, 0, -1);
                        SendMessageW(window.password, WM_COPY, 0, 0);
                    },
                    UNINSTALL => {
                        if super::bootstrap::request_uninstall().is_ok() {
                            unsafe {
                                DestroyWindow(hwnd);
                            }
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
                if unsafe { IsWindowEnabled(hwnd) } == 0 {
                    return 0;
                }
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
                            AppendMenuW(menu, MF_STRING, EXPORT, wide("导出日志").as_ptr());
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
    let result = (|| {
        let installed_now = super::bootstrap::install_if_needed()?;
        run_window(force_show || installed_now)
    })();
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
            550,
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
    let connection = child("STATIC", "连接密码", 0, 24, 390, 0)?;
    let password = child(
        "EDIT",
        "",
        WS_BORDER | ES_READONLY as u32 | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
        60,
        370,
        0,
    )?;
    for (title, id, x, width) in [
        ("复制密码", COPY_PASSWORD, 24, 100),
        ("随机更换", RANDOM_PASSWORD, 134, 110),
        ("修改密码", PASSWORD, 254, 140),
    ] {
        let button = child(
            "BUTTON",
            title,
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            104,
            width,
            id,
        )?;
        checked(
            unsafe { MoveWindow(button, x, 104, width, 28, 1) },
            "排列密码按钮",
        )?;
    }
    let auto = child(
        "BUTTON",
        "开机启动",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        160,
        370,
        AUTO,
    )?;
    let min = child(
        "BUTTON",
        "启动时最小化到托盘",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        200,
        370,
        MINIMIZED,
    )?;
    child("STATIC", "退出托盘不停止后台服务。", 0, 241, 390, 0)?;
    child(
        "BUTTON",
        "保存",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        281,
        90,
        APPLY,
    )?;
    child(
        "BUTTON",
        "导出日志",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        321,
        120,
        EXPORT,
    )?;
    child(
        "BUTTON",
        "卸载服务",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        361,
        120,
        UNINSTALL,
    )?;
    let repair = child(
        "BUTTON",
        "修复日志并启动",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        361,
        180,
        REPAIR,
    )?;
    checked(
        unsafe { MoveWindow(repair, 200, 361, 194, 28, 1) },
        "排列维护按钮",
    )?;
    let status = child(
        "EDIT",
        "正在读取后台状态…",
        ES_READONLY as u32 | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
        404,
        370,
        0,
    )?;
    checked(
        unsafe { MoveWindow(status, 24, 404, 370, 90, 1) },
        "排列服务状态",
    )?;
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
        password,
        connection,
        status,
        taskbar,
        icon,
        tray: std::cell::Cell::new(false),
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *window as *mut Window) as isize);
    }
    let tray = unsafe { Shell_NotifyIconW(NIM_ADD, &window.icon) };
    window.tray.set(tray != 0);
    show(hwnd);
    unsafe {
        PostMessageW(hwnd, INITIALIZE, usize::from(force_show), 0);
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

#[cfg(test)]
mod tests {
    use super::*;
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
