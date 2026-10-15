use crate::{tunnel::Handler, viewer};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes, service::service_fn};
use hyper_util::rt::TokioIo;
use russh::{Channel, ChannelMsg, client};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    time::{Duration, timeout},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};

const WAIT: Duration = Duration::from_secs(10);
const OUTPUT_WINDOW: usize = 128 * 1024;
pub(crate) type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

pub(crate) struct Page {
    token: String,
}
impl Page {
    pub fn new() -> Result<Self> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("无法生成终端会话凭据：{e}"))?;
        Ok(Self {
            token: random.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }
}

fn response(status: StatusCode, mime: &str, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder().status(status)
        .header("content-type", mime)
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff")
        .header("referrer-policy", "no-referrer")
        .header("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
        .body(Full::new(body.into())).unwrap()
}

pub(crate) async fn serve<S>(
    stream: S,
    ssh: Arc<client::Handle<Handler>>,
    activity: watch::Sender<viewer::Activity>,
    page: Arc<Page>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let pending = Arc::new(Mutex::new(None));
    let slot = pending.clone();
    let content = page.clone();
    let service = service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let slot = slot.clone();
        let page = content.clone();
        async move {
            let host = request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let origin = request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok());
            let allowed = host == crate::gateway::INTERNAL_HOST;
            let result = if !allowed || origin.is_some_and(|v| v != format!("http://{host}")) {
                response(
                    StatusCode::FORBIDDEN,
                    "text/plain; charset=utf-8",
                    "不允许此网页来源",
                )
            } else if request.method() != hyper::Method::GET {
                response(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "GET required")
            } else {
                match request.uri().path() {
                    "/" => response(
                        StatusCode::OK,
                        "text/html; charset=utf-8",
                        include_str!("../web/index.html").replace("__SESSION_TOKEN__", &page.token),
                    ),
                    "/files.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_str!("../../web/files.js"),
                    ),
                    "/toolbar.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_str!("../../web/toolbar.js"),
                    ),
                    "/shared.css" => response(
                        StatusCode::OK,
                        "text/css; charset=utf-8",
                        include_str!("../../web/style.css"),
                    ),
                    "/terminal.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_str!("../web/terminal.js"),
                    ),
                    "/protocol.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_str!("../web/protocol.js"),
                    ),
                    "/terminal.css" => response(
                        StatusCode::OK,
                        "text/css; charset=utf-8",
                        include_str!("../web/terminal.css"),
                    ),
                    "/vendor/xterm.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_bytes!("../web/vendor/xterm.js").as_slice(),
                    ),
                    "/vendor/addon-fit.js" => response(
                        StatusCode::OK,
                        "text/javascript; charset=utf-8",
                        include_bytes!("../web/vendor/addon-fit.js").as_slice(),
                    ),
                    "/vendor/xterm.css" => response(
                        StatusCode::OK,
                        "text/css; charset=utf-8",
                        include_str!("../web/vendor/xterm.css"),
                    ),
                    "/ws" | "/files-ws" if origin.is_some() => {
                        let handshake = Request::builder()
                            .method(request.method())
                            .uri(request.uri());
                        let mut handshake = handshake.body(())?;
                        *handshake.headers_mut() = request.headers().clone();
                        match tokio_tungstenite::tungstenite::handshake::server::create_response(
                            &handshake,
                        ) {
                            Ok(reply) => {
                                *slot
                                    .lock()
                                    .map_err(|_| anyhow::anyhow!("终端升级状态不可用"))? = Some((
                                    request.uri().path() == "/files-ws",
                                    hyper::upgrade::on(&mut request),
                                ));
                                reply.map(|_| Full::new(Bytes::new()))
                            }
                            Err(_) => response(
                                StatusCode::BAD_REQUEST,
                                "text/plain",
                                "Invalid WebSocket upgrade",
                            ),
                        }
                    }
                    "/ws" | "/files-ws" => {
                        response(StatusCode::FORBIDDEN, "text/plain", "Origin required")
                    }
                    _ => response(StatusCode::NOT_FOUND, "text/plain", "Not found"),
                }
            };
            Ok::<_, anyhow::Error>(result)
        }
    });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await?;
    let upgrade = pending
        .lock()
        .map_err(|_| anyhow::anyhow!("终端升级状态不可用"))?
        .take();
    if let Some((files, upgrade)) = upgrade {
        let io = timeout(WAIT, upgrade).await.context("终端网页升级超时")??;
        let config = WebSocketConfig::default()
            .max_message_size(Some(64 * 1024))
            .max_frame_size(Some(64 * 1024));
        let mut socket =
            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(config)).await;
        let result = if files {
            crate::terminal_files::run(&mut socket, ssh, activity, &page.token).await
        } else {
            run(&mut socket, ssh, activity, &page.token).await
        };
        if let Err(error) = &result {
            let _ = send_json(
                &mut socket,
                serde_json::json!({"type":"error", "message":format!("{error:#}")}),
            )
            .await;
        }
        let _ = timeout(WAIT, socket.close(None)).await;
        result?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Hello { token: String, cols: u32, rows: u32 },
    Resize { cols: u32, rows: u32 },
    Ack { bytes: usize },
    Disconnect,
}
fn dimensions(cols: u32, rows: u32) -> Result<()> {
    ensure!(
        (2..=500).contains(&cols) && (1..=300).contains(&rows),
        "终端尺寸超出范围"
    );
    Ok(())
}
async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    timeout(WAIT, socket.send(message))
        .await
        .context("终端网页写入超时")??;
    Ok(())
}
pub(crate) async fn send_json(socket: &mut Socket, value: serde_json::Value) -> Result<()> {
    send(socket, Message::Text(value.to_string().into())).await
}

