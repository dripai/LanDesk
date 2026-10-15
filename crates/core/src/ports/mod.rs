use crate::domain::*;
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
pub trait DesktopControl: Send + Sync {
    fn supports_files(&self) -> bool;
    fn secure_attention(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { anyhow::bail!("当前平台不支持 Ctrl+Alt+Delete") })
    }
    fn capabilities(&self) -> Capabilities;
    fn check_permissions(&self) -> Result<()>;
    fn begin_input(&self, display_id: u32) -> BoxFuture<'_, Result<()>>;
    fn input(&self, message: InputEvent) -> BoxFuture<'_, Result<()>>;
    fn paste_text(&self, text: String) -> BoxFuture<'_, Result<()>>;
    fn paste_image(&self, png: Vec<u8>) -> BoxFuture<'_, Result<()>>;
    fn copy_text(&self) -> BoxFuture<'_, Result<String>>;
    fn read_clipboard(&self) -> BoxFuture<'_, Result<String>>;
    fn end_session(&self) -> BoxFuture<'_, Result<()>>;
    fn restore(&self);
}

/// Owned on the capture worker, never sent across AppKit/OS thread boundaries.
pub trait FrameSource {
    fn frames(&self) -> &tokio::sync::watch::Receiver<Option<crate::domain::FrameEvent>>;
    fn frames_mut(
        &mut self,
    ) -> &mut tokio::sync::watch::Receiver<Option<crate::domain::FrameEvent>>;
    fn info(&self, requested_width: Option<u32>) -> CaptureInfo;
    fn display_id(&self) -> u32;
    fn native_width(&self) -> u32;
    fn geometry(&self) -> [f64; 4];
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()>;
}

pub trait CaptureBackend {
    /// None reconnects to the last successfully selected source.
    /// Errors leave the previous selection and its display lease intact.
    fn open(&mut self, target: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>>;
}

pub trait VirtualDisplay {
    fn id(&self) -> u32;
}
pub trait VirtualDisplayProvider {
    fn create(&self, spec: VirtualDisplaySpec) -> Result<std::rc::Rc<dyn VirtualDisplay>>;
}

pub trait FileSession: Send {
    fn message(&mut self, message: FileCommand) -> FileResponse;
    fn chunk(&mut self, bytes: &[u8]) -> FileResponse;
}
pub trait FileBackend: Send + Sync {
    fn open_session(self: Arc<Self>, cancelled: Arc<AtomicBool>) -> Box<dyn FileSession>;
}

pub trait SecretProtector: Send + Sync {
    fn key_for_write(&self, purpose: &str) -> Result<(crate::security::encryption::Key, Vec<u8>)>;
    fn key_for_read(
        &self,
        wrapped: &[u8],
        purpose: &str,
    ) -> Result<crate::security::encryption::Key>;
}
pub trait IdentityRepository {
    fn lock(&self) -> Result<Box<dyn std::any::Any>>;
    fn read(&self) -> Result<crate::domain::identity::ServerIdentity>;
    fn commit(
        &self,
        identity: &crate::domain::identity::ServerIdentity,
    ) -> crate::domain::identity::CommitResult;
}
pub mod client;
