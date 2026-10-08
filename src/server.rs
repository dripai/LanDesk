use crate::{
    capture::{Capture, FrameEvent},
    files::{FileSession, HomeFiles},
    native::{Native, permissions},
    protocol::{ClientMessage, PORT, code_matches, origin_allowed},
    settings::CodeSettings,
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
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct AppState {
    pub native: Native,
    pub settings: Arc<Mutex<CodeSettings>>,
    pub attempts: Arc<Mutex<Vec<Instant>>>,
    pub files: Arc<HomeFiles>,
}

struct SessionGuard(Native);
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
        bail!("请先输入连接码");
    };
    let ClientMessage::Hello { code } = serde_json::from_str(&text)? else {
        bail!("请先输入连接码");
    };
    {
        let mut attempts = state
            .attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("认证状态不可用"))?;
        attempts.retain(|t| t.elapsed() < Duration::from_secs(60));
        ensure!(attempts.len() < 5, "连接码错误次数过多，请一分钟后再试");
        let settings = state
            .settings
            .lock()
            .map_err(|_| anyhow::anyhow!("连接码设置不可用"))?;
        if !code_matches(&settings.code, &code) {
            attempts.push(Instant::now());
            bail!("连接码不正确，请查看 Mac 上的 LanDesk 窗口");
        }
        // Serialize session acquisition with code changes, so an old code cannot race an update.
        state
            .native
            .shared
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("已有一个连接，请先断开"))?;
    }
    let _guard = SessionGuard(state.native.clone());
    state.native.shared.cancel.store(false, Ordering::Release);
    let (capture_allowed, input_allowed) = permissions();
    ensure!(
        capture_allowed,
        "请在 Mac 系统设置中授予 LanDesk 屏幕录制权限，然后关闭并重新打开应用"
    );
    ensure!(
        input_allowed,
        "请在 Mac 系统设置中授予 LanDesk 辅助功能权限，然后关闭并重新打开应用"
    );
    let mut capture = tokio::task::spawn_blocking(Capture::start).await??;
    state
        .native
        .begin_input(capture.point_width, capture.point_height)
        .await?;
    // Do not cover the host until capture has produced a real frame.
    tokio::time::timeout(
        Duration::from_secs(8),
        capture.frames.wait_for(|frame| frame.is_some()),
    )
    .await??;
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
    send_json(
        socket,
        serde_json::json!({"type":"ready","width":capture.width,"height":capture.height}),
    )
    .await?;
    tokio::time::timeout(
        Duration::from_secs(3),
        socket.send(Message::Binary(jpeg.as_ref().clone().into())),
    )
    .await??;
    let mut heartbeat = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut events = 0_u32;
    let mut rate_window = Instant::now();
    let mut last_sequence = sequence;
    let files = Arc::new(Mutex::new(FileSession::new(state.files.clone())));
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

                            ClientMessage::Hello { .. } => bail!("重复认证消息"),
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
                                let result = tokio::task::block_in_place(|| capture.set_resolution(width));
                                let response = match result {
                                    Ok(()) => serde_json::json!({"type":"resolution","width":capture.width,"height":capture.height,"requested_width":width}),
                                    Err(error) => serde_json::json!({"type":"resolution_error","message":format!("{error:#}")}),
                                };
                                send_json(socket,response).await?;
                            }
                            message @ (ClientMessage::ListDirectory { .. } | ClientMessage::UploadStart { .. } | ClientMessage::UploadFinish { .. } | ClientMessage::UploadCancel { .. }) => {
                                let files = files.clone();
                                let response = tokio::task::spawn_blocking(move || files.lock().map_err(|_| anyhow::anyhow!("文件会话不可用")).map(|mut files| files.message(message))).await??;
                                send_json(socket,response).await?;
                            }
                            other => state.native.input(other).await?,
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(value) => { socket.send(Message::Pong(value)).await?; }
                    Message::Pong(_) => {}
                    Message::Binary(data) => {
                        let files = files.clone();
                        let response = tokio::task::spawn_blocking(move || files.lock().map_err(|_| anyhow::anyhow!("文件会话不可用")).map(|mut files| files.chunk(&data))).await??;
                        send_json(socket,response).await?;
                    }
                }
            }
            changed = capture.frames.changed() => {
                changed.context("画面采集已停止")?;
                let frame = capture.frames.borrow_and_update().clone();
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
    state.native.end_session().await?;
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
    use crate::native::{Command, Shared};
    use futures_util::StreamExt;
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
                settings: Arc::new(Mutex::new(CodeSettings::fixture("123456"))),
                attempts: Arc::new(Mutex::new(Vec::new())),
                files: Arc::new(HomeFiles::open(std::path::Path::new("/private/tmp")).unwrap()),
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
    async fn file_and_clipboard_operations_require_authentication() {
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
            assert!(response.contains("请先输入连接码"));
        }
        handle.abort();
    }
    #[tokio::test]
    async fn foreign_websites_cannot_open_control_socket() {
        let (state, _rx) = state();
        let (addr, handle) = serve(state).await;
        let error = connect_async(request(addr, "http://foreign.example"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("403"));
        handle.abort();
    }
    #[tokio::test]
    async fn bad_codes_are_rejected_before_capture_and_rate_limited() {
        let (state, _rx) = state();
        let shared = state.native.shared.clone();
        let (addr, handle) = serve(state).await;
        for attempt in 0..6 {
            let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
                .await
                .unwrap();
            ws.send(WsMessage::Text(r#"{"type":"hello","code":"WRONG"}"#.into()))
                .await
                .unwrap();
            let response = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(response.contains(if attempt < 5 {
                "连接码不正确"
            } else {
                "错误次数过多"
            }));
            assert!(!shared.active.load(Ordering::Acquire));
        }
        handle.abort();
    }
    #[tokio::test]
    async fn second_session_does_not_take_over_existing_controller() {
        let (state, _rx) = state();
        state.native.shared.active.store(true, Ordering::Release);
        let (addr, handle) = serve(state).await;
        let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
            .await
            .unwrap();
        ws.send(WsMessage::Text(
            r#"{"type":"hello","code":"123456"}"#.into(),
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
        handle.abort();
    }
    #[tokio::test]
    async fn authentication_uses_updated_code_and_rejects_previous_code() {
        let (state, _rx) = state();
        state.settings.lock().unwrap().code = "001234".into();
        // Avoid capture APIs: the valid code must reach the occupied-session check.
        state.native.shared.active.store(true, Ordering::Release);
        let (addr, handle) = serve(state).await;
        for (code, expected) in [("123456", "连接码不正确"), ("001234", "已有一个连接")]
        {
            let (mut ws, _) = connect_async(request(addr, "http://127.0.0.1:17890"))
                .await
                .unwrap();
            ws.send(WsMessage::Text(
                serde_json::json!({"type":"hello","code":code})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let response = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(response.contains(expected));
        }
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
}
