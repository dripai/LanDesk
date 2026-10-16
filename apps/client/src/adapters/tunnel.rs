use crate::adapters::gateway::Registration;
use crate::adapters::gateway::Routes;
use crate::adapters::terminal;
use crate::adapters::viewer;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use landesk_core::domain::connection::Authentication;
use landesk_core::domain::connection::ConnectionMode;
use landesk_core::domain::connection::ConnectionProfile;
use landesk_core::domain::connection::{Event, StopReason};
use russh::Disconnect;
use russh::client;
use russh::keys;
use russh::keys::PublicKeyOrCertificate;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::timeout;
use zeroize::Zeroizing;

async fn desktop_channel(session: &client::Handle<Handler>) -> Result<russh::Channel<client::Msg>> {
    timeout(Duration::from_secs(10), async {
        let mut channel = session.channel_open_session().await?;
        channel
            .request_subsystem(true, landesk_core::protocol::desktop::SUBSYSTEM)
            .await?;
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => Ok(channel),
            _ => bail!("服务器拒绝 LanDesk 子系统或桌面服务未就绪"),
        }
    })
    .await
    .context("LanDesk 子系统连接超时")?
}

pub(crate) struct Handler;
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, server: &PublicKeyOrCertificate) -> Result<bool> {
        ensure!(
            server.certificate().is_none(),
            "暂不支持 SSH 主机证书，请使用普通主机密钥"
        );
        // Product policy: encrypted SSH with user authentication, without host
        // identity pinning. Never read or update a known_hosts file.
        Ok(true)
    }
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
    settings: ConnectionProfile,
    password: Zeroizing<String>,
    events: mpsc::Sender<Event>,
    cancel: oneshot::Receiver<()>,
    routes: Routes,
) -> Result<StopReason> {
    settings.validate()?;
    if settings.authentication == Authentication::Password {
        ensure!(!password.is_empty(), "请输入 SSH 密码");
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    let registration = Registration::new(settings.id(), routes, sender)?;
    run_bound(
        settings,
        password,
        events,
        cancel,
        Incoming::Routed {
            receiver,
            registration,
        },
    )
    .await
}

async fn run_bound(
    settings: ConnectionProfile,
    password: Zeroizing<String>,
    events: mpsc::Sender<Event>,
    mut cancel: oneshot::Receiver<()>,
    mut incoming: Incoming,
) -> Result<StopReason> {
    events.send(Event::Status("正在连接 SSH…".into()))?;
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 2,
        ..Default::default()
    });
    let handler = Handler;
    let mut session = Arc::new(tokio::select! {
        _ = &mut cancel => return Ok(StopReason::Disconnected),
        result = timeout(Duration::from_secs(20), client::connect(config, (settings.host.as_str(), settings.ssh_port), handler)) => result.context("SSH 连接超时")??,
    });
    let result = tokio::select! {
        _ = &mut cancel => Ok(StopReason::Disconnected),
        result = async {
            events.send(Event::Status("正在验证 SSH 用户…".into()))?;
            let handle = Arc::get_mut(&mut session).context("SSH 认证期间会话已被占用")?;
            timeout(Duration::from_secs(20), authenticate(handle, &settings, password)).await.context("SSH 认证超时")??;
            if settings.mode == ConnectionMode::Desktop {
                let probe = desktop_channel(&session).await?;
                probe.close().await?;
            }
            let terminal = if settings.mode == ConnectionMode::Terminal {
                Some(Arc::new(terminal::Page::new()?))
            } else { None };
            let (activity, mut activity_changes) = landesk_core::application::viewer::activity();
            incoming.activate()?;
            events.send(Event::Ready)?;
            let mut transfers = JoinSet::new();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                let deadline = activity_changes.borrow_and_update().deadline();
                tokio::select! {
                    _ = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // Recheck after waking: a new viewer may have arrived at the deadline.
                        if activity_changes.borrow().deadline().is_some_and(|end| end <= tokio::time::Instant::now()) {
                            break Ok(StopReason::IdleTimeout);
                        }
                    }
                    changed = activity_changes.changed() => {
                        changed.context("远控连接状态通道已关闭")?;
                    }
                    _ = tick.tick() => { ensure!(!session.is_closed(), "SSH 连接已断开"); }
                    result = transfers.join_next(), if !transfers.is_empty() => {
                        if let Some(Err(error)) = result { bail!("隧道任务异常：{error}"); }
                        if let Some(Ok(Err(error))) = result {
                            events.send(Event::Status(format!("网页连接已结束：{error:#}")))?;
                        }
                    }
                    accepted = incoming.accept(), if transfers.len() < 32 => {
                        let socket = accepted?;
                        let session = session.clone();
                        let activity = activity.clone();
                        let terminal = terminal.clone();
                        transfers.spawn(async move {
                            if let Some(page) = terminal {
                                return terminal::serve(socket, session, activity, page).await;
                            }
                            let channel = desktop_channel(&session).await?;
                            viewer::forward(socket, channel.into_stream(), activity).await
                        });
                    }
                }
            }
        } => result,
    };
    drop(incoming);
    // Dropping the transfer JoinSet cancels every local socket before SSH is closed.
    let _ = timeout(
        Duration::from_secs(3),
        session.disconnect(Disconnect::ByApplication, "LanDeskClient disconnected", ""),
    )
    .await;
    result
}

