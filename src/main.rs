#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use anyhow::Result;
use landesk::{
    desktop, file_worker,
    platform::{CurrentPlatform, HostPlatform, filesystem},
    protocol, server, ssh_config,
};
use std::sync::{Arc, mpsc};

fn main() {
    if let Err(error) = run() {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
            let message: Vec<u16> = format!("{error:#}").encode_utf16().chain(Some(0)).collect();
            let title: Vec<u16> = "LanDesk".encode_utf16().chain(Some(0)).collect();
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
        #[cfg(not(target_os = "windows"))]
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    if ssh_config::run_admin_command()? {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Bind before displaying a successful status: collisions are explicit errors.
    let listener =
        runtime.block_on(tokio::net::TcpListener::bind(("127.0.0.1", protocol::PORT)))?;
    let (tx, rx) = mpsc::channel();
    let native = desktop::DesktopControl {
        tx,
        shared: Arc::new(desktop::Shared::default()),
    };
    let state = server::AppState {
        native: native.clone(),
        files: file_worker::FileService::new(filesystem::HomeFiles::open(
            &CurrentPlatform.home_directory()?,
        )?),
    };
    let handle = runtime.spawn(server::run(listener, state));
    let ui_result = CurrentPlatform.run_ui(native, rx);
    let server_result = runtime
        .block_on(async { tokio::time::timeout(std::time::Duration::from_secs(5), handle).await });
    ui_result?;
    match server_result {
        Ok(result) => result??,
        Err(_) => anyhow::bail!("连接未在规定时间内关闭"),
    }
    Ok(())
}
