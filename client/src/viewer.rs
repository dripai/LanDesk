use anyhow::{Context, Result};
use hyper::{Request, StatusCode, body::Incoming, service::service_fn, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::JoinSet,
    time::{Duration, timeout},
};

// Forward HTTP using its framing, then relay an accepted /ws upgrade unchanged.
pub async fn forward<L, R>(local: L, remote: R) -> Result<()>
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
    type Upgrade = (OnUpgrade, OnUpgrade);
    let upgrade: Arc<Mutex<Option<Upgrade>>> = Arc::new(Mutex::new(None));
    let pending = upgrade.clone();
    let service = service_fn(move |mut request: Request<Incoming>| {
        let sender = sender.clone();
        let pending = pending.clone();
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
            .context("远程网页响应超时")??;
            if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                let local_upgrade = local_upgrade.context("不支持的协议升级")?;
                let remote_upgrade = hyper::upgrade::on(&mut response);
                *pending
                    .lock()
                    .map_err(|_| anyhow::anyhow!("远控连接状态不可用"))? =
                    Some((local_upgrade, remote_upgrade));
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
    if let Some((local, remote)) = upgraded {
        let (local, remote) = timeout(Duration::from_secs(10), async {
            tokio::try_join!(local, remote)
        })
        .await
        .context("远控协议升级超时")??;
        tokio::io::copy_bidirectional(&mut TokioIo::new(local), &mut TokioIo::new(remote)).await?;
    }
    Ok(())
}
