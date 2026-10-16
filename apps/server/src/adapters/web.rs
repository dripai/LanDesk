use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use axum::Router;
use axum::extract::State;
use axum::extract::WebSocketUpgrade;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use landesk_core::application::capture::CaptureService;
use landesk_core::application::capture::Change;
use landesk_core::application::control::ControlSession;
use landesk_core::application::file_worker::FileChannel;
use landesk_core::application::file_worker::FileRequest;
use landesk_core::application::file_worker::FileService;
use landesk_core::application::runtime::Shared;
use landesk_core::domain::CaptureInfo;
use landesk_core::domain::FrameEvent;
use landesk_core::domain::InputEvent;
use landesk_core::ports::DesktopControl;
use landesk_core::protocol::ClientMessage;
use landesk_core::protocol::origin_allowed;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub transport_key: landesk_core::security::transport_key::TransportKey,
    pub native: std::sync::Arc<dyn DesktopControl>,
    pub shared: std::sync::Arc<Shared>,
    pub files: FileService,
    pub capture: CaptureService,
}

// Read independently of capture, AppKit replies and writes. Closing the browser
// must cancel a session even when one of those operations is still waiting.
struct SessionSocket {
    outgoing: tokio::sync::mpsc::Sender<Message>,
    incoming: tokio::sync::mpsc::Receiver<Message>,
    frames: tokio::sync::watch::Sender<Option<std::sync::Arc<Vec<u8>>>>,
    deferred: Option<Message>,
}
impl SessionSocket {
    async fn send(&mut self, message: Message) -> Result<()> {
        self.outgoing.send(message).await.context("网页连接已关闭")
    }
    async fn recv(&mut self) -> Option<Message> {
        let mut message = match self.deferred.take() {
            Some(message) => message,
            None => self.incoming.recv().await?,
        };
        fn pointer(message: &Message) -> bool {
            matches!(message, Message::Text(text) if matches!(serde_json::from_str(text), Ok(ClientMessage::Pointer { .. })))
        }
        if pointer(&message) {
            while let Ok(next) = self.incoming.try_recv() {
                if pointer(&next) {
                    message = next;
                } else {
                    self.deferred = Some(next);
                    break;
                }
            }
        }
        Some(message)
    }
}

fn monitor_connection(socket: WebSocket) -> (SessionSocket, impl Future<Output = Result<()>>) {
    let (mut writer, mut reader) = socket.split();
    // Backpressure bounds memory; it never disconnects a slow consumer.
    let (tx, incoming) = tokio::sync::mpsc::channel(32);
    let (outgoing, mut responses) = tokio::sync::mpsc::channel(8);
    let (frames, mut images) = tokio::sync::watch::channel::<Option<std::sync::Arc<Vec<u8>>>>(None);
    let monitor = async move {
        tokio::select! {
            result = async {
                while let Some(message) = reader.next().await {
                    let message = message?;
                    if let Message::Text(text) = &message {
                        match serde_json::from_str(text) {
                            Ok(ClientMessage::Disconnect) => return Ok(()),
                            Ok(ClientMessage::Heartbeat) => continue,
                            _ => {}
                        }
                    }
                    if matches!(message, Message::Close(_)) { return Ok(()); }
                    tx.send(message).await.context("会话接收已停止")?;
                }
                Ok::<_, anyhow::Error>(())
            } => result,
            result = async {
                loop {
                    tokio::select! {
                        biased;
                        response = responses.recv() => match response {
                            Some(response) => {
                                let closing = matches!(response, Message::Close(_));
                                writer.send(response).await?;
                                if closing { break; }
                            },
                            None => break,
                        },
                        changed = images.changed() => {
                            changed.context("画面发送已停止")?;
                            let image = images.borrow_and_update().clone();
                            if let Some(image) = image { writer.send(Message::Binary(image.as_ref().clone().into())).await?; }
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            } => result,
        }
    };
    (
        SessionSocket {
            outgoing,
            incoming,
            frames,
            deferred: None,
        },
        monitor,
    )
}

async fn wait_for_shutdown(shared: &Shared) {
    while !shared.shutdown.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn page_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; img-src 'self' blob:; connect-src 'self'; style-src 'self'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                page_headers(
                    Html(include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../web/index.html"
                    )))
                    .into_response(),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/app.js")),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/frames.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/frames.js")),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/keyboard.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../../web/keyboard.js"
                        )),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/clipboard.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../../web/clipboard.js"
                        )),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/files.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/files.js")),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/toolbar.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/toolbar.js")),
                    )
                        .into_response(),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/style.css")),
                    )
                        .into_response(),
                )
            }),
        )
        .route("/ws", get(upgrade))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            transport_auth,
        ))
        .with_state(state)
}

async fn transport_auth(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if state
        .transport_key
        .authorize(
            request
                .headers()
                .get(crate::adapters::ssh::AUTH_HEADER)
                .and_then(|v| v.to_str().ok()),
        )
        .is_err()
    {
        return (StatusCode::UNAUTHORIZED, "请通过 LanDeskClient 连接").into_response();
    }
    request
        .headers_mut()
        .remove(crate::adapters::ssh::AUTH_HEADER);
    next.run(request).await
}

