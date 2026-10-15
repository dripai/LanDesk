use crate::adapters::identity;
use landesk_core::domain::identity::ServerIdentity;
// Embedded SSH transport. Only the LanDesk subsystem is accepted; no shell
// or OS-account login is involved. Shared across platforms for protocol tests.
use anyhow::Context;
use anyhow::Result;
use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use russh::Channel;
use russh::ChannelId;
use russh::server;
use russh::server::ChannelOpenHandle;
use russh::server::Msg;
use russh::server::Server as _;
use russh::server::Session;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use zeroize::Zeroizing;

pub use landesk_core::protocol::desktop::SUBSYSTEM;
pub const USER: &str = "landesk";
pub const DEFAULT_PORT: u16 = 2222;
pub const AUTH_HEADER: &str = "x-landesk-transport-key";
#[cfg(test)]
use landesk_core::domain::identity::PASSWORD_PURPOSE;

pub trait StreamIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> StreamIo for T {}
pub type Backend = Arc<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Result<Box<dyn StreamIo>>> + Send + Sync,
>;

struct Transport {
    config: Arc<ServerIdentity>,
    config_path: PathBuf,
    backend: Backend,
    hashing: Arc<Semaphore>,
}
impl server::Server for Transport {
    type Handler = Client;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Client {
        Client {
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            backend: self.backend.clone(),
            hashing: self.hashing.clone(),
            channels: JoinSet::new(),
            pending: HashMap::new(),
        }
    }
    fn handle_session_error(&mut self, _: anyhow::Error) {
        // Do not log peer-controlled packets or credentials.
        crate::diagnostics::log("SSH connection closed with a protocol error");
    }
}
struct Client {
    config: Arc<ServerIdentity>,
    config_path: PathBuf,
    backend: Backend,
    hashing: Arc<Semaphore>,
    channels: JoinSet<()>,
    pending: HashMap<ChannelId, Channel<Msg>>,
}
fn password_rejected() -> server::Auth {
    server::Auth::Reject {
        proceed_with_methods: Some((&[russh::MethodKind::Password][..]).into()),
        partial_success: false,
    }
}
impl server::Handler for Client {
    type Error = anyhow::Error;
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<server::Auth> {
        if user != USER || password.is_empty() || password.len() > 1024 {
            return Ok(password_rejected());
        }
        let permit = self.hashing.clone().acquire_owned().await?;
        let config = self.config.clone();
        let path = self.config_path.clone();
        let password = Zeroizing::new(password.to_owned());
        let accepted = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            landesk_core::application::password::authenticate(
                &identity::IdentityFile(path),
                &config,
                &password,
            )
        })
        .await??;
        crate::diagnostics::log(if accepted {
            "SSH password authentication accepted"
        } else {
            "SSH password authentication rejected"
        });
        Ok(if accepted {
            server::Auth::Accept
        } else {
            password_rejected()
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        while self.channels.try_join_next().is_some() {}
        if self.pending.len() + self.channels.len() >= 32 {
            return Ok(());
        }
        self.pending.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn channel_close(&mut self, channel: ChannelId, _: &mut Session) -> Result<()> {
        self.pending.remove(&channel);
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<()> {
        let channel = self.pending.remove(&id);
        if name != SUBSYSTEM || channel.is_none() {
            session.channel_failure(id)?;
            session.close(id)?;
            return Ok(());
        }
        let socket = match timeout(Duration::from_secs(10), (self.backend)()).await {
            Ok(Ok(socket)) => socket,
            result => {
                crate::diagnostics::log(&format!(
                    "Desktop backend unavailable: {}",
                    match result {
                        Ok(Err(error)) => format!("{error:#}"),
                        _ => "connection timed out".into(),
                    }
                ));
                session.channel_failure(id)?;
                session.close(id)?;
                return Ok(());
            }
        };
        session.channel_success(id)?;
        let key = self.config.backend_key.clone();
        self.channels.spawn(async move {
            if proxy(channel.unwrap().into_stream(), socket, key)
                .await
                .is_err()
            {
                crate::diagnostics::log("Desktop subsystem channel closed with an error");
            }
        });
        Ok(())
    }
    async fn shell_request(&mut self, id: ChannelId, session: &mut Session) -> Result<()> {
        self.pending.remove(&id);
        session.channel_failure(id)?;
        session.close(id)?;
        Ok(())
    }
    async fn exec_request(&mut self, id: ChannelId, _: &[u8], session: &mut Session) -> Result<()> {
        self.shell_request(id, session).await
    }
}

pub async fn run(
    listener: TcpListener,
    config: ServerIdentity,
    config_path: PathBuf,
    backend: Backend,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    config.validate()?;
    let ssh = Arc::new(server::Config {
        keys: vec![identity::private_key(&config)?],
        methods: (&[russh::MethodKind::Password][..]).into(),
        keepalive_interval: Some(Duration::from_secs(15)),
        nodelay: true,
        ..Default::default()
    });
    let mut transport = Transport {
        config: Arc::new(config),
        config_path,
        backend,
        hashing: Arc::new(Semaphore::new(4)),
    };
    let server = transport.run_on_socket(ssh, &listener);
    let handle = server.handle();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result.context("内置 SSH 服务停止"),
        _ = stop => { handle.shutdown("LanDesk service stopped".into()); server.await?; Ok(()) }
    }
}

async fn proxy(
    stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    backend: Box<dyn StreamIo>,
    key: String,
) -> Result<()> {
    let (sender, connection) =
        hyper::client::conn::http1::handshake::<_, Incoming>(TokioIo::new(backend)).await?;
    let mut driver = JoinSet::new();
    driver.spawn(connection.with_upgrades());
    let sender = Arc::new(tokio::sync::Mutex::new(sender));
    let pending = Arc::new(Mutex::new(None));
    let upgrade = pending.clone();
    let service = service_fn(move |mut request: Request<Incoming>| {
        let sender = sender.clone();
        let pending = pending.clone();
        let key = key.clone();
        async move {
            // Overwrite peer-supplied values. This key never leaves the two local services.
            request.headers_mut().insert(AUTH_HEADER, key.parse()?);
            request.headers_mut().insert(
                hyper::header::HOST,
                landesk_core::protocol::INTERNAL_HOST.parse()?,
            );
            if request.headers().contains_key(hyper::header::ORIGIN) {
                request.headers_mut().insert(
                    hyper::header::ORIGIN,
                    landesk_core::protocol::desktop::INTERNAL_ORIGIN.parse()?,
                );
            }
            request.headers_mut().remove(hyper::header::COOKIE);
            let local_upgrade = request
                .headers()
                .contains_key(hyper::header::UPGRADE)
                .then(|| hyper::upgrade::on(&mut request));
            let mut response = timeout(
                Duration::from_secs(15),
                sender.lock().await.send_request(request),
            )
            .await
            .context("桌面服务响应超时")??;
            if response.status() == hyper::StatusCode::SWITCHING_PROTOCOLS {
                *pending
                    .lock()
                    .map_err(|_| anyhow::anyhow!("升级状态不可用"))? = Some((
                    local_upgrade.context("未请求协议升级")?,
                    hyper::upgrade::on(&mut response),
                ));
            }
            Ok::<_, anyhow::Error>(response.map(BodyExt::boxed))
        }
    });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await?;
    let pair = upgrade
        .lock()
        .map_err(|_| anyhow::anyhow!("升级状态不可用"))?
        .take();
    if let Some((local, remote)) = pair {
        let (local, remote) = timeout(Duration::from_secs(10), async {
            tokio::try_join!(local, remote)
        })
        .await??;
        tokio::io::copy_bidirectional(&mut TokioIo::new(local), &mut TokioIo::new(remote)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::WebSocketUpgrade;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    fn secret(password: &str) -> landesk_core::security::secret::Secret {
        landesk_core::security::secret::Secret::for_test(password.as_bytes(), PASSWORD_PURPOSE)
    }
    #[test]
    fn saved_config_recovers_ciphertext_after_restart_and_commits_rotation_together() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        crate::adapters::identity::write_new(
            &identity::create("aB123456", DEFAULT_PORT, secret("aB123456")).unwrap(),
            &path,
        )
        .unwrap();
        let first = identity::read(&path).unwrap();
        assert_eq!(
            &*first.password_secret().decrypt_for_test(PASSWORD_PURPOSE),
            b"aB123456"
        );
        identity::change_password(&path, "xY987654", secret("xY987654")).unwrap();
        let restarted = identity::read(&path).unwrap();
        assert_eq!(
            &*restarted
                .password_secret()
                .decrypt_for_test(PASSWORD_PURPOSE),
            b"xY987654"
        );
        assert_eq!(
            crate::adapters::identity::fingerprint(&first).unwrap(),
            crate::adapters::identity::fingerprint(&restarted).unwrap()
        );
        assert_eq!(first.backend_key, restarted.backend_key);
        assert!(restarted.verify_password("xY987654").unwrap());
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains("aB123456") && !stored.contains("xY987654"));
        let mut old_format: serde_json::Value = serde_json::from_str(&stored).unwrap();
        old_format
            .as_object_mut()
            .unwrap()
            .remove("display_password");
        std::fs::write(&path, serde_json::to_vec(&old_format).unwrap()).unwrap();
        assert!(identity::read(&path).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn native_windows_stored_config_recovers_password_after_restart_and_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        crate::adapters::identity::write_new(
            &identity::create_stored("aB123456", DEFAULT_PORT).unwrap(),
            &path,
        )
        .unwrap();
        assert_eq!(
            landesk_core::application::password::recover_password(
                &identity::read(&path).unwrap(),
                &crate::platform::secrets::NativeSecretProtector
            )
            .unwrap()
            .as_str(),
            "aB123456"
        );
        assert!(matches!(
            identity::change_stored_password(&path, "xY987654"),
            landesk_core::application::password::PasswordOutcome::Saved { .. }
        ));
        assert_eq!(
            landesk_core::application::password::recover_password(
                &identity::read(&path).unwrap(),
                &crate::platform::secrets::NativeSecretProtector
            )
            .unwrap()
            .as_str(),
            "xY987654"
        );
    }

    struct Peer(russh::keys::PublicKey);
    impl russh::client::Handler for Peer {
        type Error = anyhow::Error;
        async fn check_server_key(
            &mut self,
            key: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool> {
            Ok(key.public_key().key_data() == self.0.key_data())
        }
    }
    async fn connect(addr: SocketAddr, key: russh::keys::PublicKey) -> russh::client::Handle<Peer> {
        russh::client::connect(Arc::new(russh::client::Config::default()), addr, Peer(key))
            .await
            .unwrap()
    }
    async fn open_desktop(peer: &russh::client::Handle<Peer>) -> Channel<russh::client::Msg> {
        let mut channel = peer.channel_open_session().await.unwrap();
        channel.request_subsystem(true, SUBSYSTEM).await.unwrap();
        assert!(matches!(
            channel.wait().await,
            Some(russh::ChannelMsg::Success)
        ));
        channel
    }
    async fn http(channel: Channel<russh::client::Msg>, path: &str) -> String {
        let mut stream = channel.into_stream();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: landesk.internal\r\nConnection: close\r\nx-landesk-transport-key: forged\r\n\r\n").as_bytes()).await.unwrap();
        let mut output = String::new();
        timeout(Duration::from_secs(5), stream.read_to_string(&mut output))
            .await
            .unwrap()
            .unwrap();
        output
    }
    #[tokio::test]
    async fn password_tunnel_serves_http_and_websocket_without_exposing_internal_key() {
        let config = identity::create(
            "测试-access-password",
            DEFAULT_PORT,
            secret("测试-access-password"),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.json");
        crate::adapters::identity::write_new(&config, &config_path).unwrap();
        let public_key = identity::private_key(&config).unwrap().public_key().clone();
        let backend_key = config.backend_key.clone();
        let expected = backend_key.clone();
        let app = Router::new()
            .route("/", get(|| async { "desktop page" }))
            .route(
                "/ws",
                get(|ws: WebSocketUpgrade| async {
                    ws.on_upgrade(|mut ws| async move {
                        while let Some(Ok(message)) = ws.next().await {
                            if ws.send(message).await.is_err() {
                                break;
                            }
                        }
                    })
                }),
            )
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let expected = expected.clone();
                    async move {
                        if request
                            .headers()
                            .get(AUTH_HEADER)
                            .and_then(|v| v.to_str().ok())
                            != Some(expected.as_str())
                        {
                            return hyper::StatusCode::UNAUTHORIZED.into_response();
                        }
                        next.run(request).await
                    }
                },
            ));
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        let backend_task = tokio::spawn(async move {
            axum::serve(backend, app).await.unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run(
            listener,
            config,
            config_path.clone(),
            Arc::new(move || {
                Box::pin(async move {
                    Ok(
                        Box::new(tokio::net::TcpStream::connect(backend_addr).await?)
                            as Box<dyn StreamIo>,
                    )
                })
            }),
            async {
                let _ = stopped.await;
            },
        ));

        let mut peer = connect(addr, public_key.clone()).await;
        assert!(
            !peer
                .authenticate_password(USER, "wrong")
                .await
                .unwrap()
                .success()
        );
        assert!(
            !peer
                .authenticate_password("Administrator", "测试-access-password")
                .await
                .unwrap()
                .success()
        );
        assert!(
            peer.authenticate_password(USER, "测试-access-password")
                .await
                .unwrap()
                .success()
        );
        let mut unsupported = peer.channel_open_session().await.unwrap();
        unsupported.request_subsystem(true, "sftp").await.unwrap();
        assert!(matches!(
            unsupported.wait().await,
            Some(russh::ChannelMsg::Failure)
        ));
        let mut shell = peer.channel_open_session().await.unwrap();
        shell.request_shell(true).await.unwrap();
        assert!(matches!(
            shell.wait().await,
            Some(russh::ChannelMsg::Failure)
        ));
        assert!(
            peer.channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 0)
                .await
                .is_err()
        );
        assert!(
            peer.channel_open_direct_tcpip("192.0.2.1", 2222, "127.0.0.1", 0)
                .await
                .is_err()
        );
        assert!(peer.tcpip_forward("127.0.0.1", 0).await.is_err());
        for _ in 0..3 {
            let channel = open_desktop(&peer).await;
            let response = http(channel, "/").await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.ends_with("desktop page"));
            assert!(!response.contains(&backend_key));
        }
        let channel = open_desktop(&peer).await;
        let mut request = "ws://landesk.internal/ws".into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "http://landesk.internal".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::client_async(request, channel.into_stream())
            .await
            .unwrap();
        for message in [
            Message::Text(r#"{"type":"hello"}"#.into()),
            Message::Binary(vec![1, 3, 7, 0, 255].into()),
        ] {
            ws.send(message.clone()).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap(), message);
        }
        // Disconnect must close the upgraded stream, not leave an orphan controller.
        peer.disconnect(russh::Disconnect::ByApplication, "test", "")
            .await
            .unwrap();
        let closed = timeout(Duration::from_secs(5), ws.next()).await.unwrap();
        assert!(closed.is_none() || matches!(closed, Some(Err(_)) | Some(Ok(Message::Close(_)))));
        let mut second = connect(addr, public_key.clone()).await;
        assert!(
            second
                .authenticate_password(USER, "测试-access-password")
                .await
                .unwrap()
                .success()
        );
        let channel = open_desktop(&second).await;
        assert!(http(channel, "/").await.contains("desktop page"));
        identity::change_password(&config_path, "new-pass", secret("new-pass")).unwrap();
        let mut old = connect(addr, public_key).await;
        assert!(
            !old.authenticate_password(USER, "测试-access-password")
                .await
                .unwrap()
                .success()
        );
        assert!(
            old.authenticate_password(USER, "new-pass")
                .await
                .unwrap()
                .success()
        );
        let channel = open_desktop(&second).await;
        assert!(http(channel, "/").await.contains("desktop page"));
        stop.send(()).unwrap();
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        backend_task.abort();
    }

    #[test]
    fn stored_configuration_hashes_password_and_preserves_host_identity() {
        let config = identity::create(
            "another-test-password",
            DEFAULT_PORT,
            secret("another-test-password"),
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("landesk-ssh-test-{}.json", config.backend_key));
        crate::adapters::identity::write_new(&config, &path).unwrap();
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains("another-test-password"));
        let reread = identity::read(&path).unwrap();
        assert_eq!(
            crate::adapters::identity::fingerprint(&config).unwrap(),
            crate::adapters::identity::fingerprint(&reread).unwrap()
        );
        assert!(crate::adapters::identity::write_new(&config, &path).is_err());
        assert!(identity::create("", DEFAULT_PORT, secret("")).is_err());
        assert!(identity::change_password(&path, "", secret("")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stored);
        identity::change_password(&path, "abc12345", secret("abc12345")).unwrap();
        let changed = identity::read(&path).unwrap();
        assert_eq!(
            crate::adapters::identity::fingerprint(&changed).unwrap(),
            crate::adapters::identity::fingerprint(&config).unwrap()
        );
        assert_eq!(changed.backend_key, config.backend_key);
        assert_eq!(changed.port, config.port);
        assert!(changed.verify_password("abc12345").unwrap());
        assert!(!changed.verify_password("another-test-password").unwrap());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("abc12345"));
        std::fs::remove_file(path).unwrap();
    }
}
