use crate::settings::Settings;
use anyhow::{Context, Result, bail, ensure};
use russh::{
    Disconnect, client,
    keys::{self, PublicKeyOrCertificate},
};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet, time::timeout};
use zeroize::Zeroizing;

pub const VIEWER_URL: &str = "http://127.0.0.1:17890";
const PORT: u16 = 17890;

pub enum Event {
    Status(String),
    Ready,
    Stopped(Result<()>),
}

struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, server: &PublicKeyOrCertificate) -> Result<bool> {
        ensure!(
            server.certificate().is_none(),
            "首版暂不支持 SSH 主机证书，请使用普通主机密钥"
        );
        let key = server.public_key();
        if keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
            .context("SSH 主机密钥已变化或信任文件无法读取，请核对 Mac 主机")?
        {
            return Ok(true);
        }
        save_host_key(&self.host, self.port, &key, &self.known_hosts)
            .context("无法保存首次连接的 SSH 主机密钥")?;
        Ok(true)
    }
}

fn save_host_key(
    host: &str,
    port: u16,
    key: &keys::PublicKey,
    path: &std::path::Path,
) -> Result<()> {
    let parent = path.parent().context("主机信任文件路径无效")?;
    std::fs::create_dir_all(parent)?;
    let old = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&old)?;
    // Keep russh's OpenSSH serialization, but publish only after verifying its
    // buffered write and syncing it. Its append helper alone does not report
    // errors from BufWriter::drop and can leave an incomplete live file.
    keys::known_hosts::learn_known_hosts_path(host, port, key, file.path())?;
    ensure!(
        keys::check_known_hosts_path(host, port, key, file.path())?,
        "主机密钥写入不完整"
    );
    file.as_file().sync_all()?;
    file.persist(path).context("主机信任文件保存失败")?;
    Ok(())
}

pub async fn run(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    cancel: oneshot::Receiver<()>,
) -> Result<()> {
    settings.validate()?;
    ensure!(!password.is_empty(), "请输入 Mac SSH 密码");
    // Reserve the fixed browser origin before authenticating. Never use another port silently.
    let listener = TcpListener::bind(("127.0.0.1", PORT))
        .await
        .context("本机 17890 端口被占用，请关闭旧连接脚本或另一个客户端")?;
    run_bound(settings, password, known_hosts, events, cancel, listener).await
}

