use crate::{
    capture::{CaptureService, Change, FrameEvent},
    file_worker::{FileChannel, FileRequest, FileService},
    native::{Native, permissions},
    protocol::{ClientMessage, PORT, origin_allowed},
};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use std::{sync::atomic::Ordering, time::Duration};

#[derive(Clone)]
pub struct AppState {
    pub native: Native,
    pub files: FileService,
    pub capture: CaptureService,
}

struct SessionGuard(Native);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        // Queue restoration before allowing a new session to acquire ownership.
        self.0.restore();
        self.0.shared.active.store(false, Ordering::Release);
    }
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

async fn wait_for_shutdown(native: &Native) {
    while !native.shared.shutdown.load(Ordering::Acquire) {
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
            get(|| async { page_headers(Html(include_str!("../web/index.html")).into_response()) }),
        )
        .route(
            "/app.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!("../web/app.js"),
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
                        include_str!("../web/frames.js"),
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
                        include_str!("../web/keyboard.js"),
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
                        include_str!("../web/clipboard.js"),
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
                        include_str!("../web/files.js"),
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
                        include_str!("../web/style.css"),
                    )
                        .into_response(),
                )
            }),
        )
        .route("/ws", get(upgrade))
        .with_state(state)
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
            let native = state.native.clone();
            let (result, transport_done) = tokio::select! {
                result = session(&mut socket, state) => (result, false),
                result = &mut closed => (result, true),
                _ = wait_for_shutdown(&native) => (Ok(()), false),
            };
            if let Err(e) = result {
                eprintln!("LanDesk session failed: {e:#}");
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

async fn send_json(socket: &mut SessionSocket, value: serde_json::Value) -> Result<()> {
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

async fn session(socket: &mut SessionSocket, state: AppState) -> Result<()> {
    let message = socket.recv().await.context("连接已关闭")?;
    let Message::Text(text) = message else {
        bail!("请先建立会话");
    };
    let ClientMessage::Hello {} = serde_json::from_str(&text)? else {
        bail!("请先建立会话");
    };
    state
        .native
        .shared
        .active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| anyhow::anyhow!("已有一个连接，请先断开"))?;
    let _guard = SessionGuard(state.native.clone());
    let (capture_allowed, input_allowed) = permissions();
    ensure!(
        capture_allowed,
        "请在 Mac 系统设置中授予 LanDesk 屏幕录制权限，然后关闭并重新打开应用"
    );
    ensure!(
        input_allowed,
        "请在 Mac 系统设置中授予 LanDesk 辅助功能权限，然后关闭并重新打开应用"
    );
    eprintln!("LanDesk session starting capture");
    let mut capture = state.capture.start().await?;
    let initial = capture.info.borrow().clone();
    state
        .native
        .begin_input(initial["display_id"].as_u64().context("缺少显示器编号")? as u32)
        .await?;
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
        let mut ready = initial;
        ready["type"] = "ready".into();
        ready
    })
    .await?;
    socket
        .send(Message::Binary(jpeg.as_ref().clone().into()))
        .await?;
    let mut frames = capture.frames.clone();
    let mut metadata = capture.info.clone();
    let updates = socket.outgoing.clone();
    let display = state.native.shared.display.clone();
    tokio::select! {
        result = control_loop(socket, &state, state.files.connect(), &mut frames, sequence,
            |change| capture.configure(change)) => result?,
        result = async {
            loop {
                metadata.changed().await.context("采集状态已停止")?;
                let info = metadata.borrow_and_update().clone();
                display.store(info["display_id"].as_u64().context("缺少显示器编号")? as u32, Ordering::Release);
                updates.send(Message::Text(info.to_string().into())).await.context("网页连接已关闭")?;
            }
            #[allow(unreachable_code)] Ok::<_, anyhow::Error>(())
        } => result?,
    }
    state.native.end_session().await?;
    Ok(())
}

