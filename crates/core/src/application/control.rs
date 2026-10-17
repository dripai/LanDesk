use crate::{application::runtime::Shared, ports::DesktopControl};
use anyhow::{Result, ensure};
use std::sync::{Arc, atomic::Ordering};
/// A lease owns remote input. Restoration is queued before releasing ownership,
/// including cancellation, errors and task unwinding.
pub struct ControlSession {
    desktop: Arc<dyn DesktopControl>,
    shared: Arc<Shared>,
}
impl ControlSession {
    pub fn acquire(desktop: Arc<dyn DesktopControl>, shared: Arc<Shared>) -> Result<Self> {
        ensure!(
            shared
                .active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "已有一个连接，请先断开"
        );
        let session = Self { desktop, shared };
        Ok(session)
    }
    pub fn authorize(&self) -> Result<()> {
        self.desktop.check_permissions()
    }
    pub async fn finish(&self) -> Result<()> {
        self.desktop.end_session().await
    }
}
impl Drop for ControlSession {
    fn drop(&mut self) {
        self.desktop.restore();
        self.shared.active.store(false, Ordering::Release);
    }
}
