//! The downloadable EXE contains its installer and the original signed driver.
use super::desktop::wide;
use super::ui;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::path::Path;
use std::path::PathBuf;
use std::ptr::null;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Services::*;

pub fn machine_logs() -> Result<PathBuf> {
    Ok(landesk_storage::application_data("server")?.join("logs"))
}
pub fn user_logs() -> Result<PathBuf> {
    machine_logs()
}
pub fn diagnostic_exports() -> Result<PathBuf> {
    Ok(landesk_storage::application_data("server")?.join("diagnostics"))
}
pub fn log_locations() -> String {
    format!(
        "运行/安装日志：{}\n界面日志：{}",
        machine_logs()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| e.to_string()),
        user_logs()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| e.to_string())
    )
}
pub fn initialize_logging(args: &[String]) -> Result<()> {
    let mode = args.first().map(String::as_str).unwrap_or("ui");
    let filename = match mode {
        "--service" => "server.log",
        "--transport-service" => "transport.log",
        "--worker" => "worker.log",
        _ => "ui.log",
    };
    let directory = if filename == "ui.log" {
        let directory = user_logs()?;
        std::fs::create_dir_all(&directory)?;
        directory
    } else {
        machine_logs()?
    };
    let path = directory.join(filename);
    crate::diagnostics::initialize(path.clone())
        .with_context(|| format!("无法打开日志 {}", path.display()))?;
    crate::diagnostics::log(&format!(
        "Process started; mode={mode}; arch={}",
        std::env::consts::ARCH
    ));
    std::panic::set_hook(Box::new(|panic| {
        // A panic payload can contain application data. Record code location, not that payload.
        crate::diagnostics::log(&format!(
            "PANIC at {:?}; backtrace={}",
            panic.location(),
            std::backtrace::Backtrace::force_capture()
        ));
    }));
    Ok(())
}

fn installed_executable() -> Result<Option<PathBuf>> {
    let manager = super::service::ServiceHandle::new(unsafe {
        OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT)
    })?;
    let service = unsafe {
        OpenServiceW(
            manager.0,
            wide("LanDeskServer").as_ptr(),
            SERVICE_QUERY_CONFIG,
        )
    };
    if service.is_null() {
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_SERVICE_DOES_NOT_EXIST,
            "读取安装状态失败: {}",
            std::io::Error::from_raw_os_error(error as i32)
        );
        return Ok(None);
    }
    let service = super::service::ServiceHandle::new(service)?;
    Ok(Some(ui::configuration(&service)?.1))
}

pub fn install(launcher: u32) -> Result<()> {
    let result = super::setup::install(launcher);
    if let Err(error) = &result {
        ui::error_box(null_mut(), format!("安装失败：{error:#}"));
    }
    result
}
pub fn request_uninstall() -> Result<()> {
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
    let result = ui::elevated(
        null_mut(),
        &std::env::current_exe()?,
        "--prepare-uninstall",
        false,
    );
    if let Err(error) = &result {
        ui::error_box(null_mut(), format!("无法启动卸载：{error:#}"));
    }
    result
}
pub fn prepare_uninstall() -> Result<()> {
    let result = super::setup::prepare_uninstall();
    if let Err(error) = &result {
        ui::error_box(null_mut(), format!("卸载启动失败：{error:#}"));
    }
    result
}
pub fn export() -> Result<()> {
    let result = super::setup::export();
    if let Err(error) = &result {
        ui::error_box(null_mut(), format!("日志导出失败：{error:#}"));
    }
    result
}
pub fn install_if_needed() -> Result<bool> {
    let executable = std::env::current_exe()?;
    if let Some(installed) = installed_executable()?
        && super::setup_support::read(super::setup_support::UNINSTALL, "DisplayVersion")?
            .map(|value| super::setup_support::decode(&value))
            .transpose()?
            .as_deref()
            == Some(env!("CARGO_PKG_VERSION"))
        && installed.is_file()
        && (std::fs::canonicalize(&installed)? == std::fs::canonicalize(&executable)?
            || std::fs::read(&installed)? == std::fs::read(&executable)?)
    {
        return Ok(false);
    }
    ui::elevated(
        null_mut(),
        &std::env::current_exe()?,
        &format!("--install-integrated {}", std::process::id()),
        true,
    )?;
    ensure!(
        installed_executable()?.is_some(),
        "安装进程结束，但后台服务尚未注册"
    );
    Ok(true)
}
pub fn open_folder(path: &Path) -> Result<()> {
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide(&path.to_string_lossy()).as_ptr(),
            null(),
            null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    ensure!(
        result as isize > 32,
        "无法打开日志文件夹 ({})",
        result as isize
    );
    Ok(())
}
