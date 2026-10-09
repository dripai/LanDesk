#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use anyhow::Result;
use landesk::{
    access, desktop, file_worker,
    platform::{CurrentPlatform, HostPlatform, filesystem},
    server, transport,
};
use std::sync::{Arc, mpsc};

fn main() {
    if let Err(error) = run() {
        #[cfg(target_os = "windows")]
        if !landesk::platform::windows::desktop::background() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    MB_ICONERROR, MB_OK, MessageBoxW,
                };
                let message: Vec<u16> =
                    format!("{error:#}").encode_utf16().chain(Some(0)).collect();
                let title: Vec<u16> = "LanDeskServer".encode_utf16().chain(Some(0)).collect();
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        #[cfg(not(target_os = "windows"))]
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    #[cfg(target_os = "windows")]
    if landesk::platform::windows::desktop::startup()? {
        return Ok(());
    }
    let path = access::path()?;
    let settings = access::Settings::load(&path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (tx, rx) = mpsc::channel();
    let native = desktop::DesktopControl {
        tx,
        shared: Arc::new(desktop::Shared::default()),
    };
    let (control, listener, transport_task) = {
        let _runtime = runtime.enter();
        transport::start(settings, path, native.shared.clone())
    };
    native
        .shared
        .connection
        .set(control)
        .ok()
        .expect("initialize connection control once");
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
    runtime.block_on(transport_task)??;
    match server_result {
        Ok(result) => result??,
        Err(_) => anyhow::bail!("连接未在规定时间内关闭"),
    }
    Ok(())
}
