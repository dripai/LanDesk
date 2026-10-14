use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use landesk_client::{
    gateway,
    settings::{Authentication, ConnectionMode, Settings},
    tunnel,
};
use russh::{
    Channel, ChannelId, Pty,
    keys::{self, Algorithm, PrivateKey, PublicKey},
    server,
};
use std::sync::{Arc, Mutex, mpsc};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use zeroize::Zeroizing;

#[derive(Default)]
struct Observed {
    ptys: usize,
    shells: usize,
    closed: usize,
    size: (u32, u32),
}
struct SshServer {
    observed: Arc<Mutex<Observed>>,
    pty: u8,
    key: PublicKey,
}
impl server::Handler for SshServer {
    type Error = anyhow::Error;
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<server::Auth> {
        Ok(if user == "tester" && password == "test-only" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<server::Auth> {
        Ok(if user == "tester" && key == &self.key {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut server::Session,
    ) -> Result<()> {
        assert_eq!(term, "xterm-256color");
        let mut observed = self.observed.lock().unwrap();
        observed.ptys += 1;
        observed.size = (cols, rows);
        match self.pty {
            0 => session.channel_success(channel)?,
            1 => session.channel_failure(channel)?,
            _ => {}
        }
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<()> {
        self.observed.lock().unwrap().shells += 1;
        session.channel_success(channel)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<()> {
        assert_eq!(name, "sftp");
        session.channel_failure(channel)?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut server::Session,
    ) -> Result<()> {
        self.observed.lock().unwrap().size = (cols, rows);
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<()> {
        if data == b"__exit__" {
            session.exit_status_request(channel, 12)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else if data == b"__burst__" {
            for _ in 0..32 {
                session.data(channel, vec![b'x'; 16 * 1024])?;
            }
        } else {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }
    async fn channel_close(&mut self, _: ChannelId, _: &mut server::Session) -> Result<()> {
        self.observed.lock().unwrap().closed += 1;
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    _events: mpsc::Receiver<tunnel::Event>,
    addr: std::net::SocketAddr,
    id: String,
    observed: Arc<Mutex<Observed>>,
    cancel: Option<oneshot::Sender<()>>,
    stop: Option<oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new(pty: u8, encrypted_key: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let known = dir.path().join("known_hosts");
        let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let login_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let public_key = login_key.public_key().clone();
        let ssh_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ssh_port = ssh_listener.local_addr().unwrap().port();
        keys::known_hosts::learn_known_hosts_path(
            "127.0.0.1",
            ssh_port,
            host_key.public_key(),
            &known,
        )
        .unwrap();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let server = SshServer {
            observed: observed.clone(),
            pty,
            key: public_key,
        };
        let server_task = tokio::spawn(async move {
            let (socket, _) = ssh_listener.accept().await.unwrap();
            let config = Arc::new(server::Config {
                keys: vec![host_key],
                ..Default::default()
            });
            let _ = server::run_stream(config, socket, server)
                .await
                .unwrap()
                .await;
        });
        let mut profile = Settings {
            host: "127.0.0.1".into(),
            ssh_port,
            user: "tester".into(),
            mode: ConnectionMode::Terminal,
            ..Default::default()
        };
        if encrypted_key {
            let path = dir.path().join("id_ed25519");
            let key = login_key.encrypt(&mut rand::rng(), "test-only").unwrap();
            std::fs::write(
                &path,
                key.to_openssh(keys::ssh_key::LineEnding::LF)
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
            profile.authentication = Authentication::PrivateKey { path };
        }
        let routes = gateway::Routes::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let gateway_routes = routes.clone();
        let gateway_task = tokio::spawn(async move {
            gateway::run(listener, gateway_routes, stopped)
                .await
                .unwrap();
        });
        let (cancel, cancelled) = oneshot::channel();
        let (tx, rx) = mpsc::channel();
        let id = profile.id();
        let client_task = tokio::spawn(async move {
            let result = tunnel::run(
                profile,
                Zeroizing::new("test-only".into()),
                known,
                tx.clone(),
                cancelled,
                routes,
            )
            .await;
            let _ = tx.send(tunnel::Event::Stopped(result));
        });
        timeout(Duration::from_secs(15), async {
            loop {
                match rx.try_recv() {
                    Ok(tunnel::Event::Ready) => break,
                    Ok(tunnel::Event::Stopped(result)) => panic!("connection failed: {result:?}"),
                    _ => tokio::time::sleep(Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            _events: rx,
            addr,
            id,
            observed,
            cancel: Some(cancel),
            stop: Some(stop),
            tasks: vec![server_task, gateway_task, client_task],
        }
    }
    async fn token(&self) -> String {
        let socket = TcpStream::connect(self.addr).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake::<
            _,
            http_body_util::Empty<hyper::body::Bytes>,
        >(TokioIo::new(socket))
        .await
        .unwrap();
        let task = tokio::spawn(connection);
        let request = hyper::Request::builder()
            .uri(format!("/s/{}/", self.id))
            .header("host", "127.0.0.1:17890")
            .body(http_body_util::Empty::new())
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        let token = html
            .split("name=\"session-token\" content=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        task.abort();
        token
    }
    fn request(&self, origin: &str) -> hyper::Request<()> {
        let mut request = format!("ws://{}/s/{}/ws", self.addr, self.id)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("host", "127.0.0.1:17890".parse().unwrap());
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        request
    }
    async fn socket(&self, token: &str) -> Ws {
        let (mut ws, _) = connect_async(self.request("http://127.0.0.1:17890"))
            .await
            .unwrap();
        ws.send(Message::Text(
            serde_json::json!({"type":"hello","token":token,"cols":80,"rows":24})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        ws
    }
    async fn observed(&self, predicate: impl Fn(&Observed) -> bool) {
        timeout(Duration::from_secs(3), async {
            while !predicate(&self.observed.lock().unwrap()) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
async fn json(ws: &mut Ws) -> serde_json::Value {
    let message = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn echo(ws: &mut Ws, input: &[u8]) {
    ws.send(Message::Binary(input.to_vec().into()))
        .await
        .unwrap();
    let message = timeout(Duration::from_secs(3), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(message.into_data().as_ref(), input);
    ws.send(Message::Text(
        serde_json::json!({"type":"ack","bytes":input.len()})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn password_terminal_transfers_utf8_control_bytes_resize_exit_and_isolates_tabs() {
    let fixture = Fixture::new(0, false).await;
    let token = fixture.token().await;
    let mut a = fixture.socket(&token).await;
    assert_eq!(json(&mut a).await["type"], "ready");
    echo(&mut a, "中文🙂\r\n".as_bytes()).await;
    echo(&mut a, b"\x03\x1b[A\t").await;
    a.send(Message::Text(
        r#"{"type":"resize","cols":120,"rows":40}"#.into(),
    ))
    .await
    .unwrap();
    fixture.observed(|o| o.size == (120, 40)).await;
    let mut b = fixture.socket(&token).await;
    assert_eq!(json(&mut b).await["type"], "ready");
    a.close(None).await.unwrap();
    fixture.observed(|o| o.closed >= 1).await;
    echo(&mut b, b"second terminal stays alive").await;
    b.send(Message::Binary(b"__exit__".to_vec().into()))
        .await
        .unwrap();
    let exit = json(&mut b).await;
    assert_eq!(exit["type"], "exit");
    assert_eq!(exit["code"], 12);
}

#[tokio::test]
async fn encrypted_private_key_authentication_opens_a_terminal() {
    let fixture = Fixture::new(0, true).await;
    let mut ws = fixture.socket(&fixture.token().await).await;
    assert_eq!(json(&mut ws).await["type"], "ready");
    echo(&mut ws, b"authenticated with an encrypted key").await;
}

#[tokio::test]
async fn origin_and_session_token_are_checked_before_any_pty_is_opened() {
    let fixture = Fixture::new(0, false).await;
    let err = connect_async(fixture.request("https://evil.example"))
        .await
        .unwrap_err();
    assert!(
        matches!(err,tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 403)
    );
    let mut ws = fixture.socket("wrong-token").await;
    assert_eq!(json(&mut ws).await["type"], "error");
    assert_eq!(fixture.observed.lock().unwrap().ptys, 0);
}

#[tokio::test]
async fn denied_pty_is_an_explicit_error_without_starting_shell() {
    let fixture = Fixture::new(1, false).await;
    let mut ws = fixture.socket(&fixture.token().await).await;
    let error = json(&mut ws).await;
    assert_eq!(error["type"], "error");
    assert!(error["message"].as_str().unwrap().contains("PTY"));
    fixture.observed(|o| o.closed == 1).await;
    assert_eq!(fixture.observed.lock().unwrap().shells, 0);
}

#[tokio::test]
async fn closing_browser_during_pty_initialization_closes_ssh_channel() {
    let fixture = Fixture::new(2, false).await;
    let mut ws = fixture.socket(&fixture.token().await).await;
    fixture.observed(|o| o.ptys == 1).await;
    ws.close(None).await.unwrap();
    fixture.observed(|o| o.closed == 1).await;
}

#[tokio::test]
async fn slow_renderer_pauses_output_and_acknowledgements_resume_without_loss() {
    let fixture = Fixture::new(0, false).await;
    let mut ws = fixture.socket(&fixture.token().await).await;
    assert_eq!(json(&mut ws).await["type"], "ready");
    ws.send(Message::Binary(b"__burst__".to_vec().into()))
        .await
        .unwrap();
    let mut received = 0;
    while received < 128 * 1024 {
        let frame = timeout(Duration::from_secs(3), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Binary(data) = frame else {
            panic!("expected terminal output")
        };
        assert!(data.iter().all(|b| *b == b'x'));
        received += data.len();
    }
    // The remote has sent 512 KiB, but no more is delivered until rendering is acknowledged.
    assert!(
        timeout(Duration::from_millis(100), ws.next())
            .await
            .is_err()
    );
    ws.send(Message::Text(
        serde_json::json!({"type":"ack", "bytes":received})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    while received < 512 * 1024 {
        let frame = timeout(Duration::from_secs(3), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Binary(data) = frame else {
            panic!("expected terminal output")
        };
        assert!(data.iter().all(|b| *b == b'x'));
        received += data.len();
        ws.send(Message::Text(
            serde_json::json!({"type":"ack", "bytes":data.len()})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    assert_eq!(received, 512 * 1024);
    echo(&mut ws, b"still responsive").await;
}

#[tokio::test]
async fn sftp_checks_origin_token_and_rejection_does_not_close_terminal() {
    let fixture = Fixture::new(0, false).await;
    let token = fixture.token().await;
    let mut terminal = fixture.socket(&token).await;
    assert_eq!(json(&mut terminal).await["type"], "ready");
    let request = |origin: &str| {
        let mut request = fixture.request(origin);
        *request.uri_mut() = format!("ws://{}/s/{}/files-ws", fixture.addr, fixture.id)
            .parse()
            .unwrap();
        request
    };
    assert!(
        connect_async(request("http://untrusted.invalid"))
            .await
            .is_err()
    );
    for credential in ["wrong-token", token.as_str()] {
        let (mut files, _) = connect_async(request("http://127.0.0.1:17890"))
            .await
            .unwrap();
        files
            .send(Message::Text(
                serde_json::json!({"type":"hello","token":credential})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let reply = json(&mut files).await;
        assert_eq!(reply["type"], "error");
        let message = reply["message"].as_str().unwrap();
        assert!(if credential == "wrong-token" {
            message.contains("凭据")
        } else {
            message.contains("SFTP")
        });
        echo(&mut terminal, b"still-running").await;
    }
}
