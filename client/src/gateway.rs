use anyhow::{Context, Result, ensure};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};
use tokio::{
    io::DuplexStream,
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::{Duration, timeout},
};

pub type Routes = Arc<RwLock<HashMap<String, mpsc::Sender<DuplexStream>>>>;
type Body = BoxBody<Bytes, hyper::Error>;
pub const PORT: u16 = 17890;

fn message(status: StatusCode, text: &str) -> Response<Body> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>LanDesk</title><p>{text}</p><p><a href=''>重新连接</a></p>"
    );
    Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .header(
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        )
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

fn route_path(path: &str) -> Option<(&str, &str)> {
    let (id, path) = path.strip_prefix("/s/")?.split_once('/')?;
    (id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some((id, path))
}

pub async fn run(
    listener: TcpListener,
    routes: Routes,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => return Ok(()),
            result = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(error)) = result { return Err(error).context("网页入口任务异常"); }
                // A browser reset closes just that connection.
            }
            accepted = listener.accept(), if clients.len() < 256 => {
                let (socket, _) = accepted?;
                let routes = routes.clone();
                clients.spawn(async move { serve(socket, routes).await });
            }
        }
    }
}

async fn serve(socket: tokio::net::TcpStream, routes: Routes) -> Result<()> {
    let drivers = Arc::new(Mutex::new(JoinSet::new()));
    let pending = Arc::new(Mutex::new(None));
    let upgrade = pending.clone();
    let jobs = drivers.clone();
    let service = service_fn(move |mut request: Request<Incoming>| {
        let routes = routes.clone();
        let pending = pending.clone();
        let jobs = jobs.clone();
        async move {
            let host = request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let allowed_host = host == "127.0.0.1:17890" || host == "localhost:17890";
            let origin_ok = request
                .headers()
                .get("origin")
                .is_none_or(|v| v.to_str().ok() == Some(format!("http://{host}").as_str()));
            if !allowed_host
                || !origin_ok
                || (request.headers().contains_key(hyper::header::UPGRADE)
                    && !request.headers().contains_key("origin"))
            {
                return Ok::<_, anyhow::Error>(message(StatusCode::FORBIDDEN, "不允许此网页来源"));
            }
            let Some((id, path)) = route_path(request.uri().path()) else {
                return Ok(message(
                    StatusCode::NOT_FOUND,
                    "请先在 LanDeskClient 中连接服务器。",
                ));
            };
            let target = routes
                .read()
                .map_err(|_| anyhow::anyhow!("连接列表不可用"))?
                .get(id)
                .cloned();
            let Some(target) = target else {
                return Ok(message(
                    StatusCode::CONFLICT,
                    "此服务器连接已关闭，请在 LanDeskClient 中重新连接，然后刷新此标签。",
                ));
            };
            let path = format!(
                "/{}{}",
                path,
                request
                    .uri()
                    .query()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default()
            );
            *request.uri_mut() = path.parse()?;
            // Preserve the loopback origin check on the Mac; only the path is rewritten.
            let is_upgrade = request.headers().contains_key(hyper::header::UPGRADE);
            let local_upgrade = is_upgrade.then(|| hyper::upgrade::on(&mut request));
            let (local, remote) = tokio::io::duplex(65536);
            if target.try_send(remote).is_err() {
                return Ok(message(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "连接暂不可用，请稍后重新连接。",
                ));
            }
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake::<_, Incoming>(TokioIo::new(local)).await?;
            {
                let mut jobs = jobs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("网页转发任务不可用"))?;
                while jobs.try_join_next().is_some() {}
                jobs.spawn(connection.with_upgrades());
            }
            let mut response =
                match timeout(Duration::from_secs(45), sender.send_request(request)).await {
                    Ok(Ok(response)) => response,
                    _ => {
                        return Ok(message(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "远程服务器暂不可达，请确认网络及服务端运行状态后重新连接。",
                        ));
                    }
                };
            if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                *pending
                    .lock()
                    .map_err(|_| anyhow::anyhow!("网页升级状态不可用"))? = Some((
                    local_upgrade.context("未请求协议升级")?,
                    hyper::upgrade::on(&mut response),
                ));
            }
            Ok(response.map(BodyExt::boxed))
        }
    });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(socket), service)
        .with_upgrades()
        .await?;
    let pair = upgrade
        .lock()
        .map_err(|_| anyhow::anyhow!("网页升级状态不可用"))?
        .take();
    if let Some((local, remote)) = pair {
        let (local, remote) = timeout(Duration::from_secs(10), async {
            tokio::try_join!(local, remote)
        })
        .await??;
        tokio::io::copy_bidirectional(&mut TokioIo::new(local), &mut TokioIo::new(remote)).await?;
    }
    drop(drivers);
    Ok(())
}

pub struct Registration {
    pub id: String,
    pub routes: Routes,
    pub sender: mpsc::Sender<DuplexStream>,
    registered: bool,
}
impl Registration {
    pub fn new(id: String, routes: Routes, sender: mpsc::Sender<DuplexStream>) -> Self {
        Self {
            id,
            routes,
            sender,
            registered: false,
        }
    }
    pub fn is_active(&self) -> bool {
        self.registered
    }
    pub fn activate(&mut self) -> Result<()> {
        if self.registered {
            return Ok(());
        }
        let mut routes = self
            .routes
            .write()
            .map_err(|_| anyhow::anyhow!("连接列表不可用"))?;
        ensure!(!routes.contains_key(&self.id), "此服务器已有连接");
        routes.insert(self.id.clone(), self.sender.clone());
        self.registered = true;
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if self.registered {
            match self.routes.write() {
                Ok(mut routes) => {
                    routes.remove(&self.id);
                }
                Err(error) => eprintln!("清理服务器路由失败：{error}"),
            }
        }
    }
}
