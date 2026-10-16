use super::capture::WindowsCapture;
use super::desktop::Handle;
use super::desktop::checked;
use super::desktop::wide;
use super::input::NoFiles;
use super::input::WindowsControl;
use crate::adapters::web::AppState;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use landesk_core::application::capture::CaptureService;
use landesk_core::application::file_worker::FileService;
use landesk_core::application::runtime::Shared;
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr::null;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authentication::Identity::SendSAS;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
use windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext;

const NAME: &str = "LanDeskServer";
const TRANSPORT_NAME: &str = "LanDeskTransport";
static TRANSPORT: AtomicBool = AtomicBool::new(false);
const SAS_CONTROL: u32 = 128;
static STOP: AtomicBool = AtomicBool::new(false);
static SAS: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(null_mut());

fn data_dir() -> Result<PathBuf> {
    landesk_storage::application_data("server")
}
pub use crate::diagnostics::log;
fn process_token() -> Result<Handle> {
    let mut token = null_mut();
    checked(
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_PRIVILEGES,
                &mut token,
            )
        },
        "OpenProcessToken",
    )?;
    Handle::new(token, "Process token")
}
fn require_system() -> Result<()> {
    let token = process_token()?;
    let mut buffer = [0usize; 128];
    let mut needed = 0;
    checked(
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                &mut needed,
            )
        },
        "GetTokenInformation",
    )?;
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    ensure!(
        unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) } != 0,
        "请通过已安装的 LanDeskServer 服务启动，不能直接运行桌面工作进程"
    );
    Ok(())
}
pub fn require_system_console() -> Result<()> {
    require_system()?;
    let mut id = 0;
    checked(
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) },
        "读取进程会话",
    )?;
    ensure!(
        id != 0 && id == unsafe { WTSGetActiveConsoleSessionId() },
        "当前进程不在活动控制台会话，请重新连接"
    );
    Ok(())
}
fn enable_privileges() -> Result<()> {
    let token = process_token()?;
    for name in [
        "SeTcbPrivilege",
        "SeAssignPrimaryTokenPrivilege",
        "SeIncreaseQuotaPrivilege",
        "SeDebugPrivilege",
    ] {
        let mut luid = LUID::default();
        checked(
            unsafe { LookupPrivilegeValueW(null(), wide(name).as_ptr(), &mut luid) },
            "LookupPrivilegeValue",
        )?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        unsafe {
            SetLastError(0);
        }
        checked(
            unsafe { AdjustTokenPrivileges(token.0, 0, &privileges, 0, null_mut(), null_mut()) },
            "AdjustTokenPrivileges",
        )?;
        ensure!(unsafe { GetLastError() } == 0, "服务缺少权限 {name}");
    }
    Ok(())
}
fn session_token(session: u32) -> Result<Handle> {
    super::interactive::session_token(session)
}
fn named_stop_event(name: &str) -> Result<Handle> {
    unsafe {
        let mut descriptor = null_mut();
        checked(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide("D:P(A;;GA;;;SY)(A;;GA;;;BA)").as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            ),
            "事件访问控制",
        )?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let event = CreateEventW(&attributes, 1, 0, wide(name).as_ptr());
        let error = GetLastError();
        LocalFree(descriptor);
        let event = Handle::new(event, "CreateEvent")?;
        ensure!(error != ERROR_ALREADY_EXISTS, "随机工作进程事件已存在");
        Ok(event)
    }
}
struct Worker {
    session: u32,
    process: Handle,
    stop: Handle,
    _job: Handle,
}
impl Worker {
    fn start(session: u32) -> Result<Self> {
        let token = session_token(session)?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("随机事件生成失败: {e}"))?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let event = format!(r"Global\LanDeskServer-{suffix}");
        let stop = named_stop_event(&event)?;
        let job = Handle::new(
            unsafe { CreateJobObjectW(null(), null()) },
            "CreateJobObject",
        )?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        checked(
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of_val(&limits) as u32,
                )
            },
            "SetInformationJobObject",
        )?;
        let path = std::env::current_exe()?;
        let path = path.to_str().context("程序路径不是有效 Unicode")?;
        let mut command = wide(&format!("\"{path}\" --worker {event}"));
        let mut desktop = wide(r"winsta0\default");
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop.as_mut_ptr(),
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        checked(
            unsafe {
                CreateProcessAsUserW(
                    token.0,
                    wide(path).as_ptr(),
                    command.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    CREATE_SUSPENDED | CREATE_NO_WINDOW,
                    null(),
                    null(),
                    &startup,
                    &mut process,
                )
            },
            "启动控制台工作进程",
        )?;
        let process_handle = Handle::new(process.hProcess, "Worker process")?;
        let thread = Handle::new(process.hThread, "Worker thread")?;
        let result = (|| {
            checked(
                unsafe { AssignProcessToJobObject(job.0, process_handle.0) },
                "AssignProcessToJobObject",
            )?;
            ensure!(
                unsafe { ResumeThread(thread.0) } != u32::MAX,
                "启动工作线程失败"
            );
            Ok(())
        })();
        if let Err(error) = result {
            unsafe {
                TerminateProcess(process_handle.0, 1);
                WaitForSingleObject(process_handle.0, 5000);
            }
            return Err(error);
        }
        log(&format!(
            "Console worker started: session={session}, pid={}",
            process.dwProcessId
        ));
        Ok(Self {
            session,
            process: process_handle,
            stop,
            _job: job,
        })
    }
    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process.0, 0) == WAIT_TIMEOUT }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.0);
            if WaitForSingleObject(self.process.0, 5000) != WAIT_OBJECT_0 {
                log("Worker did not stop in 5 seconds; terminating its job");
                TerminateJobObject(self._job.0, 1);
                WaitForSingleObject(self.process.0, 5000);
            }
        }
    }
}