async fn run(
    socket: &mut Socket,
    ssh: Arc<client::Handle<Handler>>,
    activity: watch::Sender<viewer::Activity>,
    token: &str,
) -> Result<()> {
    let message = timeout(WAIT, socket.next())
        .await
        .context("终端握手超时")?
        .context("网页已关闭")??;
    let Message::Text(text) = message else {
        bail!("缺少终端握手");
    };
    let Control::Hello {
        token: supplied,
        cols,
        rows,
    } = serde_json::from_str(&text).context("无效的终端握手")?
    else {
        bail!("缺少终端握手");
    };
    ensure!(supplied == token, "终端会话凭据已失效，请刷新页面");
    dimensions(cols, rows)?;
    let _viewer = viewer::Viewer::new(activity);
    // The SSH task owns every terminal. Cancelling that task disconnects SSH as well.
    let mut channel = timeout(WAIT, ssh.channel_open_session())
        .await
        .context("SSH 会话创建超时")??;
    let result = async {
        timeout(WAIT, async {
            channel
                .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
                .await?;
            expect_success(&mut channel, socket, "服务器拒绝分配 PTY").await?;
            channel.request_shell(true).await?;
            expect_success(&mut channel, socket, "服务器拒绝启动 Shell").await
        })
        .await
        .context("终端初始化超时")??;
        send_json(socket, serde_json::json!({"type":"ready"})).await?;
        relay(&mut channel, socket).await
    }
    .await;
    // A closed browser must close only its own PTY, including failed initialization.
    let cleanup = timeout(WAIT, channel.close())
        .await
        .context("关闭 SSH 终端超时")?;
    result?;
    cleanup?;
    Ok(())
}

async fn expect_success(
    channel: &mut Channel<client::Msg>,
    socket: &mut Socket,
    failure: &str,
) -> Result<()> {
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Ping(data))) => send(socket, Message::Pong(data)).await?,
                _ => bail!("终端初始化期间网页已关闭或发送了无效消息"),
            },
            reply = channel.wait() => match reply {
                Some(ChannelMsg::Success) => return Ok(()),
                Some(ChannelMsg::WindowAdjusted { .. }) => {},
                _ => bail!("{failure}"),
            },
        }
    }
}

async fn relay(channel: &mut Channel<client::Msg>, socket: &mut Socket) -> Result<()> {
    let mut pending = 0usize;
    let mut exit = None;
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                None | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Err(error)) => return Err(error.into()),
                Some(Ok(Message::Binary(data))) => {
                    ensure!(data.len() <= 16 * 1024, "单次终端输入过大");
                    timeout(WAIT, channel.data_bytes(data)).await.context("SSH 终端输入超时")??;
                },
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Control>(&text).context("无效的终端控制消息")? {
                    Control::Resize { cols, rows } => {
                        dimensions(cols, rows)?;
                        timeout(WAIT, channel.window_change(cols, rows, 0, 0)).await.context("终端尺寸更新超时")??;
                    },
                    Control::Ack { bytes } => {
                        ensure!(bytes <= pending, "无效的终端输出确认");
                        pending -= bytes;
                    },
                    Control::Disconnect => return Ok(()),
                    Control::Hello { .. } => bail!("不能重复初始化终端"),
                },
                Some(Ok(Message::Ping(data))) => send(socket, Message::Pong(data)).await?,
                Some(Ok(Message::Pong(_))) => {},
                Some(Ok(_)) => bail!("不支持的终端消息"),
            },
            message = channel.wait(), if pending < OUTPUT_WINDOW => match message {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    pending += data.len();
                    send(socket, Message::Binary(data)).await?;
                },
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(serde_json::json!({"type":"exit", "code":exit_status})),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => exit = Some(serde_json::json!({"type":"exit", "signal":format!("{signal_name:?}")})),
                None | Some(ChannelMsg::Close) => {
                    send_json(socket, exit.unwrap_or_else(|| serde_json::json!({"type":"exit"}))).await?;
                    return Ok(());
                },
                Some(ChannelMsg::Eof | ChannelMsg::WindowAdjusted { .. }) => {},
                Some(_) => {},
            },
        }
    }
}
