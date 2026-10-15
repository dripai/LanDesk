//! Connection lifecycle shared by the UI and tests. No UI or credential storage dependencies.
use crate::domain::connection::ConnectionProfile;
use crate::domain::connection::{Event, StopReason};
use crate::ports::client::{ClientGateway, ClientGatewayFactory};
use anyhow::Result;
use anyhow::ensure;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Connected,
    Disconnecting,
    Stopped,
}

pub struct Session {
    cancel: Option<oneshot::Sender<()>>,
    generation: u64,
    pub phase: Phase,
    pub status: String,
}
impl Session {
    pub fn is_busy(&self) -> bool {
        self.phase != Phase::Stopped
    }
    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Connected
    }
    fn disconnect(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
            self.phase = Phase::Disconnecting;
            self.status = "正在断开…".into();
        }
    }
}

enum Update {
    Session {
        id: String,
        generation: u64,
        event: Event,
    },
    Gateway {
        epoch: u64,
        error: String,
    },
}

pub enum Notification {
    Changed,
    Ready(String),
    GatewayFailed(String),
}

pub struct ConnectionManager {
    runtime: Arc<Runtime>,
    events: mpsc::Receiver<Update>,
    sender: mpsc::Sender<Update>,
    sessions: HashMap<String, Session>,
    factory: Arc<dyn ClientGatewayFactory>,
    gateway_stop: Option<oneshot::Sender<()>>,
    gateway: Option<Arc<dyn ClientGateway>>,
    gateway_epoch: u64,
    generation: u64,
    closing: bool,
    failed: bool,
}
impl ConnectionManager {
    pub fn new(runtime: Arc<Runtime>, factory: Arc<dyn ClientGatewayFactory>) -> Self {
        let (sender, events) = mpsc::channel();
        Self {
            runtime,
            events,
            sender,
            sessions: HashMap::new(),
            factory,
            gateway_stop: None,
            gateway: None,
            gateway_epoch: 0,
            generation: 0,
            closing: false,
            failed: false,
        }
    }
    pub fn start_gateway(&mut self) -> Result<()> {
        ensure!(
            !self.closing && self.is_idle() && self.gateway_stop.is_none(),
            "网页入口不可重复启动"
        );
        let gateway = self.runtime.block_on(self.factory.bind())?;
        self.gateway_epoch = self
            .gateway_epoch
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("网页入口编号已耗尽"))?;
        let epoch = self.gateway_epoch;
        self.gateway = Some(gateway.clone());
        self.failed = false;
        let (stop, cancelled) = oneshot::channel();
        self.gateway_stop = Some(stop);
        let events = self.sender.clone();
        self.runtime.spawn(async move {
            let result = gateway.serve(cancelled).await;
            // A requested shutdown is no longer polled; every unexpected exit is fatal.
            let message = match result {
                Ok(()) => "网页入口已停止".into(),
                Err(error) => format!("网页入口已停止：{error:#}"),
            };
            let _ = events.send(Update::Gateway {
                epoch,
                error: message,
            });
        });
        Ok(())
    }
    pub fn viewer_url(&self, settings: &ConnectionProfile) -> Result<String> {
        self.gateway
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("网页入口未启动"))?
            .viewer_url(settings)
    }
    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }
    pub fn is_idle(&self) -> bool {
        !self.sessions.values().any(Session::is_busy)
    }
    pub fn forget(&mut self, id: &str) -> Result<()> {
        ensure!(
            !self.sessions.get(id).is_some_and(Session::is_busy),
            "请先断开连接"
        );
        self.sessions.remove(id);
        Ok(())
    }
    fn begin(&mut self, id: String) -> Result<(u64, oneshot::Receiver<()>)> {
        ensure!(!self.closing && !self.failed, "连接管理器已停止");
        ensure!(
            !self.sessions.get(&id).is_some_and(Session::is_busy),
            "连接尚未结束"
        );
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("连接编号已耗尽"))?;
        let (cancel, cancelled) = oneshot::channel();
        self.sessions.insert(
            id,
            Session {
                cancel: Some(cancel),
                generation: self.generation,
                phase: Phase::Connecting,
                status: "正在连接…".into(),
            },
        );
        Ok((self.generation, cancelled))
    }
    pub fn connect(
        &mut self,
        settings: ConnectionProfile,
        password: Zeroizing<String>,
        known_hosts: PathBuf,
    ) -> Result<()> {
        ensure!(self.gateway_stop.is_some(), "网页入口尚未启动");
        settings.validate()?;
        let gateway = self
            .gateway
            .clone()
            .ok_or_else(|| anyhow::anyhow!("网页入口未启动"))?;
        let id = settings.id();
        let (generation, cancelled) = self.begin(id.clone())?;
        let events = self.sender.clone();
        self.runtime.spawn(async move {
            let (tx, rx) = mpsc::channel();
            let task = gateway.connect(settings, password, known_hosts, tx, cancelled);
            tokio::pin!(task);
            let mut tick = tokio::time::interval(Duration::from_millis(30));
            let send = |event| {
                let _ = events.send(Update::Session {
                    id: id.clone(),
                    generation,
                    event,
                });
            };
            loop {
                tokio::select! {
                    result = &mut task => {
                        while let Ok(event) = rx.try_recv() { send(event); }
                        send(Event::Stopped(result)); break;
                    }
                    _ = tick.tick() => { while let Ok(event) = rx.try_recv() { send(event); } }
                }
            }
        });
        Ok(())
    }
    pub fn disconnect(&mut self, id: &str) {
        if let Some(session) = self.sessions.get_mut(id) {
            session.disconnect();
        }
    }
    pub fn disconnect_all(&mut self) {
        for session in self.sessions.values_mut() {
            session.disconnect();
        }
    }
    pub fn shutdown(&mut self) {
        self.closing = true;
        self.disconnect_all();
        if self.is_idle()
            && let Some(stop) = self.gateway_stop.take()
        {
            let _ = stop.send(());
        }
    }
    fn apply(&mut self, update: Update) -> Option<Notification> {
        match update {
            Update::Gateway { epoch, error } => {
                if epoch != self.gateway_epoch {
                    return None;
                }
                self.gateway = None;
                self.failed = true;
                self.gateway_stop = None;
                self.disconnect_all();
                Some(Notification::GatewayFailed(error))
            }
            Update::Session {
                id,
                generation,
                event,
            } => {
                let session = self.sessions.get_mut(&id)?;
                if session.generation != generation || !session.is_busy() {
                    return None;
                }
                match event {
                    Event::Ready if session.phase == Phase::Connecting => {
                        session.phase = Phase::Connected;
                        session.status = "已连接 · 无网页会话 30 秒后断开".into();
                        return Some(Notification::Ready(id));
                    }
                    Event::Ready => return None,
                    Event::Status(status) if session.phase != Phase::Disconnecting => {
                        session.status = status
                    }
                    Event::Status(_) => return None,
                    Event::Stopped(result) => {
                        session.cancel = None;
                        session.phase = Phase::Stopped;
                        session.status = match result {
                            Ok(StopReason::Disconnected) => "已断开".into(),
                            Ok(StopReason::IdleTimeout) => {
                                "空闲 30 秒，SSH 已关闭，请重新连接".into()
                            }
                            Err(error) => format!("{error:#}"),
                        };
                    }
                }
                Some(Notification::Changed)
            }
        }
    }
    pub fn poll(&mut self) -> Vec<Notification> {
        let mut notifications = Vec::new();
        while let Ok(update) = self.events.try_recv() {
            if let Some(notification) = self.apply(update) {
                notifications.push(notification);
            }
        }
        if self.closing {
            self.shutdown();
        }
        // A worker may become ready and stop between two UI polls.
        notifications.retain(|notification| match notification {
            Notification::Ready(id) => self.get(id).is_some_and(Session::is_ready),
            _ => true,
        });
        notifications
    }
}
impl Drop for ConnectionManager {
    fn drop(&mut self) {
        self.disconnect_all();
        if let Some(stop) = self.gateway_stop.take() {
            let _ = stop.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeFactory;
    struct FakeGateway;
    impl ClientGatewayFactory for FakeFactory {
        fn bind(&self) -> futures_util::future::BoxFuture<'_, Result<Arc<dyn ClientGateway>>> {
            Box::pin(async { Ok(Arc::new(FakeGateway) as Arc<dyn ClientGateway>) })
        }
    }
    impl ClientGateway for FakeGateway {
        fn viewer_url(&self, _: &ConnectionProfile) -> Result<String> {
            Ok("http://127.0.0.1/test".into())
        }
        fn serve(
            self: Arc<Self>,
            cancel: oneshot::Receiver<()>,
        ) -> futures_util::future::BoxFuture<'static, Result<()>> {
            Box::pin(async {
                let _ = cancel.await;
                Ok(())
            })
        }
        fn connect(
            self: Arc<Self>,
            _: ConnectionProfile,
            _: Zeroizing<String>,
            _: PathBuf,
            _: mpsc::Sender<Event>,
            cancel: oneshot::Receiver<()>,
        ) -> futures_util::future::BoxFuture<'static, Result<StopReason>> {
            Box::pin(async {
                let _ = cancel.await;
                Ok(StopReason::Disconnected)
            })
        }
    }
    fn manager() -> ConnectionManager {
        ConnectionManager::new(
            Arc::new(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            ),
            Arc::new(FakeFactory),
        )
    }
    fn event(
        manager: &mut ConnectionManager,
        id: &str,
        generation: u64,
        event: Event,
    ) -> Option<Notification> {
        manager.apply(Update::Session {
            id: id.into(),
            generation,
            event,
        })
    }
    #[test]
    fn disconnect_rejects_late_ready_and_reconnect_ignores_previous_attempt() {
        let mut manager = manager();
        let (first, mut cancelled) = manager.begin("a".into()).unwrap();
        assert!(manager.begin("a".into()).is_err());
        assert!(manager.forget("a").is_err());
        manager.disconnect("a");
        assert!(cancelled.try_recv().is_ok());
        assert!(event(&mut manager, "a", first, Event::Ready).is_none());
        assert!(!manager.get("a").unwrap().is_ready());
        assert!(manager.begin("a".into()).is_err());
        event(
            &mut manager,
            "a",
            first,
            Event::Stopped(Ok(StopReason::Disconnected)),
        );
        let (second, _cancelled) = manager.begin("a".into()).unwrap();
        assert!(
            event(
                &mut manager,
                "a",
                first,
                Event::Stopped(Ok(StopReason::IdleTimeout))
            )
            .is_none()
        );
        assert!(event(&mut manager, "a", first, Event::Ready).is_none());
        assert!(
            matches!(event(&mut manager, "a", second, Event::Ready), Some(Notification::Ready(id)) if id == "a")
        );
        assert!(manager.get("a").unwrap().is_ready());
    }
    #[test]
    fn gateway_failure_cancels_all_sessions_and_blocks_new_connections() {
        let mut manager = manager();
        let (a, mut stop_a) = manager.begin("a".into()).unwrap();
        let (b, mut stop_b) = manager.begin("b".into()).unwrap();
        event(&mut manager, "a", a, Event::Ready);
        assert!(!manager.get("b").unwrap().is_ready());
        manager.apply(Update::Gateway {
            epoch: manager.gateway_epoch,
            error: "failed".into(),
        });
        assert!(stop_a.try_recv().is_ok());
        assert!(stop_b.try_recv().is_ok());
        assert!(manager.begin("c".into()).is_err());
        event(
            &mut manager,
            "a",
            a,
            Event::Stopped(Ok(StopReason::Disconnected)),
        );
        assert!(!manager.is_idle());
        event(
            &mut manager,
            "b",
            b,
            Event::Stopped(Err(anyhow::anyhow!("failed"))),
        );
        assert!(manager.is_idle());
    }
    #[test]
    fn shutdown_waits_for_workers_and_drop_cancels() {
        let mut manager = manager();
        let (stop, mut gateway_cancelled) = oneshot::channel();
        manager.gateway_stop = Some(stop);
        let (id, mut cancelled) = manager.begin("a".into()).unwrap();
        manager.shutdown();
        assert!(cancelled.try_recv().is_ok());
        assert!(matches!(
            gateway_cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(!manager.is_idle());
        assert!(manager.begin("b".into()).is_err());
        event(
            &mut manager,
            "a",
            id,
            Event::Stopped(Ok(StopReason::Disconnected)),
        );
        assert!(manager.is_idle());
        manager.poll();
        assert!(gateway_cancelled.try_recv().is_ok());
        let mut manager = self::manager();
        let (_, mut cancelled) = manager.begin("a".into()).unwrap();
        drop(manager);
        assert!(cancelled.try_recv().is_ok());
    }

    #[test]
    fn gateways_coexist_and_retry_ignores_old_failure_events() {
        let mut first = manager();
        let mut second = manager();
        first.start_gateway().unwrap();
        second.start_gateway().unwrap();
        let old_epoch = first.gateway_epoch;
        // Dropping cancellation sender terminates the old listener once its runtime polls.
        first.apply(Update::Gateway {
            epoch: old_epoch,
            error: "test failure".into(),
        });
        assert!(first.failed);
        first.start_gateway().unwrap();
        assert!(!first.failed);
        assert!(
            first
                .apply(Update::Gateway {
                    epoch: old_epoch,
                    error: "late".into()
                })
                .is_none()
        );
        assert!(!first.failed);
        assert!(first.gateway_stop.is_some() && second.gateway_stop.is_some());
    }

    #[test]
    fn ready_followed_by_stop_in_one_poll_does_not_open_a_dead_viewer() {
        let mut manager = manager();
        let (generation, _cancelled) = manager.begin("a".into()).unwrap();
        for event in [Event::Ready, Event::Stopped(Ok(StopReason::Disconnected))] {
            manager
                .sender
                .send(Update::Session {
                    id: "a".into(),
                    generation,
                    event,
                })
                .unwrap();
        }
        let notifications = manager.poll();
        assert!(
            !notifications
                .iter()
                .any(|n| matches!(n, Notification::Ready(_)))
        );
        assert!(manager.is_idle());
    }
}