async fn run_bound(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    mut cancel: oneshot::Receiver<()>,
    listener: TcpListener,
) -> Result<()> {
    events.send(Event::Status("正在连接 SSH…".into()))?;
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 2,
        ..Default::default()
    });
    let handler = Handler {
        host: settings.host.clone(),
        port: settings.ssh_port,
        known_hosts,
    };
    let mut session = tokio::select! {
        _ = &mut cancel => return Ok(()),
        result = timeout(Duration::from_secs(20), client::connect(config, (settings.host.as_str(), settings.ssh_port), handler)) => result.context("SSH 连接超时")??,
    };
    let result = tokio::select! {
        _ = &mut cancel => Ok(()),
        result = async {
            events.send(Event::Status("正在验证 Mac 用户…".into()))?;
            let auth = timeout(Duration::from_secs(20), session.authenticate_password(&settings.user, password.to_string())).await.context("SSH 密码验证超时")??;
            drop(password);
            ensure!(auth.success(), "SSH 密码验证失败，请检查用户名、密码及 Mac 远程登录设置");
            let probe = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), "127.0.0.1", 0)).await.context("Mac 服务检查超时")?.context("无法访问 Mac LanDesk，请先启动 Mac 应用")?;
            probe.close().await?;
            events.send(Event::Ready)?;
            let mut transfers = JoinSet::new();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = tick.tick() => { ensure!(!session.is_closed(), "SSH 连接已断开"); }
                    result = transfers.join_next(), if !transfers.is_empty() => {
                        if let Some(Err(error)) = result { bail!("隧道任务异常：{error}"); }
                        // Browser cancellation/reset is normal and affects only that TCP stream.
                    }
                    accepted = listener.accept(), if transfers.len() < 32 => {
                        let (mut socket, address) = accepted?;
                        let channel = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), address.ip().to_string(), address.port().into())).await.context("转发通道打开超时")??;
                        transfers.spawn(async move { tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await });
                    }
                }
            }
        } => result,
    };
    // Dropping the transfer JoinSet cancels every local socket before SSH is closed.
    let _ = timeout(
        Duration::from_secs(3),
        session.disconnect(Disconnect::ByApplication, "LanDeskClient disconnected", ""),
    )
    .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct EchoServer;
    impl russh::server::Handler for EchoServer {
        type Error = anyhow::Error;
        async fn auth_password(
            &mut self,
            user: &str,
            password: &str,
        ) -> Result<russh::server::Auth> {
            Ok(if user == "tester" && password == "test-only" {
                russh::server::Auth::Accept
            } else {
                russh::server::Auth::reject()
            })
        }
        async fn channel_open_direct_tcpip(
            &mut self,
            channel: russh::Channel<russh::server::Msg>,
            host: &str,
            port: u32,
            _: &str,
            _: u32,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<()> {
            ensure!(
                host == "127.0.0.1" && port == u32::from(PORT),
                "unexpected forwarding destination"
            );
            reply.accept().await;
            tokio::spawn(async move {
                let (mut input, mut output) = tokio::io::split(channel.into_stream());
                let _ = tokio::io::copy(&mut input, &mut output).await;
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn encrypted_tunnel_transfers_bytes_and_cancellation_releases_listener() {
        timeout(Duration::from_secs(10), async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("known_hosts");
            let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            let ssh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let ssh_port = ssh_listener.local_addr().unwrap().port();
            keys::known_hosts::learn_known_hosts_path(
                "127.0.0.1",
                ssh_port,
                key.public_key(),
                &path,
            )
            .unwrap();
            let config = Arc::new(russh::server::Config {
                keys: vec![key],
                ..Default::default()
            });
            let server = tokio::spawn(async move {
                let (socket, _) = ssh_listener.accept().await.unwrap();
                russh::server::run_stream(config, socket, EchoServer)
                    .await
                    .unwrap()
                    .await
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let settings = Settings {
                host: "127.0.0.1".into(),
                user: "tester".into(),
                ssh_port,
                ..Settings::default()
            };
            let (events, receiver) = mpsc::channel();
            let (cancel, cancelled) = oneshot::channel();
            let tunnel = tokio::spawn(run_bound(
                settings,
                Zeroizing::new("test-only".into()),
                path,
                events,
                cancelled,
                listener,
            ));
            loop {
                if let Ok(Event::Ready) = receiver.try_recv() {
                    break;
                }
                assert!(!tunnel.is_finished(), "tunnel failed before ready");
                tokio::task::yield_now().await;
            }
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let bytes: Vec<u8> = (0..65536).map(|n| (n % 251) as u8).collect();
            socket.write_all(&bytes).await.unwrap();
            let mut received = vec![0; bytes.len()];
            socket.read_exact(&mut received).await.unwrap();
            assert_eq!(received, bytes);
            cancel.send(()).unwrap();
            tunnel.await.unwrap().unwrap();
            assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
            let _listener = TcpListener::bind(address).await.unwrap();
            server.abort();
        })
        .await
        .unwrap();
    }

    fn key() -> PublicKeyOrCertificate {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
            .into()
    }
    fn handler(path: PathBuf) -> Handler {
        Handler {
            host: "example.test".into(),
            port: 22,
            known_hosts: path,
        }
    }
    #[tokio::test]
    async fn first_host_is_recorded_automatically_and_changed_key_is_rejected() {
        use russh::client::Handler as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let server_key = key();
        let mut client = handler(path.clone());
        assert!(client.check_server_key(&server_key).await.unwrap());
        assert!(path.exists());
        let original = std::fs::read(&path).unwrap();
        assert!(client.check_server_key(&server_key).await.unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(client.check_server_key(&key()).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[tokio::test]
    async fn host_record_errors_are_not_silently_accepted() {
        use russh::client::Handler as _;
        let directory = tempfile::tempdir().unwrap();
        let mut client = handler(directory.path().to_owned());
        assert!(client.check_server_key(&key()).await.is_err());
    }
}
