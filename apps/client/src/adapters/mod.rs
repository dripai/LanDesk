//! Client composition: the shared lifecycle sees gateway/SSH ports only.

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use landesk_core::{
    domain::connection::{ConnectionProfile, Event, StopReason},
    ports::client::{ClientGateway, ClientGatewayFactory},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};
use tokio::{net::TcpListener, sync::oneshot};
use zeroize::Zeroizing;
pub struct DesktopGatewayFactory;
struct DesktopGateway {
    listener: Mutex<Option<TcpListener>>,
    endpoint: gateway::Endpoint,
    routes: gateway::Routes,
}
impl ClientGatewayFactory for DesktopGatewayFactory {
    fn bind(&self) -> BoxFuture<'_, Result<Arc<dyn ClientGateway>>> {
        Box::pin(async {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let endpoint = gateway::Endpoint::new(listener.local_addr()?)?;
            Ok(Arc::new(DesktopGateway {
                listener: Mutex::new(Some(listener)),
                endpoint,
                routes: gateway::Routes::default(),
            }) as Arc<dyn ClientGateway>)
        })
    }
}
impl ClientGateway for DesktopGateway {
    fn viewer_url(&self, profile: &ConnectionProfile) -> Result<String> {
        self.endpoint.viewer_url(&self.routes, profile)
    }
    fn serve(self: Arc<Self>, cancel: oneshot::Receiver<()>) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let listener = self
                .listener
                .lock()
                .map_err(|_| anyhow::anyhow!("网页入口状态损坏"))?
                .take()
                .context("网页入口已启动")?;
            gateway::run(listener, self.routes.clone(), cancel).await
        })
    }
    fn connect(
        self: Arc<Self>,
        profile: ConnectionProfile,
        password: Zeroizing<String>,
        known_hosts: PathBuf,
        events: mpsc::Sender<Event>,
        cancel: oneshot::Receiver<()>,
    ) -> BoxFuture<'static, Result<StopReason>> {
        Box::pin(async move {
            tunnel::run(
                profile,
                password,
                known_hosts,
                events,
                cancel,
                self.routes.clone(),
            )
            .await
        })
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub mod credentials;
pub mod gateway;
pub mod settings;
mod terminal;
mod terminal_files;
pub mod tunnel;
mod viewer;