async fn authenticate(
    session: &mut client::Handle<Handler>,
    settings: &ConnectionProfile,
    secret: Zeroizing<String>,
) -> Result<()> {
    let auth = match &settings.authentication {
        Authentication::Password => {
            session
                .authenticate_password(settings.ssh_user(), secret.to_string())
                .await?
        }
        Authentication::PrivateKey { path } => {
            ensure!(
                settings.mode == landesk_core::domain::connection::ConnectionMode::Terminal,
                "新版 LanDesk 桌面使用连接密码，请编辑旧连接配置"
            );
            let path = path.clone();
            let key = tokio::task::spawn_blocking(move || {
                // Keep both the file contents and passphrase out of logs and config files.
                let encoded =
                    Zeroizing::new(std::fs::read_to_string(path).context("无法读取私钥文件")?);
                keys::decode_secret_key(&encoded, (!secret.is_empty()).then_some(secret.as_str()))
                    .context("无法解析或解密私钥，请检查文件和私钥口令")
            })
            .await
            .context("私钥加载任务失败")??;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            session
                .authenticate_publickey(
                    settings.ssh_user(),
                    keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await?
        }
    };
    ensure!(
        auth.success(),
        "连接认证失败，请检查连接密码或 SSH 登录设置"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::Algorithm;
    use russh::keys::PrivateKey;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    struct EchoServer(
        u16,
        std::collections::HashMap<russh::ChannelId, russh::Channel<russh::server::Msg>>,
    );
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
        async fn channel_open_session(
            &mut self,
            channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<()> {
            self.1.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }
        async fn subsystem_request(
            &mut self,
            id: russh::ChannelId,
            name: &str,
            session: &mut russh::server::Session,
        ) -> Result<()> {
            ensure!(name == "landesk", "unexpected subsystem");
            let channel = self.1.remove(&id).unwrap();
            session.channel_success(id)?;
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
        address: std::net::SocketAddr,
        id: String,
        ssh_port: u16,
        cancel: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<Result<StopReason>>,
        server: tokio::task::JoinHandle<()>,
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
            let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            let ssh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let ssh_port = ssh_listener.local_addr().unwrap().port();
            let config = Arc::new(russh::server::Config {
                keys: vec![key],
                ..Default::default()
            });
            let server = tokio::spawn(async move {
                let (socket, _) = ssh_listener.accept().await.unwrap();
                let _ = russh::server::run_stream(
                    config,
                    socket,
                    EchoServer(ssh_port, Default::default()),
                )
                .await
                .unwrap()
                .await;
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let settings = ConnectionProfile {
                host: "127.0.0.1".into(),
                user: "tester".into(),
                ssh_port,
                ..ConnectionProfile::default()
            };
            let (events, receiver) = mpsc::channel();
            let (cancel, cancelled) = oneshot::channel();
            let id = settings.id();
            let task = if let Some(routes) = routes {
                tokio::spawn(run(
                    settings,
                    Zeroizing::new("test-only".into()),
                    events,
                    cancelled,
                    routes,
                ))
            } else {
                tokio::spawn(run_bound(
                    settings,
                    Zeroizing::new("test-only".into()),
                    events,
                    cancelled,
                    Incoming::Local(listener),
                ))
            };
            let fixture = Self {
                address,
                id,
                ssh_port,
                cancel: Some(cancel),
                task,
                server,
            };
            timeout(Duration::from_secs(10), async {
                loop {
                    if let Ok(Event::Ready) = receiver.try_recv() {
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
                .write_all(b"GET / HTTP/1.1\r\nHost: landesk.internal\r\n\r\n")
                .await
                .unwrap();
            assert!(read_headers(&mut socket).await.unwrap().contains("200 OK"));
            socket.write_all(b"GET /ws HTTP/1.1\r\nHost: landesk.internal\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nOrigin: http://landesk.internal\r\n\r\n").await.unwrap();
            assert!(
                read_headers(&mut socket)
                    .await
                    .unwrap()
                    .contains("101 Switching Protocols")
            );
            socket
        }
        async fn assert_closed(&mut self) {
            assert_eq!(
                timeout(Duration::from_secs(35), &mut self.task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                StopReason::IdleTimeout
            );
            assert!(tokio::net::TcpStream::connect(self.address).await.is_err());
            let _listener = TcpListener::bind(self.address).await.unwrap();
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
    async fn http_keep_alive_and_failed_upgrades_do_not_prevent_idle_shutdown() {
        let mut fixture = TestTunnel::new().await;
        let mut socket = tokio::net::TcpStream::connect(fixture.address)
            .await
            .unwrap();
        socket.write_all(b"GET / HTTP/1.1\r\nHost: landesk.internal\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").await.unwrap();
        assert!(read_headers(&mut socket).await.unwrap().contains("200 OK"));
        fixture.assert_closed().await;
        assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn active_viewer_survives_idle_limit_and_last_close_stops_tunnel() {
        let mut fixture = TestTunnel::new().await;
        let first = fixture.viewer().await;
        let mut second = fixture.viewer().await;
        drop(first);
        tokio::time::sleep(
            landesk_core::application::viewer::IDLE_TIMEOUT + Duration::from_secs(1),
        )
        .await;
        assert!(
            !fixture.task.is_finished(),
            "another viewer must keep tunnel alive"
        );
        second.write_all(b"still connected").await.unwrap();
        let mut bytes = [0; 15];
        second.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"still connected");
        drop(second);
        // A refresh within the grace period restores the same tunnel.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let refreshed = fixture.viewer().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!fixture.task.is_finished());
        drop(refreshed);
        fixture.assert_closed().await;
    }

    async fn http(
        address: std::net::SocketAddr,
        path: &str,
        origin: Option<&str>,
        cookie: &str,
    ) -> (String, String) {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let origin = origin
            .map(|s| format!("Origin: {s}\r\n"))
            .unwrap_or_default();
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nCookie: {cookie}\r\n{origin}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
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
    async fn shared_gateway_isolates_servers_and_keeps_running_after_one_disconnects() {
        timeout(Duration::from_secs(15), async {
            let routes = Routes::default();
            let mut first = TestTunnel::new_with_routes(Some(routes.clone())).await;
            let mut second = TestTunnel::new_with_routes(Some(routes.clone())).await;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel();
            let gateway = tokio::spawn(crate::adapters::gateway::run(listener, routes.clone(), stopped));
            let first_cookie = crate::adapters::gateway::test_cookie(&routes, &first.id, address.port());
            let second_cookie = crate::adapters::gateway::test_cookie(&routes, &second.id, address.port());
            let first_path = format!("/s/{}/probe?file=first", first.id);
            let second_path = format!("/s/{}/probe?file=second", second.id);
            let (a, b) = tokio::join!(http(address, &first_path, None, &first_cookie), http(address, &second_path, None, &second_cookie));
            assert!(a.0.contains("200 OK")); assert!(a.1.starts_with(&first.ssh_port.to_string())); assert!(a.1.contains("GET /probe?file=first HTTP/1.1"));
            assert!(b.1.starts_with(&second.ssh_port.to_string()));
            assert!(http(address, &first_path, Some("http://evil.example"), &first_cookie).await.0.contains("403"));
            assert!(http(address, "/s/invalid/probe", None, "").await.0.contains("404"));
            let mut ws = tokio::net::TcpStream::connect(address).await.unwrap();
            ws.write_all(format!("GET /s/{}/ws HTTP/1.1\r\nHost: {address}\r\nCookie: {second_cookie}\r\nOrigin: http://{address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n", second.id).as_bytes()).await.unwrap();
            assert!(read_headers(&mut ws).await.unwrap().contains("101"));
            ws.write_all(b"server-two").await.unwrap(); let mut bytes = [0; 10]; ws.read_exact(&mut bytes).await.unwrap(); assert_eq!(&bytes, b"server-two");
            first.cancel.take().unwrap().send(()).unwrap(); (&mut first.task).await.unwrap().unwrap();
            assert!(http(address, &first_path, None, &first_cookie).await.0.contains("409"));
            assert!(http(address, &second_path, None, &second_cookie).await.1.starts_with(&second.ssh_port.to_string()));
            ws.write_all(b"still-live").await.unwrap(); ws.read_exact(&mut bytes).await.unwrap(); assert_eq!(&bytes, b"still-live");
            assert_eq!(routes.read().unwrap().len(), 1);
            second.cancel.take().unwrap().send(()).unwrap(); (&mut second.task).await.unwrap().unwrap();
            assert_eq!(ws.read(&mut [0]).await.unwrap(), 0);
            assert!(routes.read().unwrap().is_empty());
            assert!(http(address, &second_path, None, &second_cookie).await.0.contains("409"));
            stop.send(()).unwrap(); gateway.await.unwrap().unwrap();
        }).await.unwrap();
    }

    #[tokio::test]
    async fn changed_host_keys_connect_but_wrong_password_is_rejected() {
        timeout(Duration::from_secs(15), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let first = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            let second = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            assert_ne!(first.public_key(), second.public_key());
            let keys = [first, second.clone(), second];
            let server = tokio::spawn(async move {
                let mut sessions = JoinSet::new();
                for key in keys {
                    let (socket, _) = listener.accept().await.unwrap();
                    let config = Arc::new(russh::server::Config {
                        keys: vec![key],
                        auth_rejection_time: Duration::ZERO,
                        ..Default::default()
                    });
                    sessions.spawn(async move {
                        let session = russh::server::run_stream(
                            config,
                            socket,
                            EchoServer(0, Default::default()),
                        )
                        .await
                        .unwrap();
                        let _ = session.await;
                    });
                }
                while sessions.join_next().await.is_some() {}
            });
            let profile = ConnectionProfile {
                host: address.ip().to_string(),
                ssh_port: address.port(),
                ..Default::default()
            };
            for (password, accepted) in [("test-only", true), ("test-only", true), ("wrong", false)]
            {
                let mut session =
                    client::connect(Arc::new(client::Config::default()), address, Handler)
                        .await
                        .unwrap();
                let result =
                    authenticate(&mut session, &profile, Zeroizing::new(password.into())).await;
                assert_eq!(result.is_ok(), accepted);
                session
                    .disconnect(Disconnect::ByApplication, "test complete", "en")
                    .await
                    .unwrap();
                drop(session);
            }
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}