async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !origin_allowed(origin, host) {
        return (StatusCode::FORBIDDEN, "只允许本机浏览器或 SSH 隧道连接").into_response();
    }
    upgrade
        .max_message_size(512 * 1024)
        .max_frame_size(512 * 1024)
        .on_upgrade(move |socket| async move {
            let (mut socket, closed) = monitor_connection(socket);
            tokio::pin!(closed);
            let shared = state.shared.clone();
            let (result, transport_done) = tokio::select! {
                result = session(&mut socket, state) => (result, false),
                result = &mut closed => (result, true),
                _ = wait_for_shutdown(&shared) => (Ok(()), false),
            };
            if let Err(e) = result {
                if e.downcast_ref::<serde_json::Error>().is_some() {
                    crate::diagnostics::log("LanDesk session rejected an invalid protocol message");
                } else {
                    crate::diagnostics::log(&format!("LanDesk session failed: {e:#}"));
                }
                if transport_done {
                    return;
                }
                // Let the transport flush the error and Close in order.
                // The session guard has already released input and ownership.
                tokio::select! {
                    _ = async {
                        let _ = socket.send(Message::Text(serde_json::json!({"type":"error","message":format!("{e:#}")}).to_string().into())).await;
                        let _ = socket.send(Message::Close(None)).await;
                        std::future::pending::<()>().await;
                    } => {},
                    _ = tokio::time::timeout(Duration::from_secs(2), &mut closed) => {},
                }
            }
        })
        .into_response()
}

async fn send_json(socket: &mut SessionSocket, value: impl serde::Serialize) -> Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(&value)?.into()))
        .await?;
    Ok(())
}

#[derive(Default)]
struct InputStatus {
    failure: Option<String>,
}
impl InputStatus {
    // Send transitions only: mouse movement must not flood the warning banner.
    fn observe(&mut self, result: Result<()>, can_recover: bool) -> Option<serde_json::Value> {
        match result {
            Err(error) => {
                let error = format!("{error:#}");
                if self.failure.is_some() {
                    return None;
                }
                crate::diagnostics::log(&format!(
                    "Input control unavailable; viewing continues: {error}"
                ));
                self.failure = Some(error.clone());
                Some(
                    serde_json::json!({"type":"input_state","available":false,"message":format!("键鼠控制暂不可用：{error}。画面保持连接，后续操作会继续尝试。")}),
                )
            }
            Ok(()) if can_recover && self.failure.take().is_some() => {
                crate::diagnostics::log("Input control recovered");
                Some(serde_json::json!({"type":"input_state","available":true}))
            }
            Ok(()) => None,
        }
    }
}
fn actionable_input(message: &ClientMessage) -> bool {
    match message {
        ClientMessage::ReleaseAll => false,
        ClientMessage::Text { text } | ClientMessage::PasteText { text } => !text.is_empty(),
        ClientMessage::Wheel { x, y } => *x != 0 || *y != 0,
        _ => true,
    }
}

async fn session(socket: &mut SessionSocket, state: AppState) -> Result<()> {
    let message = socket.recv().await.context("连接已关闭")?;
    let Message::Text(text) = message else {
        bail!("请先建立会话");
    };
    let ClientMessage::Hello { display } = serde_json::from_str(&text)? else {
        bail!("请先建立会话");
    };
    let guard = ControlSession::acquire(state.native.clone(), state.shared.clone())?;
    guard.authorize()?;
    crate::diagnostics::log("LanDesk session starting capture");
    let mut capture = state.capture.start(display).await?;
    let initial = capture.info.borrow().clone();
    let mut input_status = InputStatus::default();
    let input_warning =
        input_status.observe(state.native.begin_input(initial.display_id).await, false);
    // Report readiness only after capture has produced a real frame.
    capture.frames.wait_for(|frame| frame.is_some()).await?;
    let first_frame = capture
        .frames
        .borrow_and_update()
        .clone()
        .context("首帧丢失")?;
    let FrameEvent::Frame { jpeg, sequence } = first_frame else {
        let FrameEvent::Error(error) = first_frame else {
            unreachable!()
        };
        bail!("{error}");
    };
    send_json(socket, {
        let mut ready = serde_json::to_value(initial)?;
        ready["type"] = "ready".into();
        ready["capabilities"] = serde_json::to_value(state.native.capabilities())?;
        ready["capabilities"]["files"] = state.native.supports_files().into();
        ready
    })
    .await?;
    socket
        .send(Message::Binary(jpeg.as_ref().clone().into()))
        .await?;
    if let Some(warning) = input_warning {
        send_json(socket, warning).await?;
    }
    let mut frames = capture.frames.clone();
    let mut metadata = capture.info.clone();
    let updates = socket.outgoing.clone();
    let display = state.shared.display.clone();
    tokio::select! {
        result = control_loop(socket, &state, state.files.connect(), &mut frames, sequence, input_status,
            |change| capture.configure(change)) => result?,
        result = async {
            loop {
                metadata.changed().await.context("采集状态已停止")?;
                let info = metadata.borrow_and_update().clone();
                display.store(info.display_id, Ordering::Release);
                updates.send(Message::Text(landesk_core::protocol::display_message(info)?.to_string().into())).await.context("网页连接已关闭")?;
            }
            #[allow(unreachable_code)] Ok::<_, anyhow::Error>(())
        } => result?,
    }
    if let Err(error) = guard.finish().await {
        crate::diagnostics::log(&format!("Input cleanup failed at disconnect: {error:#}"));
    }
    Ok(())
}

