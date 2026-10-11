#[cfg(not(target_os = "macos"))]
compile_error!("LanDesk 服务端目前仅支持 macOS，Windows 请使用浏览器连接。");

use landesk::{capture, file_worker, platform, protocol, server};
use platform::macos::{capture as mac_capture, files, native};

use anyhow::Result;
use std::sync::{Arc, mpsc};

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Bind before displaying a successful status: collisions are explicit errors.
    let listener =
        runtime.block_on(tokio::net::TcpListener::bind(("127.0.0.1", protocol::PORT)))?;
    let (tx, rx) = mpsc::channel();
    let native = native::Native {
        tx,
        shared: Arc::new(platform::Shared::default()),
    };
    let state = server::AppState {
        shared: native.shared.clone(),
        native: Arc::new(native.clone()),
        capture: capture::CaptureService::new(|| {
            Box::new(mac_capture::MacCaptureBackend::default())
        })?,
        files: file_worker::FileService::new(files::HomeFiles::open(std::path::Path::new(
            &objc2_foundation::NSHomeDirectory().to_string(),
        ))?),
    };
    let handle = runtime.spawn(server::run(listener, state));
    let ui_result = native::run(native, rx);
    let server_result = runtime
        .block_on(async { tokio::time::timeout(std::time::Duration::from_secs(5), handle).await });
    ui_result?;
    match server_result {
        Ok(result) => result??,
        Err(_) => anyhow::bail!("连接未在规定时间内关闭"),
    }
    Ok(())
}