async fn control_loop(
    socket: &mut SessionSocket,
    state: &AppState,
    files: FileChannel,
    frames: &mut tokio::sync::watch::Receiver<Option<FrameEvent>>,
    sequence: u64,
    configure: impl FnMut(Change) -> BoxFuture<'static, Result<serde_json::Value>>,
) -> Result<()> {
    let output = socket.frames.clone();
    tokio::select! {
        result = control_commands(socket, state, files, configure) => result,
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
    mut configure: impl FnMut(Change) -> BoxFuture<'static, Result<serde_json::Value>>,
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
                                if let Err(error) = state.native.paste_text(text).await {
                                    send_json(socket, serde_json::json!({"type":"notice", "message":format!("粘贴失败: {error:#}")})).await?;
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
                            message @ (ClientMessage::SetResolution { .. } | ClientMessage::SetDisplay { .. }) => {
                                state.native.input(ClientMessage::ReleaseAll).await?;
                                let change = match message {
                                    ClientMessage::SetResolution { width } => Change::Resolution(width),
                                    ClientMessage::SetDisplay { display_id } => Change::Display(display_id),
                                    _ => unreachable!(),
                                };
                                if let Err(error) = configure(change).await {
                                    send_json(socket,serde_json::json!({"type":"resolution_error","message":format!("{error:#}")})).await?;
                                }
                            }
                            message @ (ClientMessage::ListDirectory { .. } | ClientMessage::UploadStart { .. } | ClientMessage::UploadFinish { .. } | ClientMessage::UploadCancel { .. }) => {
                                if let Some(response) = files.submit(FileRequest::Message(message)) {
                                    send_json(socket,response).await?;
                                }
                            }
                            other => state.native.input(other).await?,
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
                ensure!(!state.native.shared.shutdown.load(Ordering::Acquire),"Mac 已停止服务");
            }
        }
    }
    Ok(())
}

pub async fn run(listener: tokio::net::TcpListener, state: AppState) -> Result<()> {
    let shared = state.native.shared.clone();
    println!("LanDesk 已启动，仅监听 http://127.0.0.1:{PORT}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            while !shared.shutdown.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::HomeFiles;
    use crate::native::{Command, Shared};
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::sync::mpsc;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as WsMessage, client::IntoClientRequest},
    };

    fn state() -> (AppState, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        (
            AppState {
                native: Native {
                    tx,
                    shared: Arc::new(Shared::default()),
                },
                capture: CaptureService::new().unwrap(),
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
            .insert("host", "127.0.0.1:17890".parse().unwrap());
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
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
            let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
            ("127.0.0.1:17890", Some("http://foreign.example")),
            ("127.0.0.1:17890", Some("http://localhost:17890")),
            (
                "foreign.example:17890",
                Some("http://foreign.example:17890"),
            ),
            ("127.0.0.1:17890", Some("null")),
            ("127.0.0.1:17890", None),
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
        state.native.shared.active.store(true, Ordering::Release);
        let shared = state.native.shared.clone();
        let (addr, handle) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
        state.native.shared.active.store(true, Ordering::Release);
        drop(SessionGuard(state.native.clone()));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::RestoreSession { .. }
        ));
        assert!(!state.native.shared.active.load(Ordering::Acquire));
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
        let shared = state.native.shared.clone();
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
                        state.native.shared.active.store(true, Ordering::Release);
                        let guard = SessionGuard(state.native.clone());
                        let result = tokio::select! {
                            result = control_loop(&mut socket, &state, files, &mut receiver, 0, |_| {
                                Box::pin(async { Ok(serde_json::json!({})) })
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
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
        let shared = state.native.shared.clone();
        let (frames, receiver) = tokio::sync::watch::channel(None);
        let (finished, done) = tokio::sync::oneshot::channel();
        let context = Arc::new(std::sync::Mutex::new(Some((receiver, finished))));
        let app = Router::new().route("/ws", get(move |upgrade: WebSocketUpgrade| {
            let state = state.clone();
            let (mut receiver, finished) = context.lock().unwrap().take().unwrap();
            async move { upgrade.on_upgrade(move |socket| async move {
                let (mut socket, closed) = monitor_connection(socket);
                state.native.shared.active.store(true, Ordering::Release);
                let guard = SessionGuard(state.native.clone());
                let result = tokio::select! {
                    result = control_loop(&mut socket, &state, state.files.connect(), &mut receiver, 0,
                        |_| Box::pin(async { Ok(serde_json::json!({})) })) => result,
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
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
        state.native.shared.active.store(true, Ordering::Release);
        let (addr, server) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
            let shared = state.native.shared.clone();
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
                            state.native.shared.active.store(true, Ordering::Release);
                            let guard = SessionGuard(state.native.clone());
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
            let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
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