async fn control_loop(
    socket: &mut SessionSocket,
    state: &AppState,
    files: FileChannel,
    frames: &mut tokio::sync::watch::Receiver<Option<FrameEvent>>,
    sequence: u64,
    input_status: InputStatus,
    configure: impl FnMut(Change) -> BoxFuture<'static, Result<CaptureInfo>>,
) -> Result<()> {
    let output = socket.frames.clone();
    tokio::select! {
        result = control_commands(socket, state, files, input_status, configure) => result,
        result = async {
            let mut last_sequence = sequence;
            loop {
                frames.changed().await.context("画面采集已停止")?;
                match frames.borrow_and_update().clone() {
                    Some(FrameEvent::Frame { jpeg, sequence }) if sequence != last_sequence => {
                        output.send_replace(Some(jpeg)); last_sequence = sequence;
                    }
                    Some(FrameEvent::Error(error)) => bail!("画面采集失败: {error}"),
                    _ => {}
                }
            }
            #[allow(unreachable_code)] Ok::<_, anyhow::Error>(())
        } => result,
    }
}

async fn control_commands(
    socket: &mut SessionSocket,
    state: &AppState,
    mut files: FileChannel,
    mut input_status: InputStatus,
    mut configure: impl FnMut(Change) -> BoxFuture<'static, Result<CaptureInfo>>,
) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut images = crate::clipboard_image::Images::default();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(incoming) = incoming else { break; };
                match incoming {
                    Message::Text(text) => {
                        let message: ClientMessage = serde_json::from_str(&text).context("操作消息无效")?;
                        match message {
                            ClientMessage::Heartbeat => {},
                            ClientMessage::SecureAttention => {
                                let message = match state.native.secure_attention().await {
                                    Ok(()) => "Ctrl+Alt+Delete 请求已提交".into(),
                                    Err(error) => format!("Ctrl+Alt+Delete 失败: {error:#}"),
                                };
                                send_json(socket, serde_json::json!({"type":"notice", "message":message})).await?;
                            },
                            ClientMessage::Disconnect => break,

                            ClientMessage::Hello { .. } => bail!("重复握手消息"),
                            ClientMessage::PasteImageChunk { id, offset, total, data } => {
                                let result = async {
                                    let complete = images.chunk(id, offset, total, &data)?;
                                    if let Some(png) = complete {
                                        let png = tokio::task::spawn_blocking(move || crate::clipboard_image::validate_png(png)).await??;
                                        state.native.paste_image(png).await?;
                                        Ok::<_, anyhow::Error>(serde_json::json!({"type":"image_pasted","id":id}))
                                    } else {
                                        Ok(serde_json::json!({"type":"image_progress","id":id}))
                                    }
                                }.await;
                                let response = result.unwrap_or_else(|error| serde_json::json!({"type":"image_error","id":id,"message":format!("{error:#}")}));
                                send_json(socket, response).await?;
                            }
                            ClientMessage::PasteImageCancel { id } => images.cancel(id),
                            ClientMessage::PasteText { text } => {
                                let can_recover = !text.is_empty();
                                if let Some(status) = input_status.observe(state.native.paste_text(text).await, can_recover) {
                                    send_json(socket, status).await?;
                                }
                            }
                            message @ (ClientMessage::ReadClipboard { .. } | ClientMessage::CopyClipboard { .. }) => {
                                let (id, result) = match message {
                                    ClientMessage::CopyClipboard { id } => (id, state.native.copy_text().await),
                                    ClientMessage::ReadClipboard { id } => (id, state.native.read_clipboard().await),
                                    _ => unreachable!(),
                                };
                                let response = match result {
                                    Ok(text) => serde_json::json!({"type":"clipboard_text","id":id,"text":text}),
                                    Err(error) => serde_json::json!({"type":"clipboard_error","id":id,"message":error.to_string()}),
                                };
                                send_json(socket,response).await?;
                            }
                            message @ (ClientMessage::SetResolution { .. } | ClientMessage::SetDisplay { .. } | ClientMessage::SetDisplaySource { .. }) => {
                                if let Some(status) = input_status.observe(state.native.input(InputEvent::ReleaseAll).await, false) {
                                    send_json(socket, status).await?;
                                }
                                let change = match message {
                                    ClientMessage::SetResolution { width } => Change::Resolution(width),
                                    ClientMessage::SetDisplay { display_id } => Change::Display(display_id),
                                    ClientMessage::SetDisplaySource { display } => Change::Source(display),
                                    _ => unreachable!(),
                                };
                                if let Err(error) = configure(change).await {
                                    send_json(socket,serde_json::json!({"type":"resolution_error","message":format!("{error:#}")})).await?;
                                }
                            }
                            message @ (ClientMessage::ListDirectory { .. } | ClientMessage::UploadStart { .. } | ClientMessage::UploadFinish { .. } | ClientMessage::UploadCancel { .. }) => {
                                if let Some(response) = files.submit(FileRequest::Message(message.try_into()?)) {
                                    send_json(socket,response).await?;
                                }
                            }
                            other => {
                                let can_recover = actionable_input(&other);
                                if let Some(status) = input_status.observe(state.native.input(other.try_into()?).await, can_recover) {
                                    send_json(socket, status).await?;
                                }
                            },
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(value) => { socket.send(Message::Pong(value)).await?; }
                    Message::Pong(_) => {}
                    Message::Binary(data) => {
                        if let Some(response) = files.submit(FileRequest::Chunk(data.to_vec())) {
                            send_json(socket,response).await?;
                        }
                    }
                }
            }
            response = files.response() => send_json(socket,response).await?,
            _ = tick.tick() => {
                images.expire();
                ensure!(!state.shared.shutdown.load(Ordering::Acquire),"Mac 已停止服务");
            }
        }
    }
    Ok(())
}

