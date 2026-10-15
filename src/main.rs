#[cfg(target_os = "macos")]
mod macos {
    use landesk::{capture, file_worker, platform, server};
    use platform::macos::{capture as mac_capture, files, native};

    use anyhow::Result;
    use std::sync::{Arc, mpsc};

    pub fn run() -> Result<()> {
        native::initialize_app();
        let connection = platform::macos::connection::prepare()?;
        let password = connection.recover_password()?;
        let port = connection.port;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let (tx, rx) = mpsc::channel();
        let native = native::Native {
            tx,
            shared: Arc::new(platform::Shared::default()),
        };
        let state = server::AppState {
            shared: native.shared.clone(),
            native: Arc::new(native.clone()),
            transport_key: landesk::server_auth::TransportKey::new(connection.backend_key.clone())?,
            capture: capture::CaptureService::new(|| {
                Box::new(mac_capture::MacCaptureBackend::default())
            })?,
            files: file_worker::FileService::new(files::HomeFiles::open(std::path::Path::new(
                &objc2_foundation::NSHomeDirectory().to_string(),
            ))?),
        };
        let transport_listener =
            runtime.block_on(tokio::net::TcpListener::bind(("0.0.0.0", port)))?;
        let shutdown = native.shared.clone();
        let failure = native.shared.clone();
        let transport = runtime.spawn(async move {
            let result = landesk::transport::run(
                transport_listener,
                connection,
                platform::macos::connection::path(),
                Arc::new(move || {
                    let state = state.clone();
                    Box::pin(async move {
                        let (transport, desktop) = tokio::io::duplex(65536);
                        tokio::spawn(async move {
                            if let Err(error) = server::serve_stream(desktop, state).await {
                                landesk::diagnostics::log(&format!(
                                    "Desktop stream closed: {error:#}"
                                ));
                            }
                        });
                        Ok(Box::new(transport) as Box<dyn landesk::transport::StreamIo>)
                    })
                }),
                async move {
                    while !shutdown.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                },
            )
            .await;
            if result.is_err() {
                failure
                    .shutdown
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            result
        });
        let shutdown = native.shared.clone();
        let ui_result = native::run(native, rx, port, password);
        shutdown
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        ui_result?;
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), transport).await
        })???;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Windows 服务端请使用 LanDeskServer.exe")
}
