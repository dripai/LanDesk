use crate::{
    desktop::DesktopControl,
    file_worker::{FileChannel, FileRequest, FileService},
    platform::{CurrentPlatform, FrameEvent, HostPlatform},
    protocol::{ClientMessage, origin_allowed},
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
use futures_util::SinkExt;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct AppState {
    pub native: DesktopControl,
    pub files: FileService,
}

struct SessionGuard(DesktopControl);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        // Queue restoration before allowing a new session to acquire ownership.
        self.0.restore();
        self.0.shared.active.store(false, Ordering::Release);
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
        .route(
            "/platform.js",
            get(|| async {
                page_headers(
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        include_str!("../web/platform.js"),
                    )
                        .into_response(),
                )
            }),
        )
        .route("/ws", get(upgrade))
        .route(
            "/info",
            get(|| async { page_headers(axum::Json(CurrentPlatform.info()).into_response()) }),
        )
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
        return (StatusCode::FORBIDDEN, "只允许 LanDeskClient 网页入口连接").into_response();
    }
    upgrade
        .max_message_size(512 * 1024)
        .max_frame_size(512 * 1024)
        .on_upgrade(move |mut socket| async move {
            if let Err(e) = session(&mut socket, state).await {
                let _ = send_json(
                    &mut socket,
                    serde_json::json!({"type":"error","message":e.to_string()}),
                )
                .await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), socket.close()).await;
        })
        .into_response()
}

async fn send_json(socket: &mut WebSocket, value: serde_json::Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await??;
    Ok(())
}

async fn session(socket: &mut WebSocket, state: AppState) -> Result<()> {
    let message = tokio::time::timeout(Duration::from_secs(8), socket.recv())
        .await?
        .context("连接已关闭")??;
    let Message::Text(text) = message else {
        bail!("请先建立会话");
    };
    let ClientMessage::Hello { protocol_version } = serde_json::from_str(&text)? else {
        bail!("请先建立会话");
    };
    ensure!(
        protocol_version == crate::platform::PROTOCOL_VERSION,
        "服务端协议版本不匹配，请更新 LanDesk 并刷新页面"
    );
    state
        .native
        .shared
        .active
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| anyhow::anyhow!("已有一个连接，请先断开"))?;
    let _guard = SessionGuard(state.native.clone());
    ensure!(
        !state.native.shared.maintenance.load(Ordering::SeqCst),
        "正在保存连接设置，请完成后重新连接"
    );
    state.native.shared.cancel.store(false, Ordering::Release);
    CurrentPlatform.check_permissions()?;
    let mut capture = tokio::task::spawn_blocking(|| CurrentPlatform.capture()).await??;
    let (input_width, input_height) = capture.input_dimensions();
    state.native.begin_input(input_width, input_height).await?;
    let mut frames = capture.frames();
    // Report readiness only after capture has produced a real frame.
    tokio::time::timeout(
        Duration::from_secs(8),
        frames.wait_for(|frame| frame.is_some()),
    )
    .await??;
    let first_frame = frames.borrow_and_update().clone().context("首帧丢失")?;
    let FrameEvent::Frame { jpeg, sequence } = first_frame else {
        let FrameEvent::Error(error) = first_frame else {
            unreachable!()
        };
        bail!("{error}");
    };
    send_json(
        socket,
        serde_json::json!({"type":"ready","width":capture.dimensions().0,"height":capture.dimensions().1,"server":CurrentPlatform.info()}),
    )
    .await?;
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Binary(jpeg.as_ref().clone().into())),
    )
    .await??;
    control_loop(
        socket,
        &state,
        state.files.connect(),
        &mut frames,
        sequence,
        |width| {
            capture.set_resolution(width)?;
            Ok(capture.dimensions())
        },
    )
    .await?;
    state.native.end_session().await?;
    Ok(())
}