fn report(state: u32, error: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: u32::from(error == ERROR_SERVICE_SPECIFIC_ERROR),
        dwWaitHint: if state == SERVICE_STOP_PENDING {
            10000
        } else {
            0
        },
        ..Default::default()
    };
    unsafe {
        SetServiceStatus(STATUS.load(Ordering::Acquire), &status);
    }
}
unsafe extern "system" fn control(
    code: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::Release);
            report(SERVICE_STOP_PENDING, 0);
        }
        SERVICE_CONTROL_INTERROGATE => {}
        SAS_CONTROL if !TRANSPORT.load(Ordering::Relaxed) => {
            SAS.store(true, Ordering::Release);
        }
        _ => return ERROR_CALL_NOT_IMPLEMENTED,
    }
    0
}
fn sas_policy() -> Result<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System").as_ptr(),
            wide("SoftwareSASGeneration").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    ensure!(
        status == 0,
        "读取软件 SAS 策略失败: {}",
        std::io::Error::from_raw_os_error(status as i32)
    );
    ensure!(
        matches!(value, 1 | 3),
        "系统策略未允许服务发送 Ctrl+Alt+Delete，当前值为 {value}"
    );
    Ok(value)
}
fn send_sas(session: u32) -> Result<()> {
    let policy = sas_policy()?;
    log(&format!(
        "SAS dispatch starting: session={session}, SoftwareSASGeneration={policy}"
    ));
    let token = session_token(session)?;
    checked(
        unsafe { ImpersonateLoggedOnUser(token.0) },
        "SAS session impersonation",
    )?;
    unsafe {
        SendSAS(0);
    }
    if unsafe { RevertToSelf() } == 0 {
        // Continuing a privileged service under an unexpected identity is unsafe.
        log("RevertToSelf failed after SAS");
        std::process::abort();
    }
    // SendSAS is VOID: returning does not prove the login UI reacted.
    log(&format!(
        "SendSAS returned: session={session}; Windows provides no result acknowledgment"
    ));
    Ok(())
}
pub(super) struct ServiceHandle(pub SC_HANDLE);
impl ServiceHandle {
    pub(super) fn new(handle: SC_HANDLE) -> Result<Self> {
        ensure!(
            !handle.is_null(),
            "打开 Windows 服务失败: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self(handle))
    }
}
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
pub fn request_sas() -> Result<()> {
    log(&format!(
        "SAS requested by worker; desktop={}",
        super::desktop::context()
    ));
    require_system_console()?;
    sas_policy()?;
    let manager =
        ServiceHandle::new(unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) })?;
    let service = ServiceHandle::new(unsafe {
        OpenServiceW(manager.0, wide(NAME).as_ptr(), SERVICE_USER_DEFINED_CONTROL)
    })?;
    checked(
        unsafe { ControlService(service.0, SAS_CONTROL, &mut SERVICE_STATUS::default()) },
        "请求 Ctrl+Alt+Delete",
    )?;
    log("SAS request accepted by SCM; pending service dispatch");
    Ok(())
}
fn serve() -> Result<()> {
    require_system()?;
    enable_privileges()?;
    // Validate configuration before telling SCM that service startup succeeded.
    let _ = read_transport()?;
    report(SERVICE_RUNNING, 0);
    let mut worker: Option<Worker> = None;
    let mut failures = 0;
    let mut last_session = u32::MAX;
    while !STOP.load(Ordering::Acquire) {
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session != last_session {
            worker.take();
            failures = 0;
            log(&format!(
                "Console session changed: {last_session} -> {session}"
            ));
            last_session = session;
        } else if worker.as_ref().is_some_and(|w| !w.alive()) {
            worker.take();
            failures += 1;
            ensure!(failures < 3, "控制台工作进程连续退出，请检查 server.log");
            log("Console worker exited unexpectedly; restarting");
        }
        if session != u32::MAX && session != 0 && worker.is_none() {
            match Worker::start(session) {
                Ok(next) => {
                    worker = Some(next);
                }
                Err(error) => {
                    failures += 1;
                    log(&format!("Worker startup failed ({failures}): {error:#}"));
                    if failures >= 3 {
                        return Err(error);
                    }
                }
            }
        }
        if SAS.swap(false, Ordering::AcqRel) {
            if let Some(worker) = &worker {
                if let Err(error) = send_sas(worker.session) {
                    log(&format!("SAS failed: {error:#}"));
                }
            } else {
                log("SAS dispatch failed: no console worker exists");
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(worker);
    Ok(())
}
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            wide(if TRANSPORT.load(Ordering::Relaxed) {
                TRANSPORT_NAME
            } else {
                NAME
            })
            .as_ptr(),
            Some(control),
            null(),
        )
    };
    if handle.is_null() {
        log("RegisterServiceCtrlHandler failed");
        return;
    }
    STATUS.store(handle, Ordering::Release);
    report(SERVICE_START_PENDING, 0);
    let result = (|| {
        let mode = if TRANSPORT.load(Ordering::Relaxed) {
            "--transport-service"
        } else {
            "--service"
        };
        super::bootstrap::initialize_logging(&[mode.into()])
            .context("服务日志初始化失败，请检查程序目录日志权限")?;
        if TRANSPORT.load(Ordering::Relaxed) {
            serve_transport()
        } else {
            serve()
        }
    })();
    match result {
        Ok(()) => report(SERVICE_STOPPED, 0),
        Err(error) => {
            log(&format!("Service stopped: {error:#}"));
            let code = error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
            });
            report(
                SERVICE_STOPPED,
                code.map_or(ERROR_SERVICE_SPECIFIC_ERROR, |code| code as u32),
            );
        }
    }
}
fn transport_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("config").join("config.json"))
}
fn read_transport() -> Result<landesk_core::domain::identity::ServerIdentity> {
    crate::adapters::identity::read(&transport_path()?)
}
fn serve_transport() -> Result<()> {
    let config = read_transport()?;
    let _ = landesk_core::application::password::recover_password(
        &config,
        &landesk_storage::LocalSecrets::beside(&transport_path()?),
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
            .await
            .context("内置 SSH 端口被占用")?;
        let password_pipe = super::password_pipe::Listener::new()?;
        log(&format!("SSH ready on port {}", config.port));
        report(SERVICE_RUNNING, 0);
        let running = serde_json::to_vec(&config)?;
        let active_port = config.port;
        let transport = crate::adapters::ssh::run(
            listener,
            config,
            crate::adapters::ssh::PasswordPolicy::OnRestart,
            Arc::new(|| {
                Box::pin(async {
                    Ok(Box::new(super::pipe::connect().await?)
                        as Box<dyn crate::adapters::ssh::StreamIo>)
                })
            }),
            async {
                while !STOP.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        );
        tokio::select! {
            result = transport => result,
            result = password_pipe.serve(transport_path()?, running, active_port) => result,
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(3));
    result
}
fn worker(stop_name: &str) -> Result<()> {
    require_system_console()?;
    log(&format!(
        "Worker security context: {}",
        super::interactive::current_token_report()
            .unwrap_or_else(|e| serde_json::json!({"error":format!("{e:#}")}))
    ));
    log(&format!(
        "Worker console state: {}",
        super::interactive::report()
    ));
    ensure!(
        stop_name
            .strip_prefix(r"Global\LanDeskServer-")
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())),
        "工作进程事件名无效"
    );
    let stop = Handle::new(
        unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, wide(stop_name).as_ptr()) },
        "打开服务停止事件",
    )?;
    checked(
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) },
        "设置 DPI 感知",
    )?;
    let key = read_transport()?.backend_key;
    let shared = Arc::new(Shared::default());
    let native = Arc::new(WindowsControl::new(shared.clone())?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let state = AppState { transport_key: landesk_core::security::transport_key::TransportKey::new(key)?, native, shared: shared.clone(), capture: CaptureService::new(|| Box::new(WindowsCapture::default()))?, files: FileService::new(NoFiles) };
        let server = super::pipe::serve(state);
        tokio::pin!(server);
        loop {
            tokio::select! {
                result = &mut server => return result,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if unsafe { WaitForSingleObject(stop.0, 0) } == WAIT_OBJECT_0 {
                        shared.shutdown.store(true, Ordering::Release);
                        return tokio::time::timeout(Duration::from_secs(3), &mut server).await.context("桌面服务退出超时")?;
                    }
                }
            }
        }
    });
    shared.shutdown.store(true, Ordering::Release);
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}
pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let service_mode = args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "--service" | "--transport-service"));
    if !service_mode && let Err(error) = super::bootstrap::initialize_logging(&args) {
        if args.is_empty()
            || args.first().is_some_and(|arg| {
                matches!(
                    arg.as_str(),
                    "--settings"
                        | "--startup-ui"
                        | "--install-integrated"
                        | "--uninstall"
                        | "--prepare-uninstall"
                        | "--run-uninstall"
                        | "--export-logs"
                        | "--start-background"
                        | "--manage"
                        | "--managed-uninstall"
                )
            })
        {
            super::privilege::error_box(null_mut(), format!("无法创建日志：{error:#}"));
        }
        return Err(error);
    }
    let result = (|| -> Result<()> {
        match args.as_slice() {
            [mode] if mode == "--service" || mode == "--transport-service" => {
                TRANSPORT.store(mode == "--transport-service", Ordering::Relaxed);
                let mut name = wide(if TRANSPORT.load(Ordering::Relaxed) {
                    TRANSPORT_NAME
                } else {
                    NAME
                });
                let table = [
                    SERVICE_TABLE_ENTRYW {
                        lpServiceName: name.as_mut_ptr(),
                        lpServiceProc: Some(service_main),
                    },
                    SERVICE_TABLE_ENTRYW {
                        lpServiceName: null_mut(),
                        lpServiceProc: None,
                    },
                ];
                checked(
                    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) },
                    "启动服务调度器",
                )
            }
            [mode, stop] if mode == "--worker" => worker(stop),
            [mode] if mode == "--desktop-check" => {
                let config = read_transport()?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(super::pipe::health(&config.backend_key))
            }
            [mode] if mode == "--transport-info" => {
                let config = read_transport()?;
                println!(
                    "{}",
                    serde_json::json!({"port":config.port,"user":crate::adapters::ssh::USER,"fingerprint":crate::adapters::identity::fingerprint(&config)?})
                );
                Ok(())
            }
            [mode] if mode == "--start-background" => {
                super::management::start(&super::setup::Paths::new()?)
            }
            [mode, directory] if mode == "--manage" => {
                super::management::run(std::path::Path::new(directory))
            }
            [mode, directory, owner, display, parent] if mode == "--managed-uninstall" => {
                super::setup::managed_uninstall(
                    std::path::Path::new(directory),
                    owner.parse()?,
                    display == "1",
                    parent.parse()?,
                )
            }
            [mode, launcher] if mode == "--install-integrated" => {
                super::bootstrap::install(launcher.parse().context("安装启动进程参数无效")?)
            }
            [mode] if mode == "--prepare-uninstall" => super::bootstrap::prepare_uninstall(),
            [mode, parent] if mode == "--run-uninstall" => {
                super::setup::run_uninstall(parent.parse().context("卸载进程参数无效")?)
            }
            [mode] if mode == "--uninstall" => super::bootstrap::request_uninstall(),
            [mode] if mode == "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            [mode] if mode == "--status" => {
                println!("{}", super::setup::status()?);
                Ok(())
            }
            [mode] if mode == "--export-logs" => super::bootstrap::export(),
            [] => super::ui::run(false),
            [mode] if mode == "--settings" => super::ui::run(false),
            [mode] if mode == "--startup-ui" => super::ui::run(true),
            _ => {
                bail!("参数无效；双击 LanDeskServer.exe 安装或打开设置")
            }
        }
    })();
    if let Err(error) = &result {
        log(&format!("LanDeskServer failed: {error:#}"));
        if args
            .first()
            .is_some_and(|mode| matches!(mode.as_str(), "--start-background"))
        {
            super::privilege::error_box(null_mut(), format!("{error:#}"));
        }
    }
    result
}
