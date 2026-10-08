use anyhow::{Context, Result};
use hyper::{Request, StatusCode, body::Incoming, service::service_fn, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    task::JoinSet,
    time::{Duration, Instant, timeout},
};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub struct Activity {
    viewers: usize,
    idle_since: Instant,
}

impl Activity {
    pub fn deadline(self) -> Option<Instant> {
        (self.viewers == 0).then_some(self.idle_since + IDLE_TIMEOUT)
    }
}

pub fn activity() -> (watch::Sender<Activity>, watch::Receiver<Activity>) {
    watch::channel(Activity {
        viewers: 0,
        idle_since: Instant::now(),
    })
}

struct Viewer(watch::Sender<Activity>);
impl Viewer {
    fn new(activity: watch::Sender<Activity>) -> Self {
        activity.send_modify(|state| state.viewers += 1);
        Self(activity)
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.send_modify(|state| {
            state.viewers -= 1;
            if state.viewers == 0 {
                state.idle_since = Instant::now();
            }
        });
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn only_last_viewer_starts_idle_deadline_and_refresh_resets_it() {
        let (activity, state) = activity();
        assert_eq!(
            state.borrow().deadline(),
            Some(Instant::now() + IDLE_TIMEOUT)
        );
        let first = Viewer::new(activity.clone());
        let second = Viewer::new(activity.clone());
        tokio::time::advance(Duration::from_secs(120)).await;
        assert!(state.borrow().deadline().is_none());
        drop(first);
        assert!(state.borrow().deadline().is_none());
        drop(second);
        let deadline = state.borrow().deadline().unwrap();
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(Instant::now() < deadline);
        let refreshed = Viewer::new(activity);
        assert!(state.borrow().deadline().is_none());
        drop(refreshed);
        assert_eq!(
            state.borrow().deadline(),
            Some(Instant::now() + IDLE_TIMEOUT)
        );
    }
}
