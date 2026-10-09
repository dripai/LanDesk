//! LocalSystem broker. Authenticated pipes are duplicated into desktop workers.
use super::sys::*;
use anyhow::{Context, Result, ensure};
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    Storage::FileSystem::*,
    System::{
        IO::*, JobObjects::*, Pipes::*, RemoteDesktop::ProcessIdToSessionId, Services::*,
        Threading::*,
    },
};

pub const NAME: &str = "LanDeskDesktop";
pub const PIPE: &str = r"\\.\pipe\LanDeskDesktop-v1";
pub const PIPE_ACCESS: u32 = 0x12019b; // Read/write without FILE_CREATE_PIPE_INSTANCE.
static STOP_EVENT: std::sync::OnceLock<OwnedHandle> = std::sync::OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(ptr::null_mut());
pub struct Service(pub SC_HANDLE);
impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
pub fn manager(access: u32) -> Result<Service> {
    let h = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    ensure!(
        !h.is_null(),
        "打开 Windows 服务管理器失败：{}",
        std::io::Error::last_os_error()
    );
    Ok(Service(h))
}
pub fn open(access: u32) -> Result<Service> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let h = unsafe { OpenServiceW(manager.0, wide(NAME).as_ptr(), access) };
    ensure!(
        !h.is_null(),
        "LanDesk 后台服务不可用：{}",
        std::io::Error::last_os_error()
    );
    Ok(Service(h))
}
pub fn process_id() -> Result<u32> {
    let service = open(SERVICE_QUERY_STATUS)?;
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    unsafe {
        ok(
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut needed,
            ),
            "查询后台服务失败",
        )?;
    }
    ensure!(
        status.dwCurrentState == SERVICE_RUNNING,
        "LanDesk 后台服务未运行，请重新启动 LanDeskServer"
    );
    Ok(status.dwProcessId)
}
fn report(state: u32, code: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_START_PENDING {
            10000
        } else {
            0
        },
    };
    unsafe {
        SetServiceStatus(STATUS.load(Ordering::Acquire), &status);
    }
}
unsafe extern "system" fn control(
    code: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if code == SERVICE_CONTROL_STOP || code == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::Release);
        report(SERVICE_STOP_PENDING, 0);
        if let Some(event) = STOP_EVENT.get() {
            unsafe {
                SetEvent(event.as_raw_handle());
            }
        }
    }
    0
}
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(wide(NAME).as_ptr(), Some(control), ptr::null_mut())
    };
    if handle.is_null() {
        return;
    }
    STATUS.store(handle, Ordering::Release);
    report(SERVICE_START_PENDING, 0);
    let result = serve();
    report(
        SERVICE_STOPPED,
        if result.is_ok() {
            0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
    );
}
pub fn run() -> Result<()> {
    ensure!(current_is_system()?, "后台服务须由 Windows 服务管理器启动");
    let mut name = wide(NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    unsafe {
        ok(
            StartServiceCtrlDispatcherW(table.as_ptr()),
            "启动后台服务失败",
        )
    }
}
fn pipe(first: bool) -> Result<OwnedHandle> {
    let descriptor = Descriptor::new("D:P(A;;GA;;;SY)(A;;0x12019b;;;AU)")?;
    unsafe {
        owned(
            CreateNamedPipeW(
                wide(PIPE).as_ptr(),
                PIPE_ACCESS_DUPLEX
                    | FILE_FLAG_OVERLAPPED
                    | if first {
                        FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        0
                    },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1024 * 1024,
                128 * 1024,
                0,
                &descriptor.attributes(),
            ),
            "创建桌面通道失败",
        )
    }
}
fn serve() -> Result<()> {
    let stop_event = unsafe {
        owned(
            CreateEventW(ptr::null(), 1, 0, ptr::null()),
            "创建服务停止事件失败",
        )?
    };
    STOP_EVENT
        .set(stop_event)
        .map_err(|_| anyhow::anyhow!("后台服务重复启动"))?;
    enable_privilege(SE_CREATE_GLOBAL_NAME)?;
    enable_privilege(SE_TCB_NAME)?;
    enable_privilege(SE_ASSIGNPRIMARYTOKEN_NAME)?;
    enable_privilege(SE_INCREASE_QUOTA_NAME)?;
    let job = unsafe {
        owned(
            CreateJobObjectW(ptr::null(), ptr::null()),
            "创建桌面进程组失败",
        )?
    };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        ok(
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            ),
            "配置桌面进程组失败",
        )?;
    }
    let executable = std::env::current_exe()?.canonicalize()?;
    let mut listener = pipe(true)?;
    report(SERVICE_RUNNING, 0);
    while !STOP.load(Ordering::Acquire) {
        unsafe {
            let event = owned(
                CreateEventW(ptr::null(), 1, 0, ptr::null()),
                "创建桌面连接事件失败",
            )?;
            let mut operation = OVERLAPPED {
                hEvent: event.as_raw_handle(),
                ..Default::default()
            };
            let connected = ConnectNamedPipe(listener.as_raw_handle(), &mut operation);
            if connected == 0 {
                match GetLastError() {
                    ERROR_PIPE_CONNECTED => {}
                    ERROR_IO_PENDING => {
                        let events = [
                            event.as_raw_handle(),
                            STOP_EVENT.get().unwrap().as_raw_handle(),
                        ];
                        let waited = WaitForMultipleObjects(2, events.as_ptr(), 0, INFINITE);
                        if waited != WAIT_OBJECT_0 {
                            CancelIoEx(listener.as_raw_handle(), &operation);
                            let mut transferred = 0;
                            GetOverlappedResult(
                                listener.as_raw_handle(),
                                &operation,
                                &mut transferred,
                                1,
                            );
                            if waited == WAIT_OBJECT_0 + 1 {
                                break;
                            }
                            anyhow::bail!("等待服务事件失败");
                        }
                        let mut bytes = 0;
                        ok(
                            GetOverlappedResult(
                                listener.as_raw_handle(),
                                &operation,
                                &mut bytes,
                                0,
                            ),
                            "等待桌面连接失败",
                        )?;
                    }
                    _ => return Err(std::io::Error::last_os_error()).context("等待桌面连接失败"),
                }
            }
        }
        if STOP.load(Ordering::Acquire) {
            break;
        }
        let next = pipe(false)?;
        // Validate the executable before handing the pipe to a SYSTEM desktop worker.
        let result = (|| -> Result<(OwnedHandle, super::bootstrap::Mapping)> {
            let mut pid = 0;
            unsafe {
                ok(
                    GetNamedPipeClientProcessId(listener.as_raw_handle(), &mut pid),
                    "查询桌面客户端失败",
                )?;
            }
            let process = unsafe {
                owned(
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid),
                    "打开桌面客户端失败",
                )?
            };
            ensure!(
                image_path(process.as_raw_handle())?.canonicalize()? == executable,
                "桌面客户端与已安装服务版本不一致，请重新启动 LanDeskServer"
            );
            let mut session = 0;
            unsafe {
                ok(
                    ProcessIdToSessionId(pid, &mut session),
                    "查询 Windows 会话失败",
                )?;
            }
            ensure!(session != 0, "桌面客户端必须运行在交互会话中");
            spawn_worker(listener.as_raw_handle(), session, job.as_raw_handle())
        })();
        let accepted = std::mem::replace(&mut listener, next);
        match result {
            Ok((child, mapping)) => {
                std::thread::spawn(move || unsafe {
                    let _mapping = mapping;
                    WaitForSingleObject(child.as_raw_handle(), INFINITE);
                });
                drop(accepted);
            }
            Err(error) => {
                let _ = report_client_error(accepted, format!("{error:#}"));
            }
        }
    }
    // Closing the job terminates all workers, including those on a locked desktop.
    Ok(())
}
fn spawn_worker(
    pipe: HANDLE,
    session: u32,
    job: HANDLE,
) -> Result<(OwnedHandle, super::bootstrap::Mapping)> {
    unsafe {
        let token = token(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY)?;
        let mut primary = ptr::null_mut();
        ok(
            DuplicateTokenEx(
                token.as_raw_handle(),
                TOKEN_ALL_ACCESS,
                ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary,
            ),
            "复制桌面进程令牌失败",
        )?;
        let primary = owned(primary, "桌面进程令牌")?;
        ok(
            SetTokenInformation(
                primary.as_raw_handle(),
                TokenSessionId,
                (&session as *const u32).cast(),
                size_of::<u32>() as u32,
            ),
            "设置桌面进程会话失败",
        )?;
        let (name, mapping) = super::bootstrap::Mapping::create()?;
        let executable = std::env::current_exe()?;
        let mut command = wide(format!(
            "\"{}\" --desktop-worker {}",
            executable.display(),
            name
        ));
        let mut desktop = wide("winsta0\\default");
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop.as_mut_ptr(),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            primary.as_raw_handle(),
            wide(&executable).as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_NO_WINDOW | CREATE_SUSPENDED,
            ptr::null(),
            ptr::null(),
            &startup,
            &mut info,
        );
        let result = ok(created, "启动桌面采集进程失败");
        result?;
        let child = owned(info.hProcess, "桌面进程")?;
        let thread = owned(info.hThread, "桌面线程")?;
        if AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
            TerminateProcess(child.as_raw_handle(), 1);
            anyhow::bail!("加入桌面进程组失败：{}", std::io::Error::last_os_error());
        }
        if let Err(error) = mapping.transfer(pipe, child.as_raw_handle()) {
            TerminateProcess(child.as_raw_handle(), 1);
            return Err(error);
        }
        if ResumeThread(thread.as_raw_handle()) == u32::MAX {
            TerminateProcess(child.as_raw_handle(), 1);
            anyhow::bail!("启动桌面线程失败");
        }
        Ok((child, mapping))
    }
}

fn report_client_error(pipe: OwnedHandle, message: String) -> Result<()> {
    use std::os::windows::io::IntoRawHandle;
    use tokio::io::AsyncWriteExt;
    let mut bytes = Vec::new();
    super::wire::event(&mut bytes, super::wire::Event::Error { message })?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut pipe = unsafe {
                tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
                    pipe.into_raw_handle(),
                )?
            };
            tokio::time::timeout(std::time::Duration::from_secs(1), pipe.write_all(&bytes))
                .await??;
            Ok(())
        })
}
