use crate::adapters::web as server;
use crate::platform;
use landesk_core::application::capture;
use landesk_core::application::file_worker;
use platform::macos::capture as mac_capture;
use platform::macos::files;
use platform::macos::native;

use anyhow::Result;
use std::sync::Arc;
use std::sync::mpsc;

pub fn run() -> Result<()> {
    let connection = platform::macos::connection::prepare()?;
    let password = landesk_core::application::password::recover_password(
        &connection,
        &landesk_storage::LocalSecrets::beside(&platform::macos::connection::path()?),
    );
    let port = connection.port;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (tx, rx) = mpsc::channel();
    let native = native::Native {
        tx,
        shared: Arc::new(landesk_core::application::runtime::Shared::default()),
    };
    let state = server::AppState {
        shared: native.shared.clone(),
        native: Arc::new(native.clone()),
        transport_key: landesk_core::security::transport_key::TransportKey::new(
            connection.backend_key.clone(),
        )?,
        capture: capture::CaptureService::new(|| {
            Box::new(mac_capture::MacCaptureBackend::default())
        })?,
        files: file_worker::FileService::new(files::HomeFiles::open(std::path::Path::new(
            &objc2_foundation::NSHomeDirectory().to_string(),
        ))?),
    };
    let transport_listener = runtime.block_on(tokio::net::TcpListener::bind(("0.0.0.0", port)))?;
    let (updates, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TransportUpdate>();
    let failure = native.shared.clone();
    let transport = runtime.spawn(async move {
        let backend: crate::adapters::ssh::Backend = Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                let (transport, desktop) = tokio::io::duplex(65536);
                tokio::spawn(async move {
                    if let Err(error) = server::serve_stream(desktop, state).await {
                        crate::diagnostics::log(&format!("Desktop stream closed: {error:#}"));
                    }
                });
                Ok(Box::new(transport) as Box<dyn crate::adapters::ssh::StreamIo>)
            })
        });
        run_transport(
            transport_listener,
            connection,
            platform::macos::connection::path()?,
            backend,
            &mut receiver,
            failure,
        )
        .await
    });
    let shutdown = native.shared.clone();
    let ui_result = platform::macos::settings_ui::run(native, rx, port, password, updates);
    shutdown
        .shutdown
        .store(true, std::sync::atomic::Ordering::Release);
    ui_result?;
    runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(8), transport).await
    })???;
    Ok(())
}

pub struct TransportUpdate {
    pub listener: std::net::TcpListener,
    pub identity: landesk_core::domain::identity::ServerIdentity,
    pub ready: std::sync::mpsc::Sender<()>,
}

async fn run_transport(
    mut listener: tokio::net::TcpListener,
    mut identity: landesk_core::domain::identity::ServerIdentity,
    config_path: std::path::PathBuf,
    backend: crate::adapters::ssh::Backend,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<TransportUpdate>,
    failure: Arc<landesk_core::application::runtime::Shared>,
) -> Result<()> {
    let result = async {
        loop {
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let task = crate::adapters::ssh::run(
                listener,
                identity,
                crate::adapters::ssh::PasswordPolicy::LiveFile(config_path.clone()),
                backend.clone(),
                async {
                    let _ = stopped.await;
                },
            );
            tokio::pin!(task);
            tokio::select! {
                result = &mut task => break result,
                update = receiver.recv() => {
                    let _ = stop.send(());
                    tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await??;
                    let Some(update) = update else { break Ok(()); };
                    listener = tokio::net::TcpListener::from_std(update.listener)?;
                    identity = update.identity;
                    let _ = update.ready.send(());
                }
                _ = async {
                    while !failure.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                } => {
                    let _ = stop.send(());
                    break tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await?;
                }
            }
        }
    }
    .await;
    if result.is_err() {
        failure
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn changing_port_releases_old_listener_and_starts_ssh_on_new_listener() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let old = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let old_address = old.local_addr().unwrap();
            let new = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            new.set_nonblocking(true).unwrap();
            let new_address = new.local_addr().unwrap();
            let secret = landesk_core::security::secret::Secret::for_test(
                b"test",
                landesk_core::domain::identity::PASSWORD_PURPOSE,
            );
            let identity =
                crate::adapters::identity::create("test", old_address.port(), secret).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("config.json");
            crate::adapters::identity::write_new(&identity, &path).unwrap();
            let mut changed = crate::adapters::identity::read(&path).unwrap();
            changed.port = new_address.port();
            let (updates, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let shared = Arc::new(landesk_core::application::runtime::Shared::default());
            let stop = shared.clone();
            let backend: crate::adapters::ssh::Backend =
                Arc::new(|| Box::pin(async { anyhow::bail!("unused backend") }));
            let task = tokio::spawn(async move {
                run_transport(old, identity, path, backend, &mut receiver, shared).await
            });
            async fn banner(address: std::net::SocketAddr) {
                let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
                let mut prefix = [0; 4];
                stream.read_exact(&mut prefix).await.unwrap();
                assert_eq!(&prefix, b"SSH-");
            }
            banner(old_address).await;
            let (ready, acknowledged) = mpsc::channel();
            updates
                .send(TransportUpdate {
                    listener: new,
                    identity: changed,
                    ready,
                })
                .unwrap();
            tokio::task::spawn_blocking(move || acknowledged.recv_timeout(Duration::from_secs(6)))
                .await
                .unwrap()
                .unwrap();
            assert!(std::net::TcpListener::bind(old_address).is_ok());
            banner(new_address).await;
            stop.shutdown
                .store(true, std::sync::atomic::Ordering::Release);
            task.await.unwrap().unwrap();
            assert!(std::net::TcpListener::bind(new_address).is_ok());
        })
        .await
        .unwrap();
    }
}
