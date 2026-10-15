use crate::domain::connection::{ConnectionProfile, Event, StopReason};
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;
pub trait ClientGatewayFactory: Send + Sync {
    fn bind(&self) -> BoxFuture<'_, Result<Arc<dyn ClientGateway>>>;
}
pub trait ClientGateway: Send + Sync {
    fn viewer_url(&self, profile: &ConnectionProfile) -> Result<String>;
    fn serve(self: Arc<Self>, cancel: oneshot::Receiver<()>) -> BoxFuture<'static, Result<()>>;
    fn connect(
        self: Arc<Self>,
        profile: ConnectionProfile,
        password: Zeroizing<String>,
        known_hosts: PathBuf,
        events: mpsc::Sender<Event>,
        cancel: oneshot::Receiver<()>,
    ) -> BoxFuture<'static, Result<StopReason>>;
}
