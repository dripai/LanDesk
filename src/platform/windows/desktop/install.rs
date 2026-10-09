use super::{
    service::{self, Service},
    sys::*,
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    os::windows::{fs::MetadataExt, io::AsRawHandle},
    path::{Path, PathBuf},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::{DELETE, FILE_ATTRIBUTE_REPARSE_POINT},
    System::{Services::*, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
};
fn destination() -> Result<PathBuf> {
    let bytes = std::fs::read(std::env::current_exe()?)?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    Ok(install_root()?.join(hash).join("LanDeskServer.exe"))
}
fn installed_running(path: &Path) -> bool {
    (|| -> Result<bool> {
        service::process_id()?;
        let service = service::open(SERVICE_QUERY_CONFIG)?;
        let mut needed = 0;
        unsafe {
            QueryServiceConfigW(service.0, ptr::null_mut(), 0, &mut needed);
            ensure!(
                needed >= size_of::<QUERY_SERVICE_CONFIGW>() as u32,
                "无法读取后台服务配置"
            );
            let mut bytes = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
            let config = bytes.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            ok(
                QueryServiceConfigW(service.0, config, needed, &mut needed),
                "读取后台服务配置失败",
            )?;
            let binary = (*config).lpBinaryPathName;
            ensure!(!binary.is_null(), "后台服务程序路径为空");
            let mut length = 0;
            while *binary.add(length) != 0 {
                length += 1;
            }
            let expected = wide(format!("\"{}\" --desktop-service", path.display()));
            Ok(std::slice::from_raw_parts(binary, length) == &expected[..expected.len() - 1])
        }
    })()
    .unwrap_or(false)
}
/// Returns true when this launcher handed control to the protected installed copy.
pub fn launch() -> Result<bool> {
    let target = destination()?;
    let current = std::env::current_exe()?.canonicalize()?;
    if !installed_running(&target) {
        elevate("--install-desktop-service")?;
        ensure!(installed_running(&target), "LanDesk 后台服务安装后未能运行");
    }
    if current != target.canonicalize()? {
        std::process::Command::new(target)
            .spawn()
            .context("启动已安装的 LanDeskServer 失败")?;
        return Ok(true);
    }
    Ok(false)
}
fn elevate(argument: &str) -> Result<()> {
    unsafe {
        let verb = wide("runas");
        let path = wide(std::env::current_exe()?);
        let args = wide(argument);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: verb.as_ptr(),
            lpFile: path.as_ptr(),
            lpParameters: args.as_ptr(),
            nShow: SW_HIDE,
            ..Default::default()
        };
        ok(
            ShellExecuteExW(&mut info),
            "安装桌面服务需要 Windows 管理员授权",
        )?;
        let process = owned(info.hProcess, "安装进程")?;
        ensure!(
            WaitForSingleObject(process.as_raw_handle(), INFINITE) == WAIT_OBJECT_0,
            "等待安装进程失败"
        );
        let mut code = 0;
        ok(
            GetExitCodeProcess(process.as_raw_handle(), &mut code),
            "读取安装结果失败",
        )?;
        ensure!(code == 0, "后台服务安装失败（退出码 {code}）");
        Ok(())
    }
}
fn directory(path: &Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "服务安装路径不能是链接：{}",
        path.display()
    );
    Ok(())
}
pub fn install() -> Result<()> {
    let target = destination()?;
    let root = install_root()?;
    directory(&root)?;
    directory(target.parent().unwrap())?;
    // Program Files supplies the OS-managed write protection; no user folder ACLs are changed.
    let bytes = std::fs::read(std::env::current_exe()?)?;
    if target.exists() {
        ensure!(
            std::fs::read(&target)? == bytes,
            "已安装的服务文件与版本校验不一致"
        );
    } else {
        let mut temp = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
        use std::io::Write;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&target)
            .context("保存桌面服务失败")?;
    }
    let manager = service::manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    let name = wide(service::NAME);
    let binary = wide(format!("\"{}\" --desktop-service", target.display()));
    let service = unsafe {
        let existing = OpenServiceW(
            manager.0,
            name.as_ptr(),
            SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS,
        );
        if existing.is_null() {
            ensure!(
                GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST,
                "查询已有服务失败：{}",
                std::io::Error::last_os_error()
            );
            let handle = CreateServiceW(
                manager.0,
                name.as_ptr(),
                wide("LanDeskServer 桌面服务").as_ptr(),
                SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_CHANGE_CONFIG,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                binary.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
            );
            ensure!(
                !handle.is_null(),
                "创建桌面服务失败：{}",
                std::io::Error::last_os_error()
            );
            Service(handle)
        } else {
            let service = Service(existing);
            stop(&service)?;
            ok(
                ChangeServiceConfigW(
                    service.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    wide("LocalSystem").as_ptr(),
                    ptr::null(),
                    ptr::null(),
                ),
                "更新桌面服务失败",
            )?;
            service
        }
    };
    unsafe {
        ok(StartServiceW(service.0, 0, ptr::null()), "启动桌面服务失败")?;
    }
    wait(&service, SERVICE_RUNNING)
}
fn state(service: &Service) -> Result<u32> {
    let mut status = SERVICE_STATUS::default();
    unsafe {
        ok(
            QueryServiceStatus(service.0, &mut status),
            "查询服务状态失败",
        )?;
    }
    Ok(status.dwCurrentState)
}
fn wait(service: &Service, expected: u32) -> Result<()> {
    let start = Instant::now();
    while state(service)? != expected {
        ensure!(
            start.elapsed() < Duration::from_secs(20),
            "等待后台服务状态变化超时"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
fn stop(service: &Service) -> Result<()> {
    if state(service)? != SERVICE_STOPPED {
        let mut status = SERVICE_STATUS::default();
        unsafe {
            ok(
                ControlService(service.0, SERVICE_CONTROL_STOP, &mut status),
                "停止旧桌面服务失败",
            )?;
        }
        wait(service, SERVICE_STOPPED)?;
    }
    Ok(())
}
pub fn uninstall() -> Result<()> {
    let service = service::open(SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE)?;
    stop(&service)?;
    unsafe { ok(DeleteService(service.0), "卸载桌面服务失败") }
}
