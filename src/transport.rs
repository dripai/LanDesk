//! Embedded SSH transport. Only the LanDesk HTTP endpoint is forwarded; no shell
//! or OS-account login is involved. Shared across platforms for protocol tests.
use anyhow::{Context, Result, ensure};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use http_body_util::BodyExt;
use hyper::{Request, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use russh::{
    Channel,
    server::{self, ChannelOpenHandle, Msg, Server as _, Session},
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet, time::timeout};
use zeroize::Zeroizing;

pub const USER: &str = "landesk";
pub const DEFAULT_PORT: u16 = 2222;
pub const AUTH_HEADER: &str = "x-landesk-transport-key";

pub trait StreamIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> StreamIo for T {}
pub type Backend = Arc<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Result<Box<dyn StreamIo>>> + Send + Sync,
>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    password_hash: String,
    host_key: String,
    pub backend_key: String,
}
impl Config {
    pub fn create(password: &str, port: u16) -> Result<Self> {
        crate::connection_password::validate(password)?;
        ensure!(port != 0, "SSH 端口无效");
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes())?
            .to_string();
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut()).map_err(|e| anyhow::anyhow!("随机数生成失败: {e}"))?;
        let key = russh::keys::PrivateKey::new(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed).into(),
            "LanDesk",
        )?;
        getrandom::fill(seed.as_mut()).map_err(|e| anyhow::anyhow!("随机数生成失败: {e}"))?;
        Ok(Self {
            port,
            password_hash,
            host_key: key
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)?
                .to_string(),
            backend_key: seed.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }
    pub fn read(path: &Path) -> Result<Self> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).context("缺少内置 SSH 配置，请运行安装程序")?,
        );
        let config: Self = serde_json::from_str(&text).context("内置 SSH 配置无效")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.port != 0, "SSH 端口无效");
        let hash = PasswordHash::new(&self.password_hash)?;
        ensure!(hash.algorithm.as_str() == "argon2id", "密码散列算法无效");
        self.private_key()?;
        ensure!(
            self.backend_key.len() == 64 && self.backend_key.bytes().all(|b| b.is_ascii_hexdigit()),
            "内部连接密钥无效"
        );
        Ok(())
    }
    pub fn write_new(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let data = Zeroizing::new(serde_json::to_vec(self)?);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        if let Err(error) = file.write_all(&data).and_then(|_| file.sync_all()) {
            drop(file);
            std::fs::remove_file(path).context("配置写入失败，且无法清理不完整配置")?;
            return Err(error.into());
        }
        Ok(())
    }
    /// Only the hash changes; host identity, port and desktop key stay unchanged.
    pub fn change_password(path: &Path, password: &str) -> Result<()> {
        use std::io::Write;
        crate::connection_password::validate(password)?;
        let mut config = Self::read(path)?;
        config.password_hash = Argon2::default()
            .hash_password(password.as_bytes())?
            .to_string();
        let parent = path.parent().context("连接配置路径无效")?;
        let mut stage = tempfile::NamedTempFile::new_in(parent)?;
        stage.write_all(&Zeroizing::new(serde_json::to_vec(&config)?))?;
        stage.as_file().sync_all()?;
        #[cfg(not(windows))]
        stage
            .persist(path)
            .map_err(|error| error.error)
            .context("保存连接密码失败")?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
            };
            let wide = |path: &Path| {
                path.as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect::<Vec<_>>()
            };
            let backup = parent.join(format!(
                "password-backup-{}.json",
                stage.path().file_stem().unwrap().to_string_lossy()
            ));
            let (_, stage) = stage.keep().map_err(|error| error.error)?;
            let result = unsafe {
                ReplaceFileW(
                    wide(path).as_ptr(),
                    wide(&stage).as_ptr(),
                    wide(&backup).as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                let error = std::io::Error::last_os_error();
                if backup.exists()
                    && unsafe {
                        MoveFileExW(
                            wide(&backup).as_ptr(),
                            wide(path).as_ptr(),
                            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                        )
                    } == 0
                {
                    anyhow::bail!(
                        "保存连接密码失败: {error}; 原配置恢复失败: {}，备份保留在 {}",
                        std::io::Error::last_os_error(),
                        backup.display()
                    );
                }
                if stage.exists() {
                    std::fs::remove_file(&stage)?;
                }
                return Err(error).context("保存连接密码失败，原配置已保留");
            }
            std::fs::remove_file(backup).context("连接密码已保存，但旧配置备份清理失败")?;
        }
        Ok(())
    }
    fn private_key(&self) -> Result<russh::keys::PrivateKey> {
        Ok(russh::keys::PrivateKey::from_openssh(&self.host_key)?)
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(self
            .private_key()?
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string())
    }
}

