use anyhow::Context;
use anyhow::Result;
use hyper::Request;
use hyper::StatusCode;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use landesk_core::application::viewer::{Activity, Viewer};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Duration;
use tokio::time::timeout;

// Forward HTTP using its framing, then relay an accepted /ws upgrade unchanged.
// HTTP keep-alive sockets and failed upgrade requests never keep SSH alive.
pub async fn forward<L, R>(local: L, remote: R, activity: watch::Sender<Activity>) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) =
        hyper::client::conn::http1::handshake::<_, Incoming>(TokioIo::new(remote)).await?;
    // This scope owns every spawned task; cancellation cannot leave a relay running.
    let mut drivers = JoinSet::new();
    drivers.spawn(connection.with_upgrades());
    let sender = Arc::new(tokio::sync::Mutex::new(sender));
    type Upgrade = (OnUpgrade, OnUpgrade, Viewer);
    let upgrade: Arc<Mutex<Option<Upgrade>>> = Arc::new(Mutex::new(None));
    let pending = upgrade.clone();
    let service = service_fn(move |mut request: Request<Incoming>| {
        let sender = sender.clone();
        let pending = pending.clone();
        let activity = activity.clone();
        async move {
            let is_viewer = request.method() == hyper::Method::GET
                && request.uri().path() == "/ws"
                && request
                    .headers()
                    .get(hyper::header::UPGRADE)
                    .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
            let local_upgrade = is_viewer.then(|| hyper::upgrade::on(&mut request));
            let mut response = timeout(
                Duration::from_secs(10),
                sender.lock().await.send_request(request),
            )
            .await
            .context("Mac 网页响应超时")??;
            if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                let local_upgrade = local_upgrade.context("不支持的协议升级")?;
                let remote_upgrade = hyper::upgrade::on(&mut response);
                *pending
                    .lock()
                    .map_err(|_| anyhow::anyhow!("远控连接状态不可用"))? =
                    Some((local_upgrade, remote_upgrade, Viewer::new(activity)));
            }
            Ok::<_, anyhow::Error>(response)
        }
    });
    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(local), service)
        .with_upgrades()
        .await?;
    let upgraded = upgrade
        .lock()
        .map_err(|_| anyhow::anyhow!("远控连接状态不可用"))?
        .take();
    if let Some((local, remote, _viewer)) = upgraded {
        let (local, remote) = timeout(Duration::from_secs(10), async {
            tokio::try_join!(local, remote)
        })
        .await
        .context("远控协议升级超时")??;
        tokio::io::copy_bidirectional(&mut TokioIo::new(local), &mut TokioIo::new(remote)).await?;
    }
    Ok(())
}
