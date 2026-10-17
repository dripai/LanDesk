//! SCM calls receive UTF-16 data directly, never command-line arguments to sc.exe.
use super::desktop::checked;
use super::desktop::wide;
use super::service::ServiceHandle;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::path::Path;
use std::ptr::null;
use std::ptr::null_mut;
use std::time::Duration;
use std::time::Instant;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::System::Services::*;

pub(super) const SERVER: &str = "LanDeskServer";
pub(super) const TRANSPORT: &str = "LanDeskTransport";
pub(super) fn manager() -> Result<ServiceHandle> {
    ServiceHandle::new(unsafe {
        OpenSCManagerW(
            null(),
            null(),
            SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
        )
    })
}
pub(super) fn open(name: &str) -> Result<Option<ServiceHandle>> {
    open_access(
        name,
        SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP | DELETE,
    )
}
pub(super) fn open_access(name: &str, access: u32) -> Result<Option<ServiceHandle>> {
    let manager =
        ServiceHandle::new(unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) })?;
    let handle = unsafe { OpenServiceW(manager.0, wide(name).as_ptr(), access) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_SERVICE_DOES_NOT_EXIST {
            return Ok(None);
        }
        return Err(std::io::Error::from_raw_os_error(error as i32))
            .context(format!("打开服务 {name}"));
    }
    Ok(Some(ServiceHandle::new(handle)?))
}
pub(super) unsafe fn string(pointer: *const u16) -> Result<String> {
    ensure!(!pointer.is_null(), "Windows 服务返回空字符串指针");
    let mut len = 0;
    while unsafe { *pointer.add(len) } != 0 {
        len += 1;
        ensure!(len < 32768, "Windows 服务配置字符串过长");
    }
    Ok(String::from_utf16(unsafe {
        std::slice::from_raw_parts(pointer, len)
    })?)
}
#[derive(serde::Serialize)]
pub(super) struct Configuration {
    pub binary: String,
    pub account: String,
    pub start: u32,
    pub dependencies: Vec<String>,
}
pub(super) fn configuration(service: &ServiceHandle) -> Result<Configuration> {
    let mut needed = 0;
    unsafe {
        QueryServiceConfigW(service.0, null_mut(), 0, &mut needed);
    }
    ensure!(
        unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER && (1..=65536).contains(&needed),
        "读取服务配置长度失败"
    );
    let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    checked(
        unsafe { QueryServiceConfigW(service.0, data.as_mut_ptr().cast(), needed, &mut needed) },
        "读取服务配置",
    )?;
    let value = unsafe { &*data.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let mut dependencies = Vec::new();
    if !value.lpDependencies.is_null() {
        let mut next = value.lpDependencies;
        while unsafe { *next } != 0 {
            let text = unsafe { string(next)? };
            next = unsafe { next.add(text.encode_utf16().count() + 1) };
            dependencies.push(text);
        }
    }
    Ok(Configuration {
        binary: unsafe { string(value.lpBinaryPathName)? },
        account: unsafe { string(value.lpServiceStartName)? },
        start: value.dwStartType,
        dependencies,
    })
}
pub(super) fn command(executable: &Path, transport: bool) -> String {
    format!(
        "\"{}\" {}",
        executable.display(),
        if transport {
            "--transport-service"
        } else {
            "--service"
        }
    )
}
pub(super) fn assert_owned(executable: &Path) -> Result<()> {
    for (name, transport) in [(SERVER, false), (TRANSPORT, true)] {
        if let Some(handle) = open(name)? {
            let config = configuration(&handle)?;
            let expected = if transport {
                r"NT AUTHORITY\LocalService"
            } else {
                "LocalSystem"
            };
            ensure!(
                config.binary == command(executable, transport)
                    && config.account.eq_ignore_ascii_case(expected),
                "同名服务 {name} 的路径或账号不属于 LanDesk，未修改"
            );
        }
    }
    Ok(())
}
pub(super) fn create(
    executable: &Path,
    name: &str,
    transport: bool,
    dependency: Option<&str>,
) -> Result<ServiceHandle> {
    let manager = manager()?;
    let binary = wide(&command(executable, transport));
    let account = wide(r"NT AUTHORITY\LocalService");
    let dependencies =
        dependency.map(|name| wide(name).into_iter().chain(Some(0)).collect::<Vec<_>>());
    ServiceHandle::new(unsafe {
        CreateServiceW(
            manager.0,
            wide(name).as_ptr(),
            wide(if transport {
                "LanDesk Encrypted Transport"
            } else {
                "LanDesk Remote Desktop"
            })
            .as_ptr(),
            SERVICE_QUERY_CONFIG
                | SERVICE_QUERY_STATUS
                | SERVICE_CHANGE_CONFIG
                | SERVICE_START
                | SERVICE_STOP
                | DELETE,
            SERVICE_WIN32_OWN_PROCESS,
            if transport {
                SERVICE_DEMAND_START
            } else {
                SERVICE_AUTO_START
            },
            SERVICE_ERROR_NORMAL,
            binary.as_ptr(),
            null(),
            null_mut(),
            dependencies.as_ref().map_or(null(), |v| v.as_ptr()),
            if transport { account.as_ptr() } else { null() },
            wide("").as_ptr(),
        )
    })
    .with_context(|| format!("CreateServiceW 注册 {name}"))
}
pub(super) fn configure(service: &ServiceHandle, transport: bool) -> Result<()> {
    let mut description = wide(if transport {
        "LanDesk embedded encrypted desktop transport."
    } else {
        "LanDesk console desktop capture and input."
    });
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: description.as_mut_ptr(),
    };
    checked(
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_DESCRIPTION,
                (&description as *const SERVICE_DESCRIPTIONW).cast(),
            )
        },
        "设置服务说明",
    )?;
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 15000,
        },
        SC_ACTION {
            Type: SC_ACTION_NONE,
            Delay: 0,
        },
    ];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86400,
        lpRebootMsg: null_mut(),
        lpCommand: null_mut(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    checked(
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS,
                (&failure as *const SERVICE_FAILURE_ACTIONSW).cast(),
            )
        },
        "设置服务异常重启",
    )?;
    let flag = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: 1,
    };
    checked(
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                (&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast(),
            )
        },
        "设置服务失败处理",
    )
}
pub(super) fn status(service: &ServiceHandle) -> Result<SERVICE_STATUS_PROCESS> {
    let mut state = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    checked(
        unsafe {
            QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut state as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of_val(&state) as u32,
                &mut needed,
            )
        },
        "读取服务状态",
    )?;
    Ok(state)
}
fn wait(service: &ServiceHandle, desired: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = status(service)?;
        if state.dwCurrentState == desired {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "服务状态等待超时: state={} exit={} service_exit={}",
            state.dwCurrentState,
            state.dwWin32ExitCode,
            state.dwServiceSpecificExitCode
        );
        if desired == SERVICE_RUNNING {
            ensure!(
                state.dwCurrentState != SERVICE_STOPPED,
                "服务启动后退出: exit={} service_exit={}",
                state.dwWin32ExitCode,
                state.dwServiceSpecificExitCode
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub(super) fn start(name: &str) -> Result<()> {
    let service = open_access(name, SERVICE_START | SERVICE_QUERY_STATUS)?
        .with_context(|| format!("服务 {name} 未安装"))?;
    let state = status(&service)?.dwCurrentState;
    if state == SERVICE_RUNNING {
        return Ok(());
    }
    if state != SERVICE_START_PENDING && unsafe { StartServiceW(service.0, 0, null()) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(error).with_context(|| format!("启动服务 {name} 失败"));
        }
    }
    wait(&service, SERVICE_RUNNING).with_context(|| format!("等待服务 {name} 启动失败"))
}
pub(super) fn stop(name: &str) -> Result<()> {
    if let Some(service) = open(name)? {
        let state = status(&service)?.dwCurrentState;
        if state == SERVICE_STOPPED {
            return Ok(());
        }
        if state != SERVICE_STOP_PENDING {
            checked(
                unsafe {
                    ControlService(
                        service.0,
                        SERVICE_CONTROL_STOP,
                        &mut SERVICE_STATUS::default(),
                    )
                },
                "停止服务",
            )?;
        }
        wait(&service, SERVICE_STOPPED)?;
    }
    Ok(())
}
pub(super) fn delete(name: &str) -> Result<()> {
    stop(name)?;
    if let Some(service) = open(name)? {
        checked(unsafe { DeleteService(service.0) }, "删除服务")?;
    }
    // DeleteService only marks deletion. Drop every handle before waiting for
    // removal, otherwise an immediate CreateServiceW can fail with error 1072.
    let manager = manager()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let raw = unsafe { OpenServiceW(manager.0, wide(name).as_ptr(), SERVICE_QUERY_STATUS) };
        if raw.is_null() {
            let error = unsafe { GetLastError() };
            if error == ERROR_SERVICE_DOES_NOT_EXIST {
                return Ok(());
            }
            ensure!(
                error == ERROR_SERVICE_MARKED_FOR_DELETE,
                "等待删除服务 {name} 失败: {}",
                std::io::Error::from_raw_os_error(error as i32)
            );
        } else {
            drop(ServiceHandle::new(raw)?);
        }
        ensure!(
            Instant::now() < deadline,
            "服务 {name} 尚未释放，删除等待超时；请关闭 Windows 服务管理窗口后重新运行安装程序"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub(super) fn report() -> Result<serde_json::Value> {
    let mut output = Vec::new();
    for name in [SERVER, TRANSPORT] {
        let value = match open_access(name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)? {
            Some(service) => {
                let state = status(&service)?;
                serde_json::json!({"name":name,"configuration":configuration(&service)?,"state":state.dwCurrentState,"exit_code":state.dwWin32ExitCode,"service_exit_code":state.dwServiceSpecificExitCode,"process_id":state.dwProcessId})
            }
            None => serde_json::json!({"name":name,"installed":false}),
        };
        output.push(value);
    }
    Ok(output.into())
}
pub(super) fn failure_summary() -> String {
    [TRANSPORT, SERVER]
        .into_iter()
        .map(|name| {
            let result = (|| -> Result<String> {
                let service = open_access(name, SERVICE_QUERY_STATUS)?
                    .with_context(|| format!("{name} 未安装"))?;
                let state = status(&service)?;
                Ok(format!(
                    "{name}: 状态={}，系统错误={}，服务错误={}",
                    state.dwCurrentState, state.dwWin32ExitCode, state.dwServiceSpecificExitCode
                ))
            })();
            result.unwrap_or_else(|error| format!("{name}: {error:#}"))
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires Windows administrator; creates one stopped temporary service, never starts it"]
    fn native_setup_service_deletion_waits_for_handles_and_allows_immediate_recreate() -> Result<()>
    {
        let name = format!("LanDeskDeletionTest{}", std::process::id());
        ensure!(
            open(&name)?.is_none(),
            "Test service exists; nothing changed"
        );
        let path = Path::new(r"C:\Program Files\LanDesk Native Test\LanDeskServer.exe");
        let handle = create(path, &name, true, None)?;
        // This independent holder simulates a service-settings window releasing
        // its handle after DeleteService. Handles are valid across OS threads.
        let raw = handle.0 as usize;
        std::mem::forget(handle);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(ServiceHandle(raw as _));
        });
        let result = delete(&name);
        release
            .join()
            .map_err(|_| anyhow::anyhow!("Test handle-release thread failed"))?;
        result?;
        ensure!(open(&name)?.is_none(), "Marked service was not removed");
        drop(create(path, &name, true, None)?);
        delete(&name)
    }
    #[test]
    #[ignore = "Requires Windows administrator; creates then deletes two temporary SCM entries, never starts them"]
    fn native_setup_service_registration_preserves_unicode_paths_and_accounts() -> Result<()> {
        let suffix = format!("{}", std::process::id());
        let transport = format!("LanDeskTestTransport{suffix}");
        let server = format!("LanDeskTestServer{suffix}");
        let path = Path::new(r"C:\Program Files\LanDesk 中文 测试\LanDeskServer.exe");
        let handle = create(path, &transport, true, None)?;
        let result = (|| {
            configure(&handle, true)?;
            let config = configuration(&handle)?;
            ensure!(
                config.binary == command(path, true)
                    && config
                        .account
                        .eq_ignore_ascii_case(r"NT AUTHORITY\LocalService")
                    && config.start == SERVICE_DEMAND_START
                    && config.dependencies.is_empty(),
                "通信服务配置不匹配"
            );
            let main = create(path, &server, false, Some(&transport))?;
            let result = (|| {
                configure(&main, false)?;
                let config = configuration(&main)?;
                ensure!(
                    config.binary == command(path, false)
                        && config.account == "LocalSystem"
                        && config.start == SERVICE_AUTO_START
                        && config.dependencies == [transport.clone()],
                    "桌面服务配置不匹配"
                );
                Ok(())
            })();
            checked(unsafe { DeleteService(main.0) }, "清理测试桌面服务")?;
            result
        })();
        checked(unsafe { DeleteService(handle.0) }, "清理测试通信服务")?;
        result
    }
}
