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

#[derive(Clone)]
pub struct Route {
    sender: mpsc::Sender<DuplexStream>,
    token: String,
}
pub type Routes = Arc<RwLock<HashMap<String, Route>>>;

/// Runtime browser address. Never persisted in server profiles.
#[derive(Clone)]
pub struct Endpoint {
    authority: String,
    cookie: String,
}
impl Endpoint {
    pub fn new(address: std::net::SocketAddr) -> Result<Self> {
        ensure!(
            address.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                && address.port() != 0,
            "网页入口必须使用 IPv4 回环地址和已分配端口"
        );
        Ok(Self {
            authority: address.to_string(),
            cookie: format!("landesk_{}", address.port()),
        })
    }
    pub fn viewer_url(
        &self,
        routes: &Routes,
        settings: &crate::settings::Settings,
    ) -> Result<String> {
        let id = settings.id();
        let routes = routes
            .read()
            .map_err(|_| anyhow::anyhow!("连接列表不可用"))?;
        let route = routes.get(&id).context("连接尚未就绪")?;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("access", &route.token)
            .finish();
        let name = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", settings.label())
            .finish();
        Ok(format!("http://{}/s/{id}/?{query}#{name}", self.authority))
    }
}
fn matches_token(actual: &str, expected: &str) -> bool {
    expected
        .bytes()
        .enumerate()
        .fold(actual.len() ^ expected.len(), |diff, (i, b)| {
            diff | usize::from(b ^ actual.as_bytes().get(i).copied().unwrap_or(0))
        })
        == 0
}
type Body = BoxBody<Bytes, hyper::Error>;
pub use crate::desktop_protocol::INTERNAL_HOST;

fn message(status: StatusCode, text: &str) -> Response<Body> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>LanDesk</title><p>{text}</p>"
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
    let endpoint = Endpoint::new(listener.local_addr()?)?;
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
                let endpoint = endpoint.clone();
                clients.spawn(async move { serve(socket, routes, endpoint).await });
            }
        }
    }
}