async fn control_loop(
    socket: &mut WebSocket,
    state: &AppState,
    mut files: FileChannel,
    frames: &mut tokio::sync::watch::Receiver<Option<FrameEvent>>,
    sequence: u64,
    mut set_resolution: impl FnMut(Option<u32>) -> Result<(u32, u32)>,
) -> Result<()> {
    let mut heartbeat = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut events = 0_u32;
    let mut rate_window = Instant::now();
    let mut last_sequence = sequence;
    let mut images = crate::clipboard_image::Images::default();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(incoming) = incoming else { break; };
                match incoming? {
                    Message::Text(text) => {
                        if rate_window.elapsed() >= Duration::from_secs(1) { rate_window=Instant::now(); events=0; }
                        events+=1;
                        ensure!(events<=250,"操作频率过高，连接已关闭");
                        let message: ClientMessage = serde_json::from_str(&text).context("操作消息无效")?;
                        match message {
                            ClientMessage::Heartbeat => heartbeat=Instant::now(),
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
                            ClientMessage::ReadClipboard { id } => {
                                let response = match state.native.read_clipboard().await {
                                    Ok(text) => serde_json::json!({"type":"clipboard_text","id":id,"text":text}),
                                    Err(error) => serde_json::json!({"type":"clipboard_error","id":id,"message":error.to_string()}),
                                };
                                send_json(socket,response).await?;
                            }
                            ClientMessage::SetResolution { width } => {
                                let result = tokio::task::block_in_place(|| set_resolution(width));
                                let response = match result {
                                    Ok((w, h)) => serde_json::json!({"type":"resolution","width":w,"height":h,"requested_width":width}),
                                    Err(error) => serde_json::json!({"type":"resolution_error","message":format!("{error:#}")}),
                                };
                                send_json(socket,response).await?;
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
            changed = frames.changed() => {
                changed.context("画面采集已停止")?;
                let frame = frames.borrow_and_update().clone();
                match frame {
                    Some(FrameEvent::Frame { jpeg, sequence }) if sequence != last_sequence => {
                        tokio::time::timeout(Duration::from_secs(3),socket.send(Message::Binary(jpeg.as_ref().clone().into()))).await??;
                        last_sequence=sequence;
                    }
                    Some(FrameEvent::Error(error)) => bail!("画面采集失败: {error}"),
                    _ => {}
                }
            }
            _ = tick.tick() => {
                images.expire();
                ensure!(heartbeat.elapsed()<Duration::from_secs(15),"连接超时，本机画面已恢复");
                ensure!(!state.native.shared.shutdown.load(Ordering::Acquire),"Mac 已停止服务");
                ensure!(!state.native.shared.cancel.load(Ordering::Acquire),"显示器发生变化，连接已关闭");
            }
        }
    }
    Ok(())
}

pub async fn run(listener: crate::transport::Listener, state: AppState) -> Result<()> {
    let shared = state.native.shared.clone();
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
    use crate::desktop::{Command, Shared};
    use crate::platform::filesystem::HomeFiles;
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
                native: DesktopControl {
                    tx,
                    shared: Arc::new(Shared::default()),
                },
                files: FileService::new(HomeFiles::open(&std::env::temp_dir()).unwrap()),
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
        ws.send(WsMessage::Text(
            r#"{"type":"hello","protocol_version":1}"#.into(),
        ))
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
    async fn connection_maintenance_rejects_new_sessions_without_leaking_ownership() {
        let (state, _commands) = state();
        let shared = state.native.shared.clone();
        let maintenance = shared.begin_maintenance().unwrap();
        let (addr, handle) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
            .await
            .unwrap();
        ws.send(WsMessage::Text(
            r#"{"type":"hello","protocol_version":1}"#.into(),
        ))
        .await
        .unwrap();
        let response = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(response.contains("正在保存连接设置"));
        assert!(!shared.active.load(Ordering::Acquire));
        drop(maintenance);
        handle.abort();
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
    async fn missing_heartbeat_releases_session_while_directory_is_blocked() {
        blocked_directory_connection(EndConnection::HeartbeatTimeout).await;
    }

    enum EndConnection {
        Disconnect,
        Close,
        HeartbeatTimeout,
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
                    upgrade.on_upgrade(move |mut socket| async move {
                        state.native.shared.active.store(true, Ordering::Release);
                        let guard = SessionGuard(state.native.clone());
                        let result =
                            control_loop(&mut socket, &state, files, &mut receiver, 0, |_| {
                                Ok((1, 1))
                            })
                            .await;
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
            EndConnection::HeartbeatTimeout => {
                let message = tokio::time::timeout(Duration::from_secs(12), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap();
                assert!(message.contains("file_error") && message.contains("超时"));
                let error = tokio::time::timeout(Duration::from_secs(7), done)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err();
                assert!(error.to_string().contains("连接超时"));
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
}