struct Transport {
    config: Arc<Config>,
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
        }
    }
    fn handle_session_error(&mut self, _: anyhow::Error) {
        // Do not log peer-controlled packets or credentials.
        crate::diagnostics::log("SSH connection closed with a protocol error");
    }
}
struct Client {
    config: Arc<Config>,
    config_path: PathBuf,
    backend: Backend,
    hashing: Arc<Semaphore>,
    channels: JoinSet<()>,
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
            let current = Config::read(&path)?;
            ensure!(
                current.port == config.port
                    && current.host_key == config.host_key
                    && current.backend_key == config.backend_key,
                "运行期间连接身份发生变化，请重新启动 LanDeskServer"
            );
            let hash = PasswordHash::new(&current.password_hash)?;
            Ok::<_, anyhow::Error>(
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok(),
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
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        if host != "127.0.0.1" || port != u32::from(crate::protocol::PORT) {
            return Ok(()); // Dropping the reply rejects the channel.
        }
        // The existing client probes this channel before opening its browser.
        let socket = match timeout(Duration::from_secs(10), (self.backend)()).await {
            Ok(Ok(socket)) => socket,
            Ok(Err(error)) => {
                crate::diagnostics::log(&format!("Desktop backend unavailable: {error:#}"));
                return Ok(());
            }
            Err(_) => {
                crate::diagnostics::log("Desktop backend connection timed out");
                return Ok(());
            }
        };
        while self.channels.try_join_next().is_some() {}
        let key = self.config.backend_key.clone();
        reply.accept().await;
        self.channels.spawn(async move {
            if proxy(channel.into_stream(), socket, key).await.is_err() {
                crate::diagnostics::log("Desktop forwarding channel closed with an error");
            }
        });
        Ok(())
    }
}

pub async fn run(
    listener: TcpListener,
    config: Config,
    config_path: PathBuf,
    backend: Backend,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    config.validate()?;
    let ssh = Arc::new(server::Config {
        keys: vec![config.private_key()?],
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
    use axum::{Router, extract::WebSocketUpgrade, response::IntoResponse, routing::get};
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

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
    async fn http(channel: Channel<russh::client::Msg>, path: &str) -> String {
        let mut stream = channel.into_stream();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:17890\r\nConnection: close\r\nx-landesk-transport-key: forged\r\n\r\n").as_bytes()).await.unwrap();
        let mut output = String::new();
        timeout(Duration::from_secs(5), stream.read_to_string(&mut output))
            .await
            .unwrap()
            .unwrap();
        output
    }
    #[tokio::test]
    async fn password_tunnel_serves_http_and_websocket_without_exposing_internal_key() {
        let config = Config::create("测试-access-password", DEFAULT_PORT).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.json");
        config.write_new(&config_path).unwrap();
        let public_key = config.private_key().unwrap().public_key().clone();
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
        assert!(peer.channel_open_session().await.is_err());
        assert!(
            peer.channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 0)
                .await
                .is_err()
        );
        assert!(
            peer.channel_open_direct_tcpip(
                "192.0.2.1",
                crate::protocol::PORT.into(),
                "127.0.0.1",
                0
            )
            .await
            .is_err()
        );
        assert!(peer.tcpip_forward("127.0.0.1", 0).await.is_err());
        for _ in 0..3 {
            let channel = peer
                .channel_open_direct_tcpip(
                    "127.0.0.1",
                    crate::protocol::PORT.into(),
                    "127.0.0.1",
                    0,
                )
                .await
                .unwrap();
            let response = http(channel, "/").await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.ends_with("desktop page"));
            assert!(!response.contains(&backend_key));
        }
        let channel = peer
            .channel_open_direct_tcpip("127.0.0.1", crate::protocol::PORT.into(), "127.0.0.1", 0)
            .await
            .unwrap();
        let mut request = "ws://127.0.0.1:17890/ws".into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "http://127.0.0.1:17890".parse().unwrap());
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
        let channel = second
            .channel_open_direct_tcpip("127.0.0.1", crate::protocol::PORT.into(), "127.0.0.1", 0)
            .await
            .unwrap();
        assert!(http(channel, "/").await.contains("desktop page"));
        Config::change_password(&config_path, "new-pass").unwrap();
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
        let channel = second
            .channel_open_direct_tcpip("127.0.0.1", crate::protocol::PORT.into(), "127.0.0.1", 0)
            .await
            .unwrap();
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
        let config = Config::create("another-test-password", DEFAULT_PORT).unwrap();
        let path =
            std::env::temp_dir().join(format!("landesk-ssh-test-{}.json", config.backend_key));
        config.write_new(&path).unwrap();
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains("another-test-password"));
        let reread = Config::read(&path).unwrap();
        assert_eq!(config.fingerprint().unwrap(), reread.fingerprint().unwrap());
        assert!(config.write_new(&path).is_err());
        assert!(Config::create("", DEFAULT_PORT).is_err());
        assert!(Config::change_password(&path, "").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stored);
        Config::change_password(&path, "abc12345").unwrap();
        let changed = Config::read(&path).unwrap();
        assert_eq!(
            changed.fingerprint().unwrap(),
            config.fingerprint().unwrap()
        );
        assert_eq!(changed.backend_key, config.backend_key);
        assert_eq!(changed.port, config.port);
        let hash = PasswordHash::new(&changed.password_hash).unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"abc12345", &hash)
                .is_ok()
        );
        assert!(
            Argon2::default()
                .verify_password(b"another-test-password", &hash)
                .is_err()
        );
        assert!(!std::fs::read_to_string(&path).unwrap().contains("abc12345"));
        std::fs::remove_file(path).unwrap();
    }
}