async fn serve(socket: tokio::net::TcpStream, routes: Routes, endpoint: Endpoint) -> Result<()> {
    let drivers = Arc::new(Mutex::new(JoinSet::new()));
    let pending = Arc::new(Mutex::new(None));
    let upgrade = pending.clone();
    let jobs = drivers.clone();
    let service = service_fn(move |mut request: Request<Incoming>| {
        let routes = routes.clone();
        let pending = pending.clone();
        let jobs = jobs.clone();
        let endpoint = endpoint.clone();
        async move {
            let host = request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let allowed_host = host == endpoint.authority;
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
                    "请在 LanDeskClient 中选择服务器并连接。",
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
            // Exchange a short-lived launch credential for an HttpOnly session cookie.
            // Only the exact route root accepts bootstrap credentials; never proxy them.
            if let Some(query) = request.uri().query().filter(|_| path.is_empty()) {
                let pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
                if request.method() != hyper::Method::GET
                    || pairs.len() != 1
                    || pairs[0].0 != "access"
                    || !matches_token(&pairs[0].1, &target.token)
                {
                    return Ok(message(StatusCode::FORBIDDEN, "网页会话凭证无效"));
                }
                let mut response = message(StatusCode::SEE_OTHER, "正在打开连接");
                response
                    .headers_mut()
                    .insert(hyper::header::LOCATION, format!("/s/{id}/").parse()?);
                response.headers_mut().insert(
                    hyper::header::SET_COOKIE,
                    format!(
                        "{}={}; Path=/s/{id}/; HttpOnly; SameSite=Strict",
                        endpoint.cookie, target.token
                    )
                    .parse()?,
                );
                response
                    .headers_mut()
                    .insert(hyper::header::REFERRER_POLICY, "no-referrer".parse()?);
                return Ok(response);
            }
            let authenticated = request
                .headers()
                .get_all(hyper::header::COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(';'))
                .filter_map(|v| v.trim().split_once('='))
                .any(|(name, value)| {
                    name == endpoint.cookie && matches_token(value, &target.token)
                });
            if !authenticated {
                return Ok(message(
                    StatusCode::UNAUTHORIZED,
                    "网页会话已失效，请从客户端打开连接",
                ));
            }
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
            // Browser origin was checked above. Internal HTTP has a separate identity.
            request
                .headers_mut()
                .insert(hyper::header::HOST, INTERNAL_HOST.parse()?);
            if request.headers().contains_key(hyper::header::ORIGIN) {
                request.headers_mut().insert(
                    hyper::header::ORIGIN,
                    crate::desktop_protocol::INTERNAL_ORIGIN.parse()?,
                );
            }
            request.headers_mut().remove(hyper::header::COOKIE);
            let is_upgrade = request.headers().contains_key(hyper::header::UPGRADE);
            let local_upgrade = is_upgrade.then(|| hyper::upgrade::on(&mut request));
            let (local, remote) = tokio::io::duplex(65536);
            if target.sender.try_send(remote).is_err() {
                return Ok(message(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "连接暂不可用，请在客户端检查状态后重试。",
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
            let mut response = timeout(Duration::from_secs(12), sender.send_request(request))
                .await
                .context("服务器网页响应超时")??;
            if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                *pending
                    .lock()
                    .map_err(|_| anyhow::anyhow!("网页升级状态不可用"))? = Some((
                    local_upgrade.context("未请求协议升级")?,
                    hyper::upgrade::on(&mut response),
                ));
            }
            response
                .headers_mut()
                .insert(hyper::header::REFERRER_POLICY, "no-referrer".parse()?);
            response
                .headers_mut()
                .insert(hyper::header::CACHE_CONTROL, "no-store".parse()?);
            response.headers_mut().remove(hyper::header::SET_COOKIE);
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
    token: String,
}
impl Registration {
    pub fn new(id: String, routes: Routes, sender: mpsc::Sender<DuplexStream>) -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow::anyhow!("无法生成网页会话凭证：{error}"))?;
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            id,
            routes,
            sender,
            registered: false,
            token,
        })
    }
    pub fn activate(&mut self) -> Result<()> {
        let mut routes = self
            .routes
            .write()
            .map_err(|_| anyhow::anyhow!("连接列表不可用"))?;
        ensure!(!routes.contains_key(&self.id), "此服务器已有连接");
        routes.insert(
            self.id.clone(),
            Route {
                sender: self.sender.clone(),
                token: self.token.clone(),
            },
        );
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

#[cfg(test)]
pub(crate) fn test_cookie(routes: &Routes, id: &str, port: u16) -> String {
    format!(
        "landesk_{port}={}",
        routes.read().unwrap().get(id).unwrap().token
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Empty;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn request(
        address: std::net::SocketAddr,
        path: &str,
        host: &str,
        cookie: &str,
        origin: Option<&str>,
    ) -> (StatusCode, hyper::HeaderMap, String) {
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .unwrap();
        let task = tokio::spawn(driver);
        let mut builder = Request::builder()
            .uri(path)
            .header("host", host)
            .header("cookie", cookie);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let response = sender
            .send_request(builder.body(Empty::<Bytes>::new()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        task.abort();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn dynamic_gateway_authenticates_bootstrap_origin_and_revokes_reconnected_sessions() {
        let routes = Routes::default();
        let profile = crate::settings::Settings {
            host: "example.test".into(),
            ..Default::default()
        };
        let id = profile.id();
        let (sender, mut receiver) = mpsc::channel::<DuplexStream>(4);
        let mut registration =
            Registration::new(id.clone(), routes.clone(), sender.clone()).unwrap();
        registration.activate().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = Endpoint::new(address).unwrap();
        let (stop, stopped) = oneshot::channel();
        let gateway = tokio::spawn(run(listener, routes.clone(), stopped));
        let path = format!("/s/{id}/");
        let url = url::Url::parse(&endpoint.viewer_url(&routes, &profile).unwrap()).unwrap();
        let bootstrap = format!("{}?{}", url.path(), url.query().unwrap());
        let authority = address.to_string();
        assert_eq!(
            request(address, &path, &authority, "", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(address, &bootstrap, "evil.test", "", None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                address,
                &bootstrap,
                &authority,
                "",
                Some("http://evil.test")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, headers, _) = request(address, &bootstrap, &authority, "", None).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[hyper::header::LOCATION], path);
        let set_cookie = headers[hyper::header::SET_COOKIE].to_str().unwrap();
        assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        assert_eq!(
            request(
                address,
                &path,
                &authority,
                &cookie,
                Some("http://127.0.0.1:1")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let worker = tokio::spawn(async move {
            let mut stream = receiver.recv().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
            let text = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(text.starts_with("get / http/1.1"));
            assert!(text.contains("host: landesk.internal"));
            assert!(!text.contains("cookie:") && !text.contains("access="));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        assert_eq!(
            request(address, &path, &authority, &cookie, None).await.2,
            "ok"
        );
        worker.await.unwrap();
        drop(registration);
        assert_eq!(
            request(address, &path, &authority, &cookie, None).await.0,
            StatusCode::CONFLICT
        );
        let mut replacement = Registration::new(id, routes.clone(), sender).unwrap();
        replacement.activate().unwrap();
        assert_eq!(
            request(address, &path, &authority, &cookie, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(address, &bootstrap, &authority, "", None).await.0,
            StatusCode::FORBIDDEN
        );
        stop.send(()).unwrap();
        gateway.await.unwrap().unwrap();
    }
}