/// Serve one authenticated internal stream; production never binds a desktop TCP port.
pub async fn serve_stream<S>(stream: S, state: AppState) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    hyper::server::conn::http1::Builder::new()
        .serve_connection(
            TokioIo::new(stream),
            TowerToHyperService::new(router(state)),
        )
        .with_upgrades()
        .await?;
    Ok(())
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use landesk_core::domain::Capabilities;
    use landesk_core::domain::DisplayTarget;
    use landesk_core::ports::CaptureBackend;
    use landesk_core::ports::FileBackend;
    use landesk_core::ports::FileSession;
    use landesk_core::ports::FrameSource;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn input_warning_deduplicates_failures_and_requires_real_input_to_recover() {
        let mut status = InputStatus::default();
        let denied = || anyhow::anyhow!("Windows input denied (5)");
        assert_eq!(
            status.observe(Err(denied()), true).unwrap()["available"],
            false
        );
        assert!(status.observe(Err(denied()), true).is_none());
        // Different event counts/errors during the same unavailable period
        // must not trigger more browser releases and warnings.
        assert!(
            status
                .observe(Err(anyhow::anyhow!("SendInput 0/3 denied")), true)
                .is_none()
        );
        assert!(status.observe(Ok(()), false).is_none());
        assert!(status.failure.is_some());
        assert!(!actionable_input(&ClientMessage::ReleaseAll));
        assert!(!actionable_input(&ClientMessage::Text {
            text: String::new()
        }));
        assert!(!actionable_input(&ClientMessage::Wheel { x: 0, y: 0 }));
        assert!(actionable_input(&ClientMessage::Key {
            key: "Enter".into(),
            down: true
        }));
        assert_eq!(status.observe(Ok(()), true).unwrap()["available"], true);
        assert!(status.observe(Ok(()), true).is_none());
        assert!(status.observe(Err(denied()), true).is_some());
    }

    struct Gate {
        permissions: AtomicUsize,
        restored: AtomicUsize,
    }
    impl DesktopControl for Gate {
        fn supports_files(&self) -> bool {
            true
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                os: "windows",
                virtual_display: true,
            }
        }
        fn check_permissions(&self) -> Result<()> {
            self.permissions.fetch_add(1, Ordering::SeqCst);
            bail!("permission check reached")
        }
        fn begin_input(&self, _: u32) -> BoxFuture<'_, Result<()>> {
            panic!("input must not start")
        }
        fn input(&self, _: InputEvent) -> BoxFuture<'_, Result<()>> {
            panic!("input must not start")
        }
        fn paste_text(&self, _: String) -> BoxFuture<'_, Result<()>> {
            panic!("paste must not start")
        }
        fn paste_image(&self, _: Vec<u8>) -> BoxFuture<'_, Result<()>> {
            panic!("paste must not start")
        }
        fn copy_text(&self) -> BoxFuture<'_, Result<String>> {
            panic!("copy must not start")
        }
        fn read_clipboard(&self) -> BoxFuture<'_, Result<String>> {
            panic!("clipboard must not start")
        }
        fn end_session(&self) -> BoxFuture<'_, Result<()>> {
            panic!("session must not start")
        }
        fn restore(&self) {
            self.restored.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct NoCapture;
    impl CaptureBackend for NoCapture {
        fn open(&mut self, _: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>> {
            panic!("capture must not start")
        }
    }
    struct NoFiles;
    impl FileBackend for NoFiles {
        fn open_session(self: Arc<Self>, _: Arc<AtomicBool>) -> Box<dyn FileSession> {
            panic!("files must not open")
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "build client desktop_probe and set LANDESK_CLIENT_PROBE to its executable"]
    async fn actual_client_crosses_subsystem_memory_backend_and_browser_authentication() {
        let probe = std::env::var_os("LANDESK_CLIENT_PROBE").expect("set LANDESK_CLIENT_PROBE");
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let password = "integration-only";
        let config = crate::adapters::identity::create(
            password,
            address.port(),
            landesk_core::security::secret::Secret::for_test(
                password.as_bytes(),
                landesk_core::domain::identity::PASSWORD_PURPOSE,
            ),
        )
        .unwrap();
        let path = directory.path().join("connection.json");
        crate::adapters::identity::write_new(&config, &path).unwrap();
        let state = AppState {
            native: Arc::new(Gate {
                permissions: AtomicUsize::new(0),
                restored: AtomicUsize::new(0),
            }),
            transport_key: landesk_core::security::transport_key::TransportKey::new(
                config.backend_key.clone(),
            )
            .unwrap(),
            shared: Arc::new(Shared::default()),
            capture: CaptureService::new(|| Box::new(NoCapture)).unwrap(),
            files: FileService::new(NoFiles),
        };
        let backend: crate::adapters::ssh::Backend = Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                let (client, server) = tokio::io::duplex(65536);
                tokio::spawn(serve_stream(server, state));
                Ok(Box::new(client) as Box<dyn crate::adapters::ssh::StreamIo>)
            })
        });
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let service = tokio::spawn(crate::adapters::ssh::run(
            listener,
            config,
            crate::adapters::ssh::PasswordPolicy::LiveFile(path),
            backend,
            async {
                let _ = stopped.await;
            },
        ));
        let result = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut child = std::process::Command::new(probe)
                .args(["127.0.0.1", &address.port().to_string()])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(password.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap();
        let _ = stop.send(());
        service.await.unwrap().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("PASS:"));
    }

    #[tokio::test]
    async fn access_key_is_checked_before_ownership_permissions_capture_and_input() {
        let shared = Arc::new(Shared::default());
        let native = Arc::new(Gate {
            permissions: AtomicUsize::new(0),
            restored: AtomicUsize::new(0),
        });
        let state = AppState {
            native: native.clone(),
            transport_key: landesk_core::security::transport_key::TransportKey::new(
                "test-key".into(),
            )
            .unwrap(),
            shared: shared.clone(),
            capture: CaptureService::new(|| Box::new(NoCapture)).unwrap(),
            files: FileService::new(NoFiles),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        // The local desktop socket must not expose either assets or control
        // without the transport's private credential.
        for path in ["/", "/app.js", "/ws"] {
            use tokio::io::AsyncReadExt;
            use tokio::io::AsyncWriteExt;
            let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
            socket
                .write_all(
                    format!(
                        "GET {path} HTTP/1.1\r\nHost: landesk.internal\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 401"), "{response}");
        }
        for (key, expected, active) in [
            (None, "access denied", false),
            (Some("wrong"), "access denied", true),
            (Some("test-key"), "已有一个连接", true),
            (Some("test-key"), "permission check reached", false),
        ] {
            shared.active.store(active, Ordering::Release);
            let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
            request
                .headers_mut()
                .insert("host", "landesk.internal".parse().unwrap());
            request
                .headers_mut()
                .insert("origin", "http://landesk.internal".parse().unwrap());
            if let Some(key) = key {
                request
                    .headers_mut()
                    .insert(crate::adapters::ssh::AUTH_HEADER, key.parse().unwrap());
            }
            let result = connect_async(request).await;
            if expected == "access denied" {
                let Err(tokio_tungstenite::tungstenite::Error::Http(response)) = result else {
                    panic!("must reject before upgrade");
                };
                assert_eq!(response.status(), 401);
                assert_eq!(shared.active.load(Ordering::Acquire), active);
                assert_eq!(native.permissions.load(Ordering::SeqCst), 0);
                assert_eq!(native.restored.load(Ordering::SeqCst), 0);
                continue;
            }
            let (mut ws, _) = result.unwrap();
            ws.send(WsMessage::Text(r#"{"type":"hello"}"#.into()))
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            assert!(response.contains(expected), "{response}");
            assert_eq!(shared.active.load(Ordering::Acquire), active);
            if expected != "permission check reached" {
                assert_eq!(native.permissions.load(Ordering::SeqCst), 0);
                assert_eq!(native.restored.load(Ordering::SeqCst), 0);
            }
        }
        assert_eq!(native.permissions.load(Ordering::SeqCst), 1);
        assert_eq!(native.restored.load(Ordering::SeqCst), 1);
        server.abort();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::platform::macos::files::HomeFiles;
    use crate::platform::macos::native::Command;
    use crate::platform::macos::native::Native;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::sync::mpsc;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    fn state() -> (AppState, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        (
            AppState {
                transport_key: landesk_core::security::transport_key::TransportKey::new(
                    "test-key".into(),
                )
                .unwrap(),
                shared: shared.clone(),
                native: Arc::new(Native { tx, shared }),
                capture: CaptureService::new(|| {
                    Box::new(crate::platform::macos::capture::MacCaptureBackend::default())
                })
                .unwrap(),
                files: FileService::new(
                    HomeFiles::open(std::path::Path::new("/private/tmp")).unwrap(),
                ),
            },
            rx,
        )
    }
    async fn serve(state: AppState) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        (addr, handle)
    }
    fn request(
        addr: std::net::SocketAddr,
        origin: &str,
    ) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
        request
            .headers_mut()
            .insert("host", "landesk.internal".parse().unwrap());
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        request.headers_mut().insert(
            crate::adapters::ssh::AUTH_HEADER,
            "test-key".parse().unwrap(),
        );
        request
    }
    #[tokio::test]
    async fn file_and_clipboard_operations_require_session_handshake() {
        let (state, _rx) = state();
        let (addr, handle) = serve(state).await;
        for command in [
            serde_json::json!({"type":"list_directory","id":1,"path":""}),
            serde_json::json!({"type":"upload_start","id":1,"path":"","name":"test","size":0}),
            serde_json::json!({"type":"read_clipboard","id":1}),
            serde_json::json!({"type":"paste_image_chunk","id":1,"offset":0,"total":1,"data":"AA=="}),
        ] {
            let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
                .await
                .unwrap();
            ws.send(WsMessage::Text(command.to_string().into()))
                .await
                .unwrap();
            let response = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(response.contains("请先建立会话"));
        }
        handle.abort();
    }
    #[tokio::test]
    async fn foreign_missing_and_mismatched_origins_cannot_open_control_socket() {
        let (state, _rx) = state();
        let (addr, handle) = serve(state).await;
        for (host, origin) in [
            ("landesk.internal", Some("http://foreign.example")),
            ("landesk.internal", Some("http://localhost:17890")),
            (
                "foreign.example:17890",
                Some("http://foreign.example:17890"),
            ),
            ("landesk.internal", Some("null")),
            ("landesk.internal", None),
        ] {
            let mut request = request(addr, origin.unwrap_or(""));
            request.headers_mut().insert("host", host.parse().unwrap());
            if origin.is_none() {
                request.headers_mut().remove("origin");
            }
            let error = connect_async(request).await.unwrap_err();
            assert!(error.to_string().contains("403"), "{host} {origin:?}");
        }
        handle.abort();
    }
    #[tokio::test]
    async fn hello_without_code_reaches_session_check_and_preserves_existing_controller() {
        let (state, _rx) = state();
        state.shared.active.store(true, Ordering::Release);
        let shared = state.shared.clone();
        let (addr, handle) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
            .await
            .unwrap();
        ws.send(WsMessage::Text(r#"{"type":"hello"}"#.into()))
            .await
            .unwrap();
        assert!(
            ws.next()
                .await
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap()
                .contains("已有一个连接")
        );
        assert!(shared.active.load(Ordering::Acquire));
        handle.abort();
    }
    #[test]
    fn session_failure_queues_input_release_and_releases_ownership() {
        let (state, rx) = state();
        drop(ControlSession::acquire(state.native.clone(), state.shared.clone()).unwrap());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::RestoreSession { .. }
        ));
        assert!(!state.shared.active.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn blocked_directory_does_not_block_frames_disconnect_or_release_session() {
        blocked_directory_connection(EndConnection::Disconnect).await;
    }

    #[tokio::test]
    async fn closed_browser_releases_session_while_directory_is_blocked() {
        blocked_directory_connection(EndConnection::Close).await;
    }

    #[tokio::test]
    async fn missing_heartbeat_does_not_disconnect_while_directory_is_blocked() {
        blocked_directory_connection(EndConnection::NoHeartbeat).await;
    }

    enum EndConnection {
        Disconnect,
        Close,
        NoHeartbeat,
    }

    async fn blocked_directory_connection(end: EndConnection) {
        let (state, commands) = state();
        let shared = state.shared.clone();
        let (files, started, release) = state.files.blocked_channel();
        let (frames, receiver) = tokio::sync::watch::channel(None);
        let (finished, done) = tokio::sync::oneshot::channel();
        let context = Arc::new(std::sync::Mutex::new(Some((files, receiver, finished))));
        let app = Router::new().route(
            "/ws",
            get(move |upgrade: WebSocketUpgrade| {
                let state = state.clone();
                let (files, mut receiver, finished) = context.lock().unwrap().take().unwrap();
                async move {
                    upgrade.on_upgrade(move |socket| async move {
                        let (mut socket, closed) = monitor_connection(socket);
                        let guard = ControlSession::acquire(state.native.clone(), state.shared.clone()).unwrap();
                        let result = tokio::select! {
                            result = control_loop(&mut socket, &state, files, &mut receiver, 0, InputStatus::default(), |_| {
                                Box::pin(async { Ok(CaptureInfo::default()) })
                            }) => result,
                            result = closed => result,
                        };
                        drop(guard);
                        let _ = finished.send(result);
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
            .await
            .unwrap();
        ws.send(WsMessage::Text(
            r#"{"type":"list_directory","id":1,"path":""}"#.into(),
        ))
        .await
        .unwrap();
        started.await.unwrap();
        ws.send(WsMessage::Text(r#"{"type":"heartbeat"}"#.into()))
            .await
            .unwrap();
        frames
            .send(Some(FrameEvent::Frame {
                jpeg: Arc::new(vec![1, 2, 3]),
                sequence: 1,
            }))
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame.into_data().as_ref(), &[1, 2, 3]);
        match end {
            EndConnection::Disconnect => {
                ws.send(WsMessage::Text(r#"{"type":"disconnect"}"#.into()))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(1), done)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            }
            EndConnection::Close => {
                drop(ws);
                // Abrupt socket closure can report an IO error, but must release ownership.
                let _ = tokio::time::timeout(Duration::from_secs(1), done)
                    .await
                    .unwrap()
                    .unwrap();
            }
            EndConnection::NoHeartbeat => {
                let message = tokio::time::timeout(Duration::from_secs(12), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap();
                assert!(message.contains("file_error") && message.contains("超时"));
                // The file request expires at 10 seconds. Keep the connection
                // idle beyond the former 15-second heartbeat deadline.
                tokio::time::sleep(Duration::from_secs(6)).await;
                assert!(shared.active.load(Ordering::Acquire));
                frames
                    .send(Some(FrameEvent::Frame {
                        jpeg: Arc::new(vec![4, 5, 6]),
                        sequence: 2,
                    }))
                    .unwrap();
                let frame = tokio::time::timeout(Duration::from_secs(1), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(frame.into_data().as_ref(), &[4, 5, 6]);
                ws.send(WsMessage::Text(r#"{"type":"disconnect"}"#.into()))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(1), done)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            }
        }
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::RestoreSession { .. }
        ));
        assert!(
            shared
                .active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        );
        release.send(()).unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn blocked_native_input_does_not_stop_frames_or_disconnect() {
        let (state, commands) = state();
        let shared = state.shared.clone();
        let (frames, receiver) = tokio::sync::watch::channel(None);
        let (finished, done) = tokio::sync::oneshot::channel();
        let context = Arc::new(std::sync::Mutex::new(Some((receiver, finished))));
        let app = Router::new().route("/ws", get(move |upgrade: WebSocketUpgrade| {
            let state = state.clone();
            let (mut receiver, finished) = context.lock().unwrap().take().unwrap();
            async move { upgrade.on_upgrade(move |socket| async move {
                let (mut socket, closed) = monitor_connection(socket);
                let guard = ControlSession::acquire(state.native.clone(), state.shared.clone()).unwrap();
                let result = tokio::select! {
                    result = control_loop(&mut socket, &state, state.files.connect(), &mut receiver, 0, InputStatus::default(),
                        |_| Box::pin(async { Ok(CaptureInfo::default()) })) => result,
                    result = closed => result,
                };
                drop(guard); let _ = finished.send(result);
            }) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
            .await
            .unwrap();
        ws.send(WsMessage::Text(
            r#"{"type":"key","key":"ArrowRight","down":true}"#.into(),
        ))
        .await
        .unwrap();
        let command = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(command) = commands.try_recv() {
                    break command;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let Command::Input { reply, .. } = command else {
            panic!("expected input");
        };
        // Leave the main-thread reply unresolved while receiving a real WS frame.
        frames
            .send(Some(FrameEvent::Frame {
                jpeg: Arc::new(vec![9, 8, 7]),
                sequence: 1,
            }))
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame.into_data().as_ref(), &[9, 8, 7]);
        ws.send(WsMessage::Text(r#"{"type":"disconnect"}"#.into()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(reply.is_closed());
        assert!(!shared.active.load(Ordering::Acquire));
        server.abort();
    }

    #[tokio::test]
    async fn denied_input_preserves_frames_display_switching_and_recovers_without_reconnect() {
        let (state, commands) = state();
        let shared = state.shared.clone();
        let configured = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (frames, receiver) = tokio::sync::watch::channel(None);
        let (finished, done) = tokio::sync::oneshot::channel();
        let context = Arc::new(std::sync::Mutex::new(Some((receiver, finished))));
        let switched = configured.clone();
        let app = Router::new().route("/ws", get(move |upgrade: WebSocketUpgrade| {
            let state = state.clone(); let switched = switched.clone();
            let (mut receiver, finished) = context.lock().unwrap().take().unwrap();
            async move { upgrade.on_upgrade(move |socket| async move {
                let (mut socket, closed) = monitor_connection(socket);
                let guard = ControlSession::acquire(state.native.clone(), state.shared.clone()).unwrap();
                let result = tokio::select! {
                    result = control_loop(&mut socket, &state, state.files.connect(), &mut receiver, 0, InputStatus::default(),
                        move |_| { switched.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(CaptureInfo::default()) }) }) => result,
                    result = closed => result,
                };
                drop(guard); let _ = finished.send(result);
            }) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
            .await
            .unwrap();
        async fn reply(commands: &std::sync::mpsc::Receiver<Command>, success: bool) {
            let command = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Ok(command) = commands.try_recv() {
                        break command;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let sender = match command {
                Command::Input { reply, .. } => reply,
                Command::PasteText { reply, .. } => reply,
                _ => panic!("expected input command"),
            };
            sender
                .send(if success {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("SendInput denied (5)"))
                })
                .unwrap();
        }
        ws.send(WsMessage::Text(
            r#"{"type":"key","key":"Control","down":true}"#.into(),
        ))
        .await
        .unwrap();
        reply(&commands, false).await;
        let warning = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(
            warning.contains("input_state")
                && warning.contains("false")
                && warning.contains("denied")
        );
        // The secure-attention command has its own response even when ordinary
        // input is unavailable. This fixture's platform does not support SAS.
        ws.send(WsMessage::Text(r#"{"type":"secure_attention"}"#.into()))
            .await
            .unwrap();
        let notice = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(notice.contains("notice") && notice.contains("Ctrl+Alt+Delete 失败"));
        // Repeat denial, then a successful no-op release; neither should flood or clear the warning.
        for (message, success) in [
            (r#"{"type":"pointer","x":0.5,"y":0.5}"#, false),
            (r#"{"type":"release_all"}"#, true),
            (r#"{"type":"set_resolution","width":1280}"#, false),
        ] {
            ws.send(WsMessage::Text(message.into())).await.unwrap();
            reply(&commands, success).await;
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while configured.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        frames
            .send(Some(FrameEvent::Frame {
                jpeg: Arc::new(vec![7, 8, 9]),
                sequence: 1,
            }))
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame.into_data().as_ref(), &[7, 8, 9]);
        assert!(shared.active.load(Ordering::Acquire));
        ws.send(WsMessage::Text(
            r#"{"type":"paste_text","text":"retry"}"#.into(),
        ))
        .await
        .unwrap();
        reply(&commands, true).await;
        let recovered = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(recovered.contains("input_state") && recovered.contains("true"));
        // An actual capture failure is still fatal.
        frames
            .send(Some(FrameEvent::Error("capture unavailable".into())))
            .unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(failure.to_string().contains("capture unavailable"));
        assert!(!shared.active.load(Ordering::Acquire));
        server.abort();
    }

    #[tokio::test]
    async fn pointer_coalescing_preserves_button_order_and_queue_is_bounded() {
        let (outgoing, _responses) = tokio::sync::mpsc::channel(8);
        let (input, incoming) = tokio::sync::mpsc::channel(32);
        let (frames, _) = tokio::sync::watch::channel(None);
        let mut socket = SessionSocket {
            outgoing,
            incoming,
            frames,
            deferred: None,
        };
        for x in [0.1, 0.2, 0.3] {
            input
                .send(Message::Text(
                    serde_json::json!({"type":"pointer","x":x,"y":0.5})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
        input
            .send(Message::Text(
                r#"{"type":"button","button":0,"down":true}"#.into(),
            ))
            .await
            .unwrap();
        input
            .send(Message::Text(
                r#"{"type":"pointer","x":0.9,"y":0.5}"#.into(),
            ))
            .await
            .unwrap();
        let Message::Text(first) = socket.recv().await.unwrap() else {
            panic!();
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first).unwrap()["x"],
            0.3
        );
        let Message::Text(second) = socket.recv().await.unwrap() else {
            panic!();
        };
        assert!(second.contains("button"));
        assert!(socket.recv().await.is_some());
        for _ in 0..32 {
            input.try_send(Message::Ping(Vec::new().into())).unwrap();
        }
        assert!(input.try_send(Message::Ping(Vec::new().into())).is_err());
    }

    #[tokio::test]
    async fn handshake_can_arrive_after_eight_seconds() {
        let (state, _commands) = state();
        state.shared.active.store(true, Ordering::Release);
        let (addr, server) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(8200)).await;
        ws.send(WsMessage::Text(r#"{"type":"hello"}"#.into()))
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        // It reaches the ownership check, rather than expiring before hello.
        assert!(response.contains("已有一个连接"));
        server.abort();
    }

    #[tokio::test]
    async fn delayed_native_initialization_can_finish_after_three_seconds() {
        let (state, commands) = state();
        let operation = tokio::spawn(async move { state.native.begin_input(1).await });
        tokio::time::sleep(Duration::from_millis(3200)).await;
        assert!(!operation.is_finished());
        let Command::BeginInput { reply, .. } = commands.try_recv().unwrap() else {
            panic!("expected input initialization");
        };
        reply.send(Ok(())).unwrap();
        operation.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_initialization_and_releases_session() {
        for explicit_disconnect in [false, true] {
            let (state, commands) = state();
            let shared = state.shared.clone();
            let (started, ready) = tokio::sync::oneshot::channel();
            let (finished, done) = tokio::sync::oneshot::channel();
            let context = Arc::new(std::sync::Mutex::new(Some((started, finished))));
            let app = Router::new().route(
                "/ws",
                get(move |upgrade: WebSocketUpgrade| {
                    let state = state.clone();
                    let (started, finished) = context.lock().unwrap().take().unwrap();
                    async move {
                        upgrade.on_upgrade(move |socket| async move {
                            let (_socket, closed) = monitor_connection(socket);
                            let guard =
                                ControlSession::acquire(state.native.clone(), state.shared.clone())
                                    .unwrap();
                            let result = tokio::select! {
                                result = async {
                                    let _ = started.send(());
                                    state.native.begin_input(1).await
                                } => result,
                                result = closed => result,
                            };
                            drop(guard);
                            let _ = finished.send(result);
                        })
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let (mut ws, _) = connect_async(request(addr, "http://landesk.internal"))
                .await
                .unwrap();
            ready.await.unwrap();
            let Command::BeginInput { reply, .. } = commands.try_recv().unwrap() else {
                panic!("expected input initialization");
            };
            if explicit_disconnect {
                ws.send(WsMessage::Text(r#"{"type":"disconnect"}"#.into()))
                    .await
                    .unwrap();
            } else {
                drop(ws);
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), done)
                .await
                .unwrap()
                .unwrap();
            assert!(!shared.active.load(Ordering::Acquire));
            assert!(reply.is_closed());
            assert!(matches!(
                commands.try_recv().unwrap(),
                Command::RestoreSession { .. }
            ));
            server.abort();
        }
    }
}
