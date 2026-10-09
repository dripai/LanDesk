//! Built-in encrypted transport. No shell, OS account login or arbitrary forwarding.
use crate::{access, desktop::Shared};
use anyhow::{Context, Result};
use russh::{Channel, ChannelStream, Disconnect, MethodKind, server};
use std::{
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Snapshot {
    pub port: u16,
    pub password_set: bool,
    pub listening: bool,
    pub status: String,
}
#[derive(Clone)]
pub struct Control {
    state: Arc<Mutex<Snapshot>>,
    sender: mpsc::Sender<Apply>,
}
struct Apply {
    port: u16,
    password: Zeroizing<String>,
    reply: oneshot::Sender<Result<()>>,
}
impl Control {
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().expect("connection state").clone()
    }
    pub fn apply(&self, port: u16, password: Zeroizing<String>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.sender
            .blocking_send(Apply {
                port,
                password,
                reply,
            })
            .context("连接服务已关闭")?;
        result.blocking_recv().context("连接服务已关闭")?
    }
}

pub struct Accepted {
    stream: ChannelStream<server::Msg>,
    _permit: OwnedSemaphorePermit,
}
impl AsyncRead for Accepted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for Accepted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
pub struct Listener(mpsc::Receiver<(Accepted, SocketAddr)>);
impl axum::serve::Listener for Listener {
    type Io = Accepted;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Accepted, SocketAddr) {
        match self.0.recv().await {
            Some(io) => io,
            None => std::future::pending().await,
        }
    }
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(([127, 0, 0, 1], 0).into())
    }
}

struct Handler {
    hash: String,
    authenticated: Arc<AtomicBool>,
    incoming: mpsc::Sender<(Accepted, SocketAddr)>,
    peer: SocketAddr,
    channels: Arc<Semaphore>,
    passwords: Arc<Semaphore>,
}
fn reject_password() -> server::Auth {
    // Keep password authentication available until russh's attempt cap is reached.
    server::Auth::Reject {
        proceed_with_methods: Some((&[MethodKind::Password][..]).into()),
        partial_success: false,
    }
}
impl server::Handler for Handler {
    type Error = anyhow::Error;
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<server::Auth> {
        if user != access::USER || password.len() > 256 {
            return Ok(reject_password());
        }
        let Ok(permit) = self.passwords.clone().try_acquire_owned() else {
            return Ok(reject_password());
        };
        let hash = self.hash.clone();
        let password = Zeroizing::new(password.to_owned());
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            access::verify(&hash, &password)
        })
        .await?;
        self.authenticated.store(valid, Ordering::Release);
        Ok(if valid {
            server::Auth::Accept
        } else {
            reject_password()
        })
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        if !self.authenticated.load(Ordering::Acquire)
            || host != "127.0.0.1"
            || port != access::CHANNEL_PORT
        {
            return Ok(());
        }
        let Ok(permit) = self.channels.clone().try_acquire_owned() else {
            return Ok(());
        };
        let Ok(slot) = self.incoming.try_reserve() else {
            return Ok(());
        };
        reply.accept().await;
        slot.send((
            Accepted {
                stream: channel.into_stream(),
                _permit: permit,
            },
            self.peer,
        ));
        Ok(())
    }
}

fn bind(port: u16) -> Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_only_v6(false)?;
    socket.set_nonblocking(true)?;
    socket
        .bind(&SocketAddr::from(([0u16; 8], port)).into())
        .with_context(|| format!("连接端口 {port} 不可用"))?;
    socket.listen(128)?;
    Ok(TcpListener::from_std(socket.into())?)
}
fn protocol_config(settings: &access::Settings) -> Result<Arc<server::Config>> {
    Ok(Arc::new(server::Config {
        keys: vec![settings.key()?],
        methods: (&[MethodKind::Password][..]).into(),
        max_auth_attempts: 3,
        auth_rejection_time: Duration::from_secs(2),
        inactivity_timeout: Some(Duration::from_secs(60)),
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 2,
        nodelay: true,
        ..Default::default()
    }))
}

