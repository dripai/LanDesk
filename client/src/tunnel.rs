use crate::{
    gateway::{Registration, Routes},
    settings::Settings,
    viewer,
};
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
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::{sync::oneshot, task::JoinSet, time::timeout};
use zeroize::Zeroizing;

const PORT: u16 = 17890;

pub enum Event {
    Status(String),
    Ready,
    WaitingForReconnect(String),
    Stopped(Result<StopReason>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum StopReason {
    Disconnected,
}

static HOST_KEYS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            "首版暂不支持设备证书，请使用普通主机密钥"
        );
        let _lock = HOST_KEYS
            .lock()
            .map_err(|_| anyhow::anyhow!("设备信任状态不可用"))?;
        let key = server.public_key();
        if keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
            .context("设备密钥已变化或信任文件无法读取，请核对目标设备")?
        {
            return Ok(true);
        }
        save_host_key(&self.host, self.port, &key, &self.known_hosts)
            .context("无法保存首次连接的设备密钥")?;
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

trait StreamIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> StreamIo for T {}
enum Incoming {
    Routed {
        receiver: tokio::sync::mpsc::Receiver<tokio::io::DuplexStream>,
        registration: Registration,
    },
    #[cfg(test)]
    Local(TcpListener),
}
impl Incoming {
    fn is_active(&self) -> bool {
        match self {
            Self::Routed { registration, .. } => registration.is_active(),
            #[cfg(test)]
            Self::Local(_) => true,
        }
    }
    fn activate(&mut self) -> Result<()> {
        match self {
            Self::Routed { registration, .. } => registration.activate(),
            #[cfg(test)]
            Self::Local(_) => Ok(()),
        }
    }
    async fn accept(&mut self) -> Result<Box<dyn StreamIo>> {
        match self {
            Self::Routed { receiver, .. } => {
                Ok(Box::new(receiver.recv().await.context("网页入口已关闭")?))
            }
            #[cfg(test)]
            Self::Local(listener) => Ok(Box::new(listener.accept().await?.0)),
        }
    }
}
pub async fn run(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    cancel: oneshot::Receiver<()>,
    routes: Routes,
) -> Result<StopReason> {
    settings.validate()?;
    ensure!(!password.is_empty(), "请输入 LanDesk 访问密码");
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    let registration = Registration::new(settings.id(), routes, sender);
    maintain(
        settings,
        password,
        known_hosts,
        events,
        cancel,
        Incoming::Routed {
            receiver,
            registration,
        },
    )
    .await
}

// Keep the authorized route and its in-memory password until explicit disconnect.
// A new browser request restarts transport after a network failure; no background retry loop.
async fn maintain(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    mut cancel: oneshot::Receiver<()>,
    mut incoming: Incoming,
) -> Result<StopReason> {
    let mut first = None;
    loop {
        let result = run_bound(
            settings.clone(),
            Zeroizing::new(password.to_string()),
            known_hosts.clone(),
            events.clone(),
            &mut cancel,
            &mut incoming,
            first.take(),
        )
        .await;
        match result {
            Ok(reason) => return Ok(reason),
            Err(error) if !incoming.is_active() => return Err(error),
            Err(error) => {
                events.send(Event::WaitingForReconnect(format!(
                    "连接中断：{error:#}；可在网页重新连接"
                )))?;
            }
        }
        first = Some(tokio::select! {
            _ = &mut cancel => return Ok(StopReason::Disconnected),
            socket = incoming.accept() => socket?,
        });
    }
}

async fn run_bound(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    cancel: &mut oneshot::Receiver<()>,
    incoming: &mut Incoming,
    mut first: Option<Box<dyn StreamIo>>,
) -> Result<StopReason> {
    events.send(Event::Status("正在建立加密连接…".into()))?;
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 2,
        ..Default::default()
    });
    let handler = Handler {
        host: settings.normalized_host(),
        port: settings.port,
        known_hosts,
    };
    let mut session = Arc::new(tokio::select! {
        _ = &mut *cancel => return Ok(StopReason::Disconnected),
        result = timeout(Duration::from_secs(20), client::connect(config, (settings.host.as_str(), settings.port), handler)) => result.context("加密连接超时")??,
    });
    let result = tokio::select! {
        _ = &mut *cancel => Ok(StopReason::Disconnected),
        result = async {
            events.send(Event::Status("正在验证访问密码…".into()))?;
            let auth = timeout(Duration::from_secs(20), Arc::get_mut(&mut session).context("访问认证期间会话已被占用")?.authenticate_password("landesk", password.to_string())).await.context("访问密码验证超时")??;
            drop(password);
            ensure!(auth.success(), "访问密码验证失败，请使用 LanDeskServer 设置的密码");
            let probe = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), "127.0.0.1", 0)).await.context("远程服务检查超时")?.context("无法访问 LanDesk 服务端，请先在目标电脑启动应用")?;
            probe.close().await?;
            incoming.activate()?;
            events.send(Event::Ready)?;
            let mut transfers = JoinSet::new();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = tick.tick() => { ensure!(!session.is_closed(), "加密连接已断开"); }
                    result = transfers.join_next(), if !transfers.is_empty() => {
                        if let Some(Err(error)) = result { bail!("隧道任务异常：{error}"); }
                        if let Some(Ok(Err(error))) = result {
                            events.send(Event::Status(format!("网页连接已结束：{error:#}")))?;
                        }
                    }
                    accepted = async { match first.take() { Some(socket) => Ok(socket), None => incoming.accept().await } }, if transfers.len() < 32 => {
                        let socket = accepted?;
                        let session = session.clone();
                        transfers.spawn(async move {
                            let channel = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), "127.0.0.1", 0)).await.context("转发通道打开超时")??;
                            viewer::forward(socket, channel.into_stream()).await
                        });
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

    struct EchoServer(u16);
    impl russh::server::Handler for EchoServer {
        type Error = anyhow::Error;
        async fn auth_password(
            &mut self,
            user: &str,
            password: &str,
        ) -> Result<russh::server::Auth> {
            Ok(if user == "landesk" && password == "test-only" {
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
            let label = self.0;
            tokio::spawn(async move {
                let mut stream = channel.into_stream();
                while let Ok(request) = read_headers(&mut stream).await {
                    if request.starts_with("GET /ws ") {
                        if stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").await.is_err() { break; }
                        let (mut input, mut output) = tokio::io::split(stream);
                        let _ = tokio::io::copy(&mut input, &mut output).await;
                        break;
                    }
                    if request.starts_with("GET /probe") {
                        let body = format!("{label}:{}", request.lines().next().unwrap());
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if stream.write_all(response.as_bytes()).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    if stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Ok(())
        }
    }

    async fn read_headers<R: tokio::io::AsyncRead + Unpin>(stream: &mut R) -> Result<String> {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            ensure!(bytes.len() < 8192, "test header too large");
            bytes.push(stream.read_u8().await?);
        }
        Ok(String::from_utf8(bytes)?)
    }

    struct TestTunnel {
        _directory: tempfile::TempDir,
        address: std::net::SocketAddr,
        id: String,
        ssh_port: u16,
        cancel: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<Result<StopReason>>,
        server: tokio::task::JoinHandle<()>,
        cut: tokio::sync::mpsc::Sender<()>,
        events: mpsc::Receiver<Event>,
    }
    impl Drop for TestTunnel {
        fn drop(&mut self) {
            self.task.abort();
            self.server.abort();
        }
    }
    impl TestTunnel {
        async fn new() -> Self {
            Self::new_with_routes(None).await
        }
        async fn new_with_routes(routes: Option<Routes>) -> Self {
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
            let (cut, mut cut_rx) = tokio::sync::mpsc::channel(1);
            let server = tokio::spawn(async move {
                while let Ok((socket, _)) = ssh_listener.accept().await {
                    let mut connection =
                        russh::server::run_stream(config.clone(), socket, EchoServer(ssh_port))
                            .await
                            .unwrap();
                    tokio::select! {
                        _ = &mut connection => {},
                        _ = cut_rx.recv() => {
                            let _ = connection.handle().disconnect(Disconnect::ByApplication, "test network interruption".into(), "".into()).await;
                            let _ = connection.await;
                        }
                    }
                }
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let settings = Settings {
                host: "127.0.0.1".into(),
                port: ssh_port,
                ..Settings::default()
            };
            let (events, receiver) = mpsc::channel();
            let (cancel, cancelled) = oneshot::channel();
            let id = settings.id();
            let task = if let Some(routes) = routes {
                tokio::spawn(run(
                    settings,
                    Zeroizing::new("test-only".into()),
                    path,
                    events,
                    cancelled,
                    routes,
                ))
            } else {
                tokio::spawn(maintain(
                    settings,
                    Zeroizing::new("test-only".into()),
                    path,
                    events,
                    cancelled,
                    Incoming::Local(listener),
                ))
            };
            let fixture = Self {
                _directory: directory,
                address,
                id,
                ssh_port,
                cancel: Some(cancel),
                task,
                server,
                cut,
                events: receiver,
            };
            timeout(Duration::from_secs(10), async {
                loop {
                    if let Ok(Event::Ready) = fixture.events.try_recv() {
                        break;
                    }
                    assert!(!fixture.task.is_finished(), "tunnel failed before ready");
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fixture
        }
        async fn viewer(&self) -> tokio::net::TcpStream {
            let mut socket = tokio::net::TcpStream::connect(self.address).await.unwrap();
            // Reuse a keep-alive HTTP connection for the upgrade, as browsers may do.
            socket
                .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:17890\r\n\r\n")
                .await
                .unwrap();
            assert!(read_headers(&mut socket).await.unwrap().contains("200 OK"));
            socket.write_all(b"GET /ws HTTP/1.1\r\nHost: 127.0.0.1:17890\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nOrigin: http://127.0.0.1:17890\r\n\r\n").await.unwrap();
            assert!(
                read_headers(&mut socket)
                    .await
                    .unwrap()
                    .contains("101 Switching Protocols")
            );
            socket
        }
    }

    #[tokio::test]
    async fn encrypted_websocket_transfers_bytes_and_cancellation_releases_listener() {
        timeout(Duration::from_secs(10), async {
            let mut fixture = TestTunnel::new().await;
            let mut socket = fixture.viewer().await;
            let bytes: Vec<u8> = (0..65536).map(|n| (n % 251) as u8).collect();
            socket.write_all(&bytes).await.unwrap();
            let mut received = vec![0; bytes.len()];
            socket.read_exact(&mut received).await.unwrap();
            assert_eq!(received, bytes);
            fixture.cancel.take().unwrap().send(()).unwrap();
            assert_eq!(
                (&mut fixture.task).await.unwrap().unwrap(),
                StopReason::Disconnected
            );
            assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
            let _listener = TcpListener::bind(fixture.address).await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn closing_all_viewers_does_not_expire_connection_and_browser_can_reconnect() {
        let mut fixture = TestTunnel::new().await;
        let first = fixture.viewer().await;
        drop(first);
        tokio::time::sleep(Duration::from_secs(32)).await;
        assert!(
            !fixture.task.is_finished(),
            "closing all tabs must not expire the connection"
        );
        let mut refreshed = fixture.viewer().await;
        refreshed.write_all(b"reconnected").await.unwrap();
        let mut bytes = [0; 11];
        refreshed.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"reconnected");
        fixture.cancel.take().unwrap().send(()).unwrap();
        assert_eq!(
            (&mut fixture.task).await.unwrap().unwrap(),
            StopReason::Disconnected
        );
    }

    async fn http(
        address: std::net::SocketAddr,
        path: &str,
        origin: Option<&str>,
    ) -> (String, String) {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let origin = origin
            .map(|s| format!("Origin: {s}\r\n"))
            .unwrap_or_default();
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:17890\r\n{origin}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
        let headers = read_headers(&mut socket).await.unwrap();
        let length: usize = headers
            .lines()
            .find_map(|s| {
                s.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap();
        let mut bytes = vec![0; length];
        socket.read_exact(&mut bytes).await.unwrap();
        (headers, String::from_utf8(bytes).unwrap())
    }

    #[tokio::test]
    async fn browser_request_reconnects_broken_transport_and_explicit_disconnect_removes_route() {
        timeout(Duration::from_secs(15), async {
            let routes = Routes::default();
            let mut fixture = TestTunnel::new_with_routes(Some(routes.clone())).await;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel();
            let gateway = tokio::spawn(crate::gateway::run(listener, routes.clone(), stopped));
            let path = format!("/s/{}/probe", fixture.id);
            assert!(http(address, &path, None).await.0.contains("200 OK"));
            fixture.cut.send(()).await.unwrap();
            loop {
                if let Ok(Event::WaitingForReconnect(_)) = fixture.events.try_recv() {
                    break;
                }
                assert!(!fixture.task.is_finished());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(routes.read().unwrap().contains_key(&fixture.id));
            assert!(
                http(address, &path, Some("http://evil.example"))
                    .await
                    .0
                    .contains("403")
            );
            // The same URL triggers a new authenticated transport, without client UI actions.
            let mut ws = tokio::net::TcpStream::connect(address).await.unwrap();
            ws.write_all(format!("GET /s/{}/ws HTTP/1.1\r\nHost: 127.0.0.1:17890\r\nOrigin: http://127.0.0.1:17890\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n", fixture.id).as_bytes()).await.unwrap();
            assert!(read_headers(&mut ws).await.unwrap().contains("101"));
            ws.write_all(b"browser-reconnected").await.unwrap();
            let mut bytes = [0; 19];
            ws.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"browser-reconnected");
            let response = http(address, &path, None).await;
            assert!(response.0.contains("200 OK"));
            assert!(response.1.starts_with(&fixture.ssh_port.to_string()));
            fixture.cancel.take().unwrap().send(()).unwrap();
            assert_eq!(
                (&mut fixture.task).await.unwrap().unwrap(),
                StopReason::Disconnected
            );
            assert!(http(address, &path, None).await.0.contains("409"));
            stop.send(()).unwrap();
            gateway.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn shared_gateway_isolates_servers_and_keeps_running_after_one_disconnects() {
        timeout(Duration::from_secs(15), async {
            let routes = Routes::default();
            let mut first = TestTunnel::new_with_routes(Some(routes.clone())).await;
            let mut second = TestTunnel::new_with_routes(Some(routes.clone())).await;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel();
            let gateway = tokio::spawn(crate::gateway::run(listener, routes.clone(), stopped));
            let first_path = format!("/s/{}/probe?file=first", first.id);
            let second_path = format!("/s/{}/probe?file=second", second.id);
            let (a, b) = tokio::join!(http(address, &first_path, None), http(address, &second_path, None));
            assert!(a.0.contains("200 OK")); assert!(a.1.starts_with(&first.ssh_port.to_string())); assert!(a.1.contains("GET /probe?file=first HTTP/1.1"));
            assert!(b.1.starts_with(&second.ssh_port.to_string()));
            assert!(http(address, &first_path, Some("http://evil.example")).await.0.contains("403"));
            assert!(http(address, "/s/invalid/probe", None).await.0.contains("404"));
            let mut ws = tokio::net::TcpStream::connect(address).await.unwrap();
            ws.write_all(format!("GET /s/{}/ws HTTP/1.1\r\nHost: 127.0.0.1:17890\r\nOrigin: http://127.0.0.1:17890\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n", second.id).as_bytes()).await.unwrap();
            assert!(read_headers(&mut ws).await.unwrap().contains("101"));
            ws.write_all(b"server-two").await.unwrap(); let mut bytes = [0; 10]; ws.read_exact(&mut bytes).await.unwrap(); assert_eq!(&bytes, b"server-two");
            first.cancel.take().unwrap().send(()).unwrap(); (&mut first.task).await.unwrap().unwrap();
            assert!(http(address, &first_path, None).await.0.contains("409"));
            assert!(http(address, &second_path, None).await.1.starts_with(&second.ssh_port.to_string()));
            ws.write_all(b"still-live").await.unwrap(); ws.read_exact(&mut bytes).await.unwrap(); assert_eq!(&bytes, b"still-live");
            assert_eq!(routes.read().unwrap().len(), 1);
            second.cancel.take().unwrap().send(()).unwrap(); (&mut second.task).await.unwrap().unwrap();
            assert_eq!(ws.read(&mut [0]).await.unwrap(), 0);
            assert!(routes.read().unwrap().is_empty());
            assert!(http(address, &second_path, None).await.0.contains("409"));
            stop.send(()).unwrap(); gateway.await.unwrap().unwrap();
        }).await.unwrap();
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
