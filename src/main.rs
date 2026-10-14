#[cfg(target_os = "macos")]
mod macos {
    use landesk::{capture, file_worker, platform, protocol, server};
    use platform::macos::{capture as mac_capture, files, native};

    use anyhow::Result;
    use std::sync::{Arc, mpsc};

    pub fn run() -> Result<()> {
        let mtm = native::initialize_app();
        let connection = platform::macos::connection::prepare(mtm)?;
        let port = connection.port;
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
            native: Arc::new(platform::authenticated::AuthenticatedDesktop::new(
                Arc::new(native.clone()),
                connection.backend_key.clone(),
            )),
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
                Arc::new(|| {
                    Box::pin(async {
                        Ok(Box::new(
                            tokio::net::TcpStream::connect(("127.0.0.1", protocol::PORT)).await?,
                        )
                            as Box<dyn landesk::transport::StreamIo>)
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
        let handle = runtime.spawn(server::run(listener, state));
        let shutdown = native.shared.clone();
        let ui_result = native::run(native, rx, port);
        shutdown
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        let server_result = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), handle).await
        });
        ui_result?;
        match server_result {
            Ok(result) => result??,
            Err(_) => anyhow::bail!("连接未在规定时间内关闭"),
        }
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