pub fn start(
    settings: access::Settings,
    path: PathBuf,
    shared: Arc<Shared>,
) -> (Control, Listener, tokio::task::JoinHandle<Result<()>>) {
    let (sender, commands) = mpsc::channel(1);
    let (incoming, receiver) = mpsc::channel(64);
    let control = Control {
        state: Arc::new(Mutex::new(Snapshot {
            port: settings.port,
            password_set: settings.password_hash.is_some(),
            listening: false,
            status: "首次使用：设置访问密码后点击保存并启动".into(),
        })),
        sender,
    };
    let handle = tokio::spawn(run(
        settings,
        path,
        shared,
        control.state.clone(),
        commands,
        incoming,
    ));
    (control, Listener(receiver), handle)
}
async fn run(
    mut settings: access::Settings,
    path: PathBuf,
    shared: Arc<Shared>,
    state: Arc<Mutex<Snapshot>>,
    mut commands: mpsc::Receiver<Apply>,
    incoming: mpsc::Sender<(Accepted, SocketAddr)>,
) -> Result<()> {
    let mut listener = None;
    if settings.password_hash.is_some() {
        match bind(settings.port) {
            Ok(socket) => {
                listener = Some(socket);
                state.lock().unwrap().listening = true;
                state.lock().unwrap().status = format!("服务已启动 · 连接端口 {}", settings.port);
            }
            Err(error) => state.lock().unwrap().status = format!("{error:#}"),
        }
    }
    let mut config = protocol_config(&settings)?;
    let (generation, _) = watch::channel(0u64);
    let connections = Arc::new(Semaphore::new(32));
    let passwords = Arc::new(Semaphore::new(2));
    let mut tasks = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = tick.tick() => if shared.shutdown.load(Ordering::Acquire) { break; },
            Some(request) = commands.recv() => {
                let result = async {
                    let _guard = shared.begin_maintenance()?;
                    let old = settings.clone();
                    let next = tokio::task::spawn_blocking(move || old.update(request.port, &request.password)).await??;
                    let new_listener = if listener.is_none() || next.port != settings.port { Some(bind(next.port)?) } else { None };
                    let next_protocol = protocol_config(&next)?;
                    next.save(&path)?;
                    // No fallible state changes after persisting the complete configuration.
                    if let Some(socket) = new_listener { listener = Some(socket); }
                    config = next_protocol;
                    settings = next;
                    generation.send_modify(|value| *value += 1);
                    *state.lock().unwrap() = Snapshot { port: settings.port, password_set: true, listening: true, status: format!("服务已启动 · 连接端口 {}", settings.port) };
                    Ok(())
                }.await;
                let _ = request.reply.send(result);
            },
            accepted = async { match &listener { Some(listener) => listener.accept().await, None => std::future::pending().await } } => {
                let (socket, peer) = match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        listener = None;
                        generation.send_modify(|value| *value += 1);
                        let mut status = state.lock().unwrap();
                        status.listening = false;
                        status.status = format!("连接服务已停止，请重新保存启动：{error}");
                        continue;
                    }
                };
                let Ok(permit) = connections.clone().try_acquire_owned() else { drop(socket); continue; };
                let authenticated = Arc::new(AtomicBool::new(false));
                let handler = Handler { hash: settings.password_hash.clone().context("访问密码未配置")?, authenticated: authenticated.clone(), incoming: incoming.clone(), peer, channels: Arc::new(Semaphore::new(32)), passwords: passwords.clone() };
                let config = config.clone();
                let mut stop = generation.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    let mut session = tokio::select! {
                        result = tokio::time::timeout(Duration::from_secs(10), server::run_stream(config, socket, handler)) => result??,
                        _ = stop.changed() => return Ok::<_, anyhow::Error>(()),
                    };
                    tokio::select! {
                        result = &mut session => return result,
                        _ = stop.changed() => {},
                        _ = async { tokio::time::sleep(Duration::from_secs(20)).await; if authenticated.load(Ordering::Acquire) { std::future::pending::<()>().await; } } => {},
                    }
                    let _ = session.handle().disconnect(Disconnect::ByApplication, "LanDesk connection closed".into(), "".into()).await;
                    let _ = tokio::time::timeout(Duration::from_secs(2), session).await;
                    Ok(())
                });
            },
            Some(_result) = tasks.join_next(), if !tasks.is_empty() => {},
        }
    }
    generation.send_modify(|value| *value += 1);
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::WebSocketUpgrade, routing::get};
    use futures_util::{SinkExt, StreamExt};
    use russh::{client, keys::PublicKey};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::{client_async, tungstenite::Message};

    struct Pinned(PublicKey);
    impl client::Handler for Pinned {
        type Error = anyhow::Error;
        async fn check_server_key(
            &mut self,
            key: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool> {
            Ok(key.certificate().is_none() && key.public_key() == self.0)
        }
    }
    struct Fixture {
        _directory: tempfile::TempDir,
        path: PathBuf,
        control: Control,
        shared: Arc<Shared>,
        task: tokio::task::JoinHandle<Result<()>>,
        http: tokio::task::JoinHandle<()>,
        key: PublicKey,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.shared.shutdown.store(true, Ordering::Release);
            self.http.abort();
        }
    }
    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("server.json");
            let settings = access::Settings::load(&path).unwrap();
            let key = settings.key().unwrap().public_key().clone();
            let shared = Arc::new(Shared::default());
            let (control, listener, task) = start(settings, path.clone(), shared.clone());
            let router = Router::new()
                .route("/probe", get(|| async { "encrypted-only" }))
                .route(
                    "/ws",
                    get(|ws: WebSocketUpgrade| async {
                        ws.on_upgrade(|mut socket| async move {
                            while let Some(Ok(message)) = socket.next().await {
                                if socket.send(message).await.is_err() {
                                    break;
                                }
                            }
                        })
                    }),
                );
            let http = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            Self {
                _directory: directory,
                path,
                control,
                shared,
                task,
                http,
                key,
            }
        }
        async fn apply(&self, port: u16, password: &str) -> Result<()> {
            let control = self.control.clone();
            let password = Zeroizing::new(password.to_owned());
            tokio::task::spawn_blocking(move || control.apply(port, password)).await?
        }
        async fn connect(&self) -> client::Handle<Pinned> {
            client::connect(
                Arc::new(client::Config::default()),
                ("127.0.0.1", self.control.snapshot().port),
                Pinned(self.key.clone()),
            )
            .await
            .unwrap()
        }
        async fn shutdown(mut self) {
            self.shared.shutdown.store(true, Ordering::Release);
            tokio::time::timeout(Duration::from_secs(5), &mut self.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
    fn unused_port() -> u16 {
        bind(0).unwrap().local_addr().unwrap().port()
    }
    async fn authenticated(fixture: &Fixture, password: &str) -> client::Handle<Pinned> {
        let mut client = fixture.connect().await;
        assert!(
            client
                .authenticate_password(access::USER, password)
                .await
                .unwrap()
                .success()
        );
        client
    }
    async fn channel(client: &client::Handle<Pinned>) -> russh::Channel<client::Msg> {
        client
            .channel_open_direct_tcpip("127.0.0.1", access::CHANNEL_PORT, "127.0.0.1", 0)
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn requires_setup_and_authentication_then_carries_http_and_websocket_only() {
        tokio::time::timeout(Duration::from_secs(30), async {
            let fixture = Fixture::new().await;
            assert!(!fixture.control.snapshot().listening);
            assert!(!fixture.path.exists());
            let port = unused_port();
            assert!(fixture.apply(port, "").await.is_err());
            fixture.apply(port, "test-password-1").await.unwrap();
            let mut wrong = fixture.connect().await;
            assert!(
                !wrong
                    .authenticate_password(access::USER, "wrong")
                    .await
                    .unwrap()
                    .success()
            );
            assert!(
                !wrong
                    .authenticate_password("system-user", "test-password-1")
                    .await
                    .unwrap()
                    .success()
            );
            drop(wrong);
            let client = authenticated(&fixture, "test-password-1").await;
            assert!(client.channel_open_session().await.is_err());
            assert!(
                client
                    .channel_open_direct_tcpip("example.com", 443, "127.0.0.1", 0)
                    .await
                    .is_err()
            );
            assert!(
                client
                    .channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 0)
                    .await
                    .is_err()
            );
            let mut stream = channel(&client).await.into_stream();
            stream
                .write_all(
                    b"GET /probe HTTP/1.1\r\nHost: 127.0.0.1:17890\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"));
            assert!(response.ends_with("encrypted-only"));
            let (mut ws, _) = client_async(
                "ws://127.0.0.1:17890/ws",
                channel(&client).await.into_stream(),
            )
            .await
            .unwrap();
            let bytes = vec![42u8; 64 * 1024];
            ws.send(Message::Binary(bytes.clone().into()))
                .await
                .unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap().into_data(), bytes);
            ws.close(None).await.unwrap();
            fixture.shutdown().await;
            assert!(client.is_closed());
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn rotation_closes_old_connections_preserves_key_and_rejects_old_password() {
        tokio::time::timeout(Duration::from_secs(30), async {
            let fixture = Fixture::new().await;
            let port = unused_port();
            fixture.apply(port, "test-password-1").await.unwrap();
            let old = authenticated(&fixture, "test-password-1").await;
            fixture.apply(port, "test-password-2").await.unwrap();
            while !old.is_closed() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let mut next = fixture.connect().await;
            assert!(
                !next
                    .authenticate_password(access::USER, "test-password-1")
                    .await
                    .unwrap()
                    .success()
            );
            assert!(
                next.authenticate_password(access::USER, "test-password-2")
                    .await
                    .unwrap()
                    .success()
            );
            assert_eq!(
                access::Settings::load(&fixture.path)
                    .unwrap()
                    .key()
                    .unwrap()
                    .public_key(),
                &fixture.key
            );
            fixture.shutdown().await;
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn failed_bind_or_save_keeps_running_configuration_and_active_sessions_block_changes() {
        tokio::time::timeout(Duration::from_secs(30), async {
            let fixture = Fixture::new().await;
            let port = unused_port();
            fixture.apply(port, "test-password-1").await.unwrap();
            let before = std::fs::read(&fixture.path).unwrap();
            let occupied = bind(0).unwrap();
            assert!(
                fixture
                    .apply(occupied.local_addr().unwrap().port(), "test-password-2")
                    .await
                    .is_err()
            );
            assert_eq!(fixture.control.snapshot().port, port);
            assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
            fixture.shared.active.store(true, Ordering::Release);
            assert!(fixture.apply(port, "test-password-2").await.is_err());
            fixture.shared.active.store(false, Ordering::Release);
            std::fs::remove_file(&fixture.path).unwrap();
            std::fs::create_dir(&fixture.path).unwrap();
            assert!(
                fixture
                    .apply(unused_port(), "test-password-2")
                    .await
                    .is_err()
            );
            assert_eq!(fixture.control.snapshot().port, port);
            let client = authenticated(&fixture, "test-password-1").await;
            assert!(!client.is_closed());
            fixture.shutdown().await;
        })
        .await
        .unwrap();
    }
}
